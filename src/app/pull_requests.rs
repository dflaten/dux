use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::model::{PrInfo, PrKey, PrRelationSource, PrStackEdge, PrState};
use crate::storage::{StoredPrLink, StoredPullRequest};

/// A row in the right-pane Pull Requests section.
#[derive(Clone, Debug)]
pub(crate) struct PrSectionRow {
    pub(crate) key: PrKey,
    /// Indentation depth for stacked PRs.
    pub(crate) depth: usize,
    /// PR belonging to the selected agent's current branch.
    pub(crate) is_current: bool,
    /// PR from another agent in the same multi-repo work group.
    pub(crate) is_related: bool,
    /// Project label for related PRs.
    pub(crate) project_name: Option<String>,
    /// Session that owns this PR, used for "jump to agent".
    pub(crate) session_id: Option<String>,
}

impl App {
    pub(crate) fn pr_info_from_stored(stored: &StoredPullRequest) -> Option<PrInfo> {
        let state = PrState::from_db_str(&stored.state)?;
        Some(PrInfo {
            number: stored.number,
            state,
            title: stored.title.clone(),
            host: stored.host.clone(),
            owner_repo: stored.owner_repo.clone(),
            url: if stored.url.trim().is_empty() {
                crate::storage::fallback_pr_url(&stored.host, &stored.owner_repo, stored.number)
            } else {
                stored.url.clone()
            },
            is_draft: stored.is_draft,
            head_ref: stored.head_ref.clone(),
            base_ref: stored.base_ref.clone(),
            head_repo: stored.head_repo.clone(),
        })
    }

    pub(crate) fn stored_from_pr(pr: &PrInfo) -> StoredPullRequest {
        StoredPullRequest {
            host: pr.host.trim().to_ascii_lowercase(),
            owner_repo: pr.owner_repo.trim().to_ascii_lowercase(),
            number: pr.number,
            state: pr.state.as_db_str().to_string(),
            is_draft: pr.is_draft,
            title: pr.title.clone(),
            url: pr.url.clone(),
            head_ref: pr.head_ref.clone(),
            base_ref: pr.base_ref.clone(),
            head_repo: pr.head_repo.clone(),
        }
    }

    /// Reload canonical PRs, links, and stack edges, then rebuild the derived
    /// per-session current-PR map.
    pub(crate) fn reload_pull_request_state(&mut self) {
        if !self.github_integration_enabled {
            self.pull_requests.clear();
            self.pr_links.clear();
            self.pr_stacks.clear();
            self.pr_statuses.clear();
            self.pr_index = 0;
            return;
        }

        let stored = self.session_store.load_pull_requests().unwrap_or_default();
        self.pull_requests = stored
            .iter()
            .filter_map(|item| {
                let info = Self::pr_info_from_stored(item)?;
                Some((info.key(), info))
            })
            .collect();

        let links = self.session_store.load_pr_links().unwrap_or_default();
        let mut by_session: HashMap<String, Vec<StoredPrLink>> = HashMap::new();
        for link in links {
            by_session
                .entry(link.session_id.clone())
                .or_default()
                .push(link);
        }
        self.pr_links = by_session;

        self.pr_stacks = self
            .session_store
            .load_pr_stacks()
            .unwrap_or_default()
            .into_iter()
            .map(|edge| {
                (
                    edge.child.clone(),
                    PrStackEdge {
                        child: edge.child,
                        parent: edge.parent,
                        source: edge.source,
                    },
                )
            })
            .collect();

        self.reconcile_pr_stacks();
        self.rebuild_current_pr_statuses();
        self.clamp_pr_index();
    }

    /// Recompute detected stack edges from PR base/head refs. Manual edges are
    /// never removed automatically.
    pub(crate) fn reconcile_pr_stacks(&mut self) {
        let mut best_by_head: HashMap<(String, String, String), PrKey> = HashMap::new();
        for pr in self.pull_requests.values() {
            if pr.head_ref.trim().is_empty() {
                continue;
            }
            let key = pr.key();
            let slot = (
                pr.host.trim().to_ascii_lowercase(),
                pr.owner_repo.trim().to_ascii_lowercase(),
                pr.head_ref.clone(),
            );
            match best_by_head.get(&slot) {
                Some(existing) => {
                    let replace = match (
                        self.pull_requests.get(existing),
                        self.pull_requests.get(&key),
                    ) {
                        (Some(a), Some(b)) => {
                            (b.state_rank(), b.number) > (a.state_rank(), a.number)
                        }
                        (None, Some(_)) => true,
                        _ => false,
                    };
                    if replace {
                        best_by_head.insert(slot, key);
                    }
                }
                None => {
                    best_by_head.insert(slot, key);
                }
            }
        }

        let mut changed = false;
        let children = self
            .pull_requests
            .values()
            .map(|pr| pr.key())
            .collect::<Vec<_>>();
        for child in children {
            let Some(child_info) = self.pull_requests.get(&child).cloned() else {
                continue;
            };
            let existing = self.pr_stacks.get(&child).cloned();
            if existing
                .as_ref()
                .is_some_and(|edge| edge.source == PrRelationSource::Manual)
            {
                continue;
            }
            if child_info.base_ref.trim().is_empty() {
                continue;
            }

            let slot = (
                child.host.trim().to_ascii_lowercase(),
                child.owner_repo.trim().to_ascii_lowercase(),
                child_info.base_ref.clone(),
            );
            let parent = best_by_head.get(&slot).cloned().filter(|key| *key != child);

            match parent {
                Some(parent) => {
                    if existing.as_ref().map(|edge| edge.parent.clone()) != Some(parent.clone()) {
                        if let Err(err) = self.session_store.upsert_pr_stack(
                            &child,
                            &parent,
                            PrRelationSource::Detected,
                        ) {
                            logger::warn(&format!(
                                "[gh-integration] failed to persist stack edge {} -> {}: {err:#}",
                                child.number, parent.number,
                            ));
                        }
                        self.pr_stacks.insert(
                            child.clone(),
                            PrStackEdge {
                                child,
                                parent,
                                source: PrRelationSource::Detected,
                            },
                        );
                        changed = true;
                    }
                }
                None => {
                    if let Some(edge) = existing {
                        // Keep an edge visible when a merged parent's branch was
                        // deleted and GitHub retargeted the child to the
                        // parent's base branch. This preserves stack history.
                        let keep_historical =
                            self.pull_requests
                                .get(&edge.parent)
                                .is_some_and(|parent_info| {
                                    parent_info.state == PrState::Merged
                                        && parent_info.base_ref == child_info.base_ref
                                });
                        if !keep_historical && edge.source == PrRelationSource::Detected {
                            let _ = self.session_store.delete_detected_pr_stack(&child);
                            self.pr_stacks.remove(&child);
                            changed = true;
                        }
                    }
                }
            }
        }
        if changed {
            logger::info("[gh-integration] reconciled pull request stack edges");
        }
    }

    pub(crate) fn rebuild_current_pr_statuses(&mut self) {
        self.pr_statuses.clear();
        let sessions = self
            .sessions
            .iter()
            .map(|session| (session.id.clone(), session.branch_name.clone()))
            .collect::<Vec<_>>();
        for (session_id, branch) in sessions {
            if let Some(info) = self.current_pr_for_session(&session_id, &branch) {
                self.pr_statuses.insert(session_id, info);
            }
        }
    }

    fn linked_prs_for_session(&self, session_id: &str) -> Vec<&PrInfo> {
        self.pr_links
            .get(session_id)
            .into_iter()
            .flatten()
            .filter_map(|link| self.pull_requests.get(&link.key))
            .collect()
    }

    pub(crate) fn current_pr_key_for_session(
        &self,
        session_id: &str,
        branch_name: &str,
    ) -> Option<PrKey> {
        let linked = self.linked_prs_for_session(session_id);
        let branch_match = linked
            .iter()
            .filter(|pr| pr.head_ref == branch_name)
            .max_by_key(|pr| (pr.state_rank(), pr.number))
            .map(|pr| pr.key());
        branch_match.or_else(|| {
            linked
                .iter()
                .max_by_key(|pr| (pr.state_rank(), pr.number))
                .map(|pr| pr.key())
        })
    }

    pub(crate) fn current_pr_for_session(
        &self,
        session_id: &str,
        branch_name: &str,
    ) -> Option<PrInfo> {
        let key = self.current_pr_key_for_session(session_id, branch_name)?;
        self.pull_requests.get(&key).cloned()
    }

    pub(crate) fn pr_section_rows_for_session(&self, session: &AgentSession) -> Vec<PrSectionRow> {
        if session.is_scratch() {
            return Vec::new();
        }
        let Some(current) = self.current_pr_key_for_session(&session.id, &session.branch_name)
        else {
            return self.related_pr_rows(session);
        };

        let component = self.stack_component(current.clone());
        let mut rows = Vec::new();
        let roots = self.stack_roots(&component);
        let mut visited = HashSet::new();
        for root in roots {
            self.push_stack_rows(&root, 0, &component, &current, &mut visited, &mut rows);
        }
        // Any component members left over (cycles or manual oddities) still
        // deserve a row so the section never silently drops a linked PR.
        for key in &component {
            if visited.insert(key.clone()) {
                rows.push(self.pr_row_for_key(key.clone(), 0, &current));
            }
        }

        let mut seen = rows
            .iter()
            .map(|row| row.key.clone())
            .collect::<HashSet<_>>();
        for row in self.related_pr_rows(session) {
            if seen.insert(row.key.clone()) {
                rows.push(row);
            }
        }
        rows
    }

    fn pr_row_for_key(&self, key: PrKey, depth: usize, current: &PrKey) -> PrSectionRow {
        let session_id = self
            .pr_links
            .iter()
            .find(|(_, links)| links.iter().any(|link| link.key == key))
            .map(|(session_id, _)| session_id.clone());
        PrSectionRow {
            is_current: &key == current,
            key,
            depth,
            is_related: false,
            project_name: None,
            session_id,
        }
    }

    fn push_stack_rows(
        &self,
        key: &PrKey,
        depth: usize,
        component: &HashSet<PrKey>,
        current: &PrKey,
        visited: &mut HashSet<PrKey>,
        rows: &mut Vec<PrSectionRow>,
    ) {
        if !visited.insert(key.clone()) {
            return;
        }
        rows.push(self.pr_row_for_key(key.clone(), depth, current));
        let mut children = self
            .pr_stacks
            .values()
            .filter(|edge| &edge.parent == key && component.contains(&edge.child))
            .map(|edge| edge.child.clone())
            .collect::<Vec<_>>();
        children.sort_by_key(|child| {
            self.pull_requests
                .get(child)
                .map(|pr| (pr.state_rank(), pr.number))
                .unwrap_or_default()
        });
        for child in children {
            self.push_stack_rows(&child, depth + 1, component, current, visited, rows);
        }
    }

    fn stack_roots(&self, component: &HashSet<PrKey>) -> Vec<PrKey> {
        let mut roots = component
            .iter()
            .filter(|key| {
                !self
                    .pr_stacks
                    .get(*key)
                    .is_some_and(|edge| component.contains(&edge.parent))
            })
            .cloned()
            .collect::<Vec<_>>();
        roots.sort_by_key(|key| {
            self.pull_requests
                .get(key)
                .map(|pr| (pr.state_rank(), pr.number))
                .unwrap_or_default()
        });
        roots
    }

    fn stack_component(&self, start: PrKey) -> HashSet<PrKey> {
        let mut component = HashSet::new();
        let mut queue = VecDeque::from([start]);
        while let Some(key) = queue.pop_front() {
            if !component.insert(key.clone()) {
                continue;
            }
            if let Some(parent) = self.pr_stacks.get(&key)
                && !component.contains(&parent.parent)
            {
                queue.push_back(parent.parent.clone());
            }
            for edge in self.pr_stacks.values() {
                if edge.parent == key && !component.contains(&edge.child) {
                    queue.push_back(edge.child.clone());
                }
            }
        }
        component
    }

    fn related_pr_rows(&self, session: &AgentSession) -> Vec<PrSectionRow> {
        if !self.config.ui.pr_section_show_related {
            return Vec::new();
        }
        let Some(group_id) = session.group_id.as_deref() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        for member in &self.sessions {
            if member.id == session.id || member.group_id.as_deref() != Some(group_id) {
                continue;
            }
            let Some(key) = self.current_pr_key_for_session(&member.id, &member.branch_name) else {
                continue;
            };
            if !seen.insert(key.clone()) {
                continue;
            }
            rows.push(PrSectionRow {
                key,
                depth: 0,
                is_current: false,
                is_related: true,
                project_name: Some(self.project_name_for_session(member)),
                session_id: Some(member.id.clone()),
            });
        }
        rows.sort_by(|a, b| {
            a.project_name
                .cmp(&b.project_name)
                .then_with(|| a.key.owner_repo.cmp(&b.key.owner_repo))
                .then_with(|| a.key.number.cmp(&b.key.number))
        });
        rows
    }

    pub(crate) fn selected_pr_section_rows(&self) -> Vec<PrSectionRow> {
        if !self.config.ui.pr_section || !self.github_integration_enabled {
            return Vec::new();
        }
        self.selected_session()
            .map(|session| self.pr_section_rows_for_session(session))
            .unwrap_or_default()
    }

    pub(crate) fn pr_section_visible(&self) -> bool {
        !self.selected_pr_section_rows().is_empty()
    }

    pub(crate) fn selected_pr_section_row(&self) -> Option<PrSectionRow> {
        self.selected_pr_section_rows().get(self.pr_index).cloned()
    }

    pub(crate) fn open_selected_pr_in_browser(&mut self) -> anyhow::Result<()> {
        let Some(row) = self.selected_pr_section_row() else {
            self.set_warning("Select a pull request first.");
            return Ok(());
        };
        let Some(pr) = self.pull_requests.get(&row.key).cloned() else {
            self.set_error("The selected pull request is no longer known.");
            return Ok(());
        };
        crate::browser::open_url(&pr.url)?;
        self.set_info(format!(
            "Opened PR {}#{} in the default browser.",
            pr.owner_repo, pr.number
        ));
        Ok(())
    }

    pub(crate) fn copy_selected_pr_url(&mut self) -> anyhow::Result<()> {
        let Some(row) = self.selected_pr_section_row() else {
            self.set_warning("Select a pull request first.");
            return Ok(());
        };
        let Some(pr) = self.pull_requests.get(&row.key) else {
            self.set_error("The selected pull request is no longer known.");
            return Ok(());
        };
        self.clipboard.copy_text(
            &pr.url,
            &format!("Copied PR {}#{}.", pr.owner_repo, pr.number),
            &self.worker_tx,
        )?;
        Ok(())
    }

    pub(crate) fn go_to_selected_pr_agent(&mut self) -> anyhow::Result<()> {
        let Some(row) = self.selected_pr_section_row() else {
            self.set_warning("Select a pull request first.");
            return Ok(());
        };
        let Some(session_id) = row.session_id else {
            self.set_info("This pull request is not owned by an active agent session.");
            return Ok(());
        };
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            self.set_info("The agent for this pull request is no longer active.");
            return Ok(());
        };
        let label = session
            .title
            .clone()
            .unwrap_or_else(|| session.branch_name.clone());
        self.select_left_target(&super::LeftSelectionTarget::Session(session_id));
        self.focus = FocusPane::Left;
        self.left_section = crate::app::LeftSection::Projects;
        self.set_info(format!("Switched to related agent \"{label}\"."));
        Ok(())
    }

    pub(crate) fn clamp_pr_index(&mut self) {
        let len = self.selected_pr_section_rows().len();
        if len == 0 {
            self.pr_index = 0;
        } else if self.pr_index >= len {
            self.pr_index = len - 1;
        }
    }

    /// Focus the current worktree PR by default when the selection changes.
    pub(crate) fn select_current_pr_row(&mut self) {
        let rows = self.selected_pr_section_rows();
        self.pr_index = rows.iter().position(|row| row.is_current).unwrap_or(0);
        if self.pr_index >= rows.len() {
            self.pr_index = 0;
        }
    }

    pub(crate) fn prs_for_project(&self, project_id: &str) -> Vec<PrInfo> {
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for session in self.sessions.iter().filter(|s| s.project_id == project_id) {
            for pr in self.linked_prs_for_session(&session.id) {
                if seen.insert(pr.key()) {
                    result.push(pr.clone());
                }
            }
        }
        result.sort_by_key(|pr| (pr.state_rank(), pr.number));
        result
    }

    pub(crate) fn set_manual_pr_stack_parent(
        &mut self,
        child: &PrKey,
        parent: &PrKey,
    ) -> anyhow::Result<()> {
        self.session_store
            .upsert_pr_stack(child, parent, PrRelationSource::Manual)?;
        self.pr_stacks.insert(
            child.clone(),
            PrStackEdge {
                child: child.clone(),
                parent: parent.clone(),
                source: PrRelationSource::Manual,
            },
        );
        Ok(())
    }

    pub(crate) fn clear_manual_pr_stack_parent(&mut self, child: &PrKey) -> anyhow::Result<()> {
        self.session_store.delete_pr_stack(child)?;
        self.pr_stacks.remove(child);
        self.reconcile_pr_stacks();
        Ok(())
    }

    /// Resolve parent PRs that are not attached to any dux session yet. This is
    /// what makes stacks created by other tools (or on branches without an
    /// agent) show up automatically.
    pub(crate) fn spawn_missing_base_pr_lookups(&mut self) {
        if !self.github_integration_enabled
            || !matches!(self.gh_status, crate::model::GhStatus::Available)
        {
            return;
        }
        let mut known_heads = HashSet::new();
        for pr in self.pull_requests.values() {
            if !pr.head_ref.trim().is_empty() {
                known_heads.insert((
                    pr.host.trim().to_ascii_lowercase(),
                    pr.owner_repo.trim().to_ascii_lowercase(),
                    pr.head_ref.clone(),
                ));
            }
        }
        let mut default_bases: HashSet<String> = HashSet::from([
            "main".to_string(),
            "master".to_string(),
            "develop".to_string(),
            "trunk".to_string(),
        ]);
        for project in &self.projects {
            if let Some(branch) = &project.leading_branch {
                default_bases.insert(branch.clone());
            }
            if !project.current_branch.trim().is_empty() {
                default_bases.insert(project.current_branch.clone());
            }
        }

        let now = Instant::now();
        let mut lookups = Vec::new();
        for pr in self.pull_requests.values() {
            let base = pr.base_ref.trim();
            if base.is_empty() || default_bases.contains(base) {
                continue;
            }
            let slot = (
                pr.host.trim().to_ascii_lowercase(),
                pr.owner_repo.trim().to_ascii_lowercase(),
                base.to_string(),
            );
            if known_heads.contains(&slot)
                || self.pr_base_lookups.contains(&slot)
                || self
                    .pr_base_last_checked
                    .get(&slot)
                    .is_some_and(|last| now.duration_since(*last) < Duration::from_secs(600))
            {
                continue;
            }
            lookups.push(slot);
            if lookups.len() >= 6 {
                break;
            }
        }

        for (host, owner_repo, base) in lookups {
            self.pr_base_lookups
                .insert((host.clone(), owner_repo.clone(), base.clone()));
            self.pr_base_last_checked
                .insert((host.clone(), owner_repo.clone(), base.clone()), now);
            let tx = self.worker_tx.clone();
            thread::spawn(move || {
                super::workers::run_pr_base_lookup_job(host, owner_repo, base, tx);
            });
        }
    }

    pub(crate) fn default_pr_base_for_session(&self, session: &AgentSession) -> String {
        let source_is_stacked = self
            .pull_requests
            .values()
            .any(|pr| pr.head_ref == session.source_branch && pr.base_ref != session.source_branch);
        if source_is_stacked && !session.source_branch.trim().is_empty() {
            return session.source_branch.clone();
        }
        self.projects
            .iter()
            .find(|project| project.id == session.project_id)
            .and_then(|project| project.leading_branch.clone())
            .unwrap_or_else(|| session.source_branch.clone())
    }

    pub(crate) fn create_pull_request_for_selected_session(&mut self) -> anyhow::Result<()> {
        if !self.github_integration_enabled
            || !matches!(self.gh_status, crate::model::GhStatus::Available)
        {
            self.set_error(
                "Pull request creation requires GitHub integration and an authenticated gh CLI.",
            );
            return Ok(());
        }
        let Some(session) = self.selected_session().cloned() else {
            self.set_error("Select an agent session first to create a pull request.");
            return Ok(());
        };
        if session.is_scratch() {
            self.set_error("Scratch agents are not attached to a GitHub repository.");
            return Ok(());
        }
        if session.branch_name.trim().is_empty() {
            self.set_error("The selected agent has no branch to open a pull request from.");
            return Ok(());
        }
        let Some(project) = self
            .projects
            .iter()
            .find(|project| project.id == session.project_id)
            .cloned()
        else {
            self.set_error("The selected agent's project is no longer available.");
            return Ok(());
        };
        let base = self.default_pr_base_for_session(&session);
        self.set_busy(format!(
            "Creating a pull request for \"{}\" against {base}...",
            session.branch_name
        ));
        let worker_tx = self.worker_tx.clone();
        thread::spawn(move || {
            super::workers::run_create_pull_request_job(session, project, base, worker_tx);
        });
        Ok(())
    }

    /// Toggle-independent activation path used at startup, when the integration
    /// is toggled on, and after a config reload.
    pub(crate) fn activate_github_integration(&mut self) {
        if !self.github_integration_enabled {
            self.pr_sync_enabled.store(false, Ordering::Relaxed);
            self.pr_statuses.clear();
            return;
        }
        if !matches!(self.gh_status, crate::model::GhStatus::Available) {
            self.spawn_gh_status_check();
            return;
        }
        self.update_pr_sync_sessions();
        self.ensure_pr_polling();
        self.spawn_initial_pr_refresh();
        if self.refs_watcher.is_none() {
            self.spawn_refs_watcher();
        }
        self.spawn_missing_base_pr_lookups();
    }
}
