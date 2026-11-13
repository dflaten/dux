//! Search support for the projects and agents listed in the left sidebar.
//!
//! The sidebar renders a tree of projects and their agent sessions. While a
//! search is active the tree is narrowed to the rows that match the query,
//! so a project or agent can be reached without scrolling through the whole
//! list.
//!
//! Matching is deliberately forgiving: a term matches a candidate when it
//! appears literally inside it, or — for longer terms — when its characters
//! appear in order. Every whitespace separated term of the query has to
//! match *some* candidate of a row, so `dux claude` finds the Claude agents
//! of the `dux` project without also listing every other agent of `dux`.

use super::*;

/// Minimum number of characters before a query starts narrowing the list.
///
/// Fuzzy matching is very permissive, so a single character matches nearly
/// every row and the filter looks broken. Two characters keep short queries
/// meaningful.
pub(crate) const MIN_LEFT_SEARCH_QUERY_CHARS: usize = 2;

/// Minimum length of a query term before subsequence ("fuzzy") matching is
/// allowed. Shorter terms have to match literally.
const MIN_FUZZY_TERM_CHARS: usize = 3;

/// Splits a query into its whitespace separated terms.
fn query_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|term| term.to_lowercase())
        .collect()
}

/// The fields of a row that participate in matching, split by how strictly
/// they are matched.
struct SearchFields {
    /// Names, titles, branches, providers, and statuses. These are short,
    /// human-visible values, so a subsequence match is a useful convenience.
    fuzzy: Vec<String>,
    /// Filesystem paths. These only match literally.
    ///
    /// Subsequence matching a path is far too permissive: every worktree path
    /// contains `worktrees`, which supplies `t`, `r`, and `e`, so a query like
    /// `trea` matches almost every agent as soon as any later character is an
    /// `a` (a project name with an `a`, the `agent` segment, ...). The visible
    /// row then shows nothing resembling the query.
    literal: Vec<String>,
}

/// Whether a lowercased term matches a lowercased candidate.
fn term_matches(term: &str, candidate: &str) -> bool {
    if candidate.contains(term) {
        return true;
    }
    if term.chars().count() >= MIN_FUZZY_TERM_CHARS {
        return fuzzy_subsequence_match(candidate, term);
    }
    false
}

/// Whether every term matches at least one of the fields.
fn fields_match(fields: &SearchFields, terms: &[String]) -> bool {
    terms.iter().all(|term| {
        fields
            .fuzzy
            .iter()
            .any(|candidate| term_matches(term, candidate))
            || fields
                .literal
                .iter()
                .any(|candidate| candidate.contains(term))
    })
}

/// The fields of a project that participate in matching.
fn project_fields(project: &Project) -> SearchFields {
    SearchFields {
        fuzzy: vec![project.name.to_lowercase()],
        literal: vec![project.path.to_lowercase()],
    }
}

/// The fields of an agent session that participate in matching.
fn session_fields(
    session: &AgentSession,
    project: &Project,
    related_group_ids: &HashSet<&str>,
) -> SearchFields {
    let label = session
        .title
        .as_deref()
        .unwrap_or(session.branch_name.as_str());
    let mut fuzzy = vec![
        label.to_lowercase(),
        session.branch_name.to_lowercase(),
        session.provider.as_str().to_lowercase(),
        session.status.as_str().to_lowercase(),
        project.name.to_lowercase(),
    ];
    if session.is_scratch() {
        fuzzy.push("scratch".to_string());
    }
    if session_in_related_section(session, related_group_ids) {
        fuzzy.push("related".to_string());
        fuzzy.push("multi-repo".to_string());
    }
    let mut literal = vec![
        session.worktree_path.to_lowercase(),
        project.path.to_lowercase(),
    ];
    if let Some(project_path) = session.project_path.as_deref() {
        literal.push(project_path.to_lowercase());
    }
    SearchFields { fuzzy, literal }
}

/// Whether the whole query matches the project.
fn project_matches(project: &Project, terms: &[String]) -> bool {
    fields_match(&project_fields(project), terms)
}

/// Whether the whole query matches the session.
fn session_matches(
    session: &AgentSession,
    project: &Project,
    terms: &[String],
    related_group_ids: &HashSet<&str>,
) -> bool {
    fields_match(&session_fields(session, project, related_group_ids), terms)
}

/// Builds the left pane item list, narrowed to the rows matching `query`.
///
/// An empty query produces the regular tree. A non-empty query produces the
/// matching subset: a project whose own name matches keeps all of its
/// agents, while a project that only has matching agents is shown with just
/// those agents. Collapsed projects are always expanded while filtering so
/// matching agents stay reachable, and the empty-project grouping is skipped
/// because the result set is already narrowed.
pub(crate) fn build_filtered_left_items(
    projects: &[Project],
    sessions: &[AgentSession],
    groups: &[AgentGroup],
    collapsed_projects: &HashSet<String>,
    empty_project_separator_min_projects: u16,
    query: &str,
) -> Vec<LeftItem> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return build_left_items(
            projects,
            sessions,
            groups,
            collapsed_projects,
            empty_project_separator_min_projects,
        );
    }

    // Group the sessions by project once so every project does not have to
    // rescan the whole session list.
    let mut sessions_by_project: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, session) in sessions.iter().enumerate() {
        sessions_by_project
            .entry(session.project_id.as_str())
            .or_default()
            .push(index);
    }

    let mut project_hits: HashSet<&str> = HashSet::new();
    for project in projects {
        if !project.is_scratch() && project_matches(project, &terms) {
            project_hits.insert(project.id.as_str());
        }
    }

    let related_groups = crate::app::related_group_indices(groups, sessions);
    let related_group_ids = crate::app::related_section_group_ids(groups, &related_groups);

    let mut items = Vec::new();
    for (project_index, project) in projects.iter().enumerate() {
        if project.is_scratch() {
            continue;
        }
        let project_hit = project_hits.contains(project.id.as_str());
        // A project whose checkout is missing hides its agents in the regular
        // tree; keep that rule while filtering so search does not surface
        // rows that cannot be acted on.
        let matched = if project.path_missing {
            Vec::new()
        } else {
            matching_sessions(
                project,
                sessions,
                &related_group_ids,
                sessions_by_project
                    .get(project.id.as_str())
                    .map(Vec::as_slice),
                &terms,
                project_hit,
            )
        };
        if !project_hit && matched.is_empty() {
            continue;
        }
        items.push(LeftItem::Project(project_index));
        items.extend(matched.into_iter().map(LeftItem::Session));
    }

    // Multi-repo work is matched in its own section so a project header never
    // appears with a hidden related member as its only child.
    let mut related_items = Vec::new();
    for group_index in related_groups {
        let group = &groups[group_index];
        let group_label = crate::app::related_group_label_for(sessions, group);
        let group_hit = terms
            .iter()
            .all(|term| term_matches(term, &group_label.to_lowercase()));
        let origin_index = crate::app::related_origin_session_index(sessions, group);
        let origin_hit = origin_index.is_some_and(|index| {
            projects
                .iter()
                .find(|project| project.id == sessions[index].project_id)
                .is_some_and(|project| {
                    session_matches(&sessions[index], project, &terms, &related_group_ids)
                        || project_hits.contains(project.id.as_str())
                })
        });
        let show_all_members = group_hit || origin_hit;
        let mut members = Vec::new();
        for (index, session) in sessions.iter().enumerate() {
            if session.group_id.as_deref() != Some(group.id.as_str()) || Some(index) == origin_index
            {
                continue;
            }
            let Some(project) = projects
                .iter()
                .find(|project| project.id == session.project_id)
            else {
                continue;
            };
            let member_hit = session_matches(session, project, &terms, &related_group_ids)
                || project_hits.contains(project.id.as_str());
            if show_all_members || member_hit {
                members.push(index);
            }
        }
        if show_all_members || !members.is_empty() {
            match origin_index {
                Some(index) => related_items.push(LeftItem::Session(index)),
                None => related_items.push(LeftItem::RelatedGroupLabel(group_index)),
            }
            if show_all_members {
                related_items.extend(
                    sessions
                        .iter()
                        .enumerate()
                        .filter(|(index, session)| {
                            session.group_id.as_deref() == Some(group.id.as_str())
                                && Some(*index) != origin_index
                        })
                        .map(|(index, _)| LeftItem::Session(index)),
                );
            } else {
                related_items.extend(members.into_iter().map(LeftItem::Session));
            }
        }
    }
    if !related_items.is_empty() {
        if !items.is_empty() {
            items.push(LeftItem::RelatedSeparator);
        }
        items.extend(related_items);
    }

    // Scratch always lives below the other groups.
    if let Some((_, project)) = projects.iter().enumerate().find(|(_, p)| p.is_scratch()) {
        let project_hit = project_matches(project, &terms);
        let matched = matching_sessions(
            project,
            sessions,
            &related_group_ids,
            sessions_by_project
                .get(SCRATCH_PROJECT_ID)
                .map(Vec::as_slice),
            &terms,
            project_hit,
        );
        if project_hit || !matched.is_empty() {
            if !items.is_empty() {
                items.push(LeftItem::ScratchSeparator);
            }
            items.push(LeftItem::ScratchHeader);
            items.extend(matched.into_iter().map(LeftItem::Session));
        }
    }

    items
}

/// The indices of the sessions of a project that survive the filter.
fn matching_sessions(
    project: &Project,
    sessions: &[AgentSession],
    related_group_ids: &HashSet<&str>,
    session_indices: Option<&[usize]>,
    terms: &[String],
    project_hit: bool,
) -> Vec<usize> {
    let Some(indices) = session_indices else {
        return Vec::new();
    };
    let mut matched = Vec::new();
    for &index in indices {
        let session = &sessions[index];
        if crate::app::session_in_related_section(session, related_group_ids) {
            continue;
        }
        if project_hit || session_matches(session, project, terms, related_group_ids) {
            matched.push(index);
        }
    }
    matched
}

impl App {
    /// The query currently narrowing the sidebar, or an empty string when no
    /// filter is in effect.
    pub(crate) fn left_search_query(&self) -> &str {
        if self.left_search_is_effective() {
            self.left_search.text.as_str()
        } else {
            ""
        }
    }

    /// Whether the current query is long enough to narrow the list.
    pub(crate) fn left_search_is_effective(&self) -> bool {
        self.left_search.text.trim().chars().count() >= MIN_LEFT_SEARCH_QUERY_CHARS
    }

    /// Whether the sidebar is currently narrowed to a search result set.
    pub(crate) fn left_search_is_filtering(&self) -> bool {
        self.left_search_is_effective()
    }

    /// Whether a sidebar search query is present, applied or not.
    pub(crate) fn has_left_search(&self) -> bool {
        !self.left_search.is_empty()
    }

    /// Opens the sidebar search field.
    pub(crate) fn open_left_search(&mut self) {
        self.focus = FocusPane::Left;
        self.left_section = LeftSection::Projects;
        self.left_collapsed = false;
        self.input_target = InputTarget::None;
        self.left_search.move_end();
        if !self.left_search_active {
            self.left_search_origin = self.selected_left_target();
        }
        self.left_search_active = true;
        self.set_info(
            "Project search: type to filter projects and agents. Enter applies the filter, Esc clears it.",
        );
    }

    /// Clears the sidebar search state without rebuilding the list.
    ///
    /// Callers that are about to rebuild the list themselves can use this to
    /// drop the filter before the rebuild.
    pub(crate) fn reset_left_search_state(&mut self) {
        self.left_search.clear();
        self.left_search_active = false;
        self.left_search_origin = None;
    }

    /// Leaves the search field.
    ///
    /// Applying keeps the query as a filter on the sidebar; clearing drops
    /// the query and restores the row that was selected when the search
    /// opened.
    pub(crate) fn close_left_search(&mut self, apply: bool) {
        let had_query = self.has_left_search();
        let previous = self.selected_left_target();
        // Cancelling an active search returns to where the search started.
        // Clearing an already-applied filter keeps the row that is selected
        // in the filtered list instead of jumping to whatever now sits at the
        // same index in the full tree.
        let target = if apply || !self.left_search_active {
            previous.clone()
        } else {
            self.left_search_origin.clone().or(previous.clone())
        };
        if apply {
            self.left_search_active = false;
            self.left_search_origin = None;
        } else {
            self.reset_left_search_state();
        }
        self.rebuild_left_items();
        if let Some(target) = target {
            self.select_left_target(&target);
        }
        self.refresh_after_left_selection_change(&previous);
        if apply {
            self.set_info(
                "Project filter applied. Press Esc to clear it and show every project and agent again.",
            );
        } else if had_query {
            self.set_info("Project search cleared. The full project list is shown again.");
        }
    }

    /// Drops the sidebar filter entirely.
    pub(crate) fn clear_left_search(&mut self) {
        if !self.has_left_search() && !self.left_search_active {
            return;
        }
        let previous = self.selected_left_target();
        self.reset_left_search_state();
        self.rebuild_left_items();
        if let Some(target) = previous.clone() {
            self.select_left_target(&target);
        }
        self.refresh_after_left_selection_change(&previous);
    }

    /// Re-applies the query after the search text changed.
    ///
    /// The selected row is kept when it still matches, otherwise the first
    /// matching row is selected.
    pub(crate) fn update_left_search(&mut self) {
        let previous = self.selected_left_target();
        self.rebuild_left_items();
        if let Some(target) = previous.clone()
            && self.select_left_target(&target)
        {
            // The selected row survived the new query.
        } else {
            self.select_first_left_item();
        }
        self.refresh_after_left_selection_change(&previous);
    }

    /// Ends search editing mode when the left pane is no longer focused.
    ///
    /// The filter itself stays applied so the user comes back to the same
    /// narrowed list. A query that is too short to filter anything is dropped
    /// instead of lingering as a no-op filter.
    pub(crate) fn deactivate_left_search_if_unfocused(&mut self) {
        if !self.left_search_active {
            return;
        }
        if self.focus == FocusPane::Left && self.left_section == LeftSection::Projects {
            return;
        }
        if self.left_search_is_effective() {
            self.left_search_active = false;
            self.left_search_origin = None;
        } else {
            self.reset_left_search_state();
        }
    }

    /// The identity of the row at `index` in the left pane.
    pub(crate) fn left_target_at(&self, index: usize) -> Option<LeftSelectionTarget> {
        match self.left_items().get(index)? {
            LeftItem::Project(project_index) => {
                let project = self.projects.get(*project_index)?;
                Some(LeftSelectionTarget::Project(project.id.clone()))
            }
            LeftItem::Session(session_index) => {
                let session = self.sessions.get(*session_index)?;
                Some(LeftSelectionTarget::Session(session.id.clone()))
            }
            LeftItem::ScratchHeader => Some(LeftSelectionTarget::Scratch),
            _ => None,
        }
    }

    /// The identity of the currently selected left pane row.
    pub(crate) fn selected_left_target(&self) -> Option<LeftSelectionTarget> {
        self.left_target_at(self.selected_left)
    }

    /// The identity of the selectable row above the current selection.
    pub(crate) fn previous_left_target(&self) -> Option<LeftSelectionTarget> {
        self.previous_selectable_left_item_before(self.selected_left)
            .and_then(|index| self.left_target_at(index))
    }

    /// Whether the current selection belongs to the given project.
    pub(crate) fn selection_belongs_to_project(&self, project_id: &str) -> bool {
        match self.selected_left_target() {
            Some(LeftSelectionTarget::Project(id)) => id == project_id,
            Some(LeftSelectionTarget::Session(id)) => self
                .sessions
                .iter()
                .find(|session| session.id == id)
                .is_some_and(|session| session.project_id == project_id),
            Some(LeftSelectionTarget::Scratch) => project_id == SCRATCH_PROJECT_ID,
            None => false,
        }
    }

    /// Restores the left-pane selection after a project was removed.
    ///
    /// When the removed project was selected, the cursor moves to the row
    /// above it (or a valid neighbour when there is none). Otherwise the
    /// previously selected row is kept by identity.
    pub(crate) fn restore_selection_after_removal(
        &mut self,
        selected_before: Option<LeftSelectionTarget>,
        removed_was_selected: bool,
        previous_target: Option<LeftSelectionTarget>,
    ) {
        if removed_was_selected {
            if let Some(target) = previous_target
                && self.select_left_target(&target)
            {
                return;
            }
            self.selected_left = self.selected_left.saturating_sub(1);
            self.ensure_selectable_left_item();
        } else if let Some(target) = selected_before {
            self.select_left_target(&target);
        }
    }

    /// Mirrors the bookkeeping that normal left-pane navigation performs when
    /// the selected row changes, so the center diff and the files pane do not
    /// keep showing the previous row's work after a search moved the cursor.
    pub(crate) fn refresh_after_left_selection_change(
        &mut self,
        previous: &Option<LeftSelectionTarget>,
    ) {
        if &self.selected_left_target() == previous {
            return;
        }
        self.close_diff_view();
        self.reload_changed_files_quiet();
        self.update_missing_project_warning();
    }

    /// Selects the row matching the given identity, if it is still listed.
    pub(crate) fn select_left_target(&mut self, target: &LeftSelectionTarget) -> bool {
        let position = self.left_items().iter().position(|item| match item {
            LeftItem::Project(index) => {
                let Some(project) = self.projects.get(*index) else {
                    return false;
                };
                match target {
                    LeftSelectionTarget::Project(id) => &project.id == id,
                    LeftSelectionTarget::Scratch => project.is_scratch(),
                    LeftSelectionTarget::Session(_) => false,
                }
            }
            LeftItem::ScratchHeader => matches!(target, LeftSelectionTarget::Scratch),
            LeftItem::Session(index) => {
                let Some(session) = self.sessions.get(*index) else {
                    return false;
                };
                match target {
                    LeftSelectionTarget::Session(id) => &session.id == id,
                    _ => false,
                }
            }
            _ => false,
        });
        match position {
            Some(index) => {
                self.selected_left = index;
                true
            }
            None => false,
        }
    }

    /// Selects the first selectable row of the left pane.
    pub(crate) fn select_first_left_item(&mut self) {
        self.selected_left = self
            .left_items()
            .iter()
            .position(|item| item.is_selectable())
            .unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use super::build_filtered_left_items;
    use crate::app::LeftItem;
    use crate::model::{
        AgentGroup, AgentSession, Project, ProjectBranchStatus, ProviderKind, SCRATCH_PROJECT_ID,
        SessionStatus,
    };
    use std::collections::HashSet;

    fn project(id: &str, name: &str) -> Project {
        Project {
            id: id.to_string(),
            name: name.to_string(),
            path: format!("/tmp/{id}"),
            explicit_default_provider: None,
            default_provider: ProviderKind::from_str("codex"),
            leading_branch: Some("main".to_string()),
            auto_reopen_agents: None,
            startup_command: None,
            env: Default::default(),
            current_branch: "main".to_string(),
            branch_status: ProjectBranchStatus::Unknown,
            path_missing: false,
        }
    }

    fn session(
        id: &str,
        project_id: &str,
        title: &str,
        branch: &str,
        provider: &str,
    ) -> AgentSession {
        let now = chrono::Utc::now();
        AgentSession {
            id: id.to_string(),
            project_id: project_id.to_string(),
            project_path: Some(format!("/tmp/{project_id}")),
            provider: ProviderKind::from_str(provider),
            source_branch: "main".to_string(),
            branch_name: branch.to_string(),
            worktree_path: format!("/tmp/{project_id}/worktrees/{id}"),
            title: Some(title.to_string()),
            started_providers: Vec::new(),
            provider_session_ids: Default::default(),
            desired_running: false,
            auto_reopen_enabled: true,
            status: SessionStatus::Detached,
            group_id: None,
            parent_session_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn projects() -> Vec<Project> {
        vec![project("p1", "alpha"), project("p2", "beta")]
    }

    fn sessions() -> Vec<AgentSession> {
        vec![
            session("s1", "p1", "first", "first", "codex"),
            session("s2", "p1", "second", "second", "claude"),
            session("s3", "p2", "other", "other", "codex"),
        ]
    }

    fn filtered(projects: &[Project], sessions: &[AgentSession], query: &str) -> Vec<LeftItem> {
        build_filtered_left_items(projects, sessions, &[], &HashSet::new(), 5, query)
    }

    #[test]
    fn empty_query_returns_the_regular_tree() {
        let items = filtered(&projects(), &sessions(), "");
        assert_eq!(
            items,
            vec![
                LeftItem::Project(0),
                LeftItem::Session(0),
                LeftItem::Session(1),
                LeftItem::Project(1),
                LeftItem::Session(2),
            ]
        );
    }

    #[test]
    fn project_match_keeps_all_of_its_agents() {
        let items = filtered(&projects(), &sessions(), "alpha");
        assert_eq!(
            items,
            vec![
                LeftItem::Project(0),
                LeftItem::Session(0),
                LeftItem::Session(1)
            ]
        );
    }

    #[test]
    fn agent_match_shows_the_header_and_only_that_agent() {
        let items = filtered(&projects(), &sessions(), "second");
        assert_eq!(items, vec![LeftItem::Project(0), LeftItem::Session(1)]);
    }

    #[test]
    fn rows_that_do_not_match_are_hidden() {
        let items = filtered(&projects(), &sessions(), "nothing-matches-this");
        assert!(items.is_empty());
    }

    #[test]
    fn every_term_has_to_match() {
        // "alpha" matches the project, "claude" matches one of its agents.
        let items = filtered(&projects(), &sessions(), "alpha claude");
        assert_eq!(items, vec![LeftItem::Project(0), LeftItem::Session(1)]);
    }

    #[test]
    fn a_term_matching_nothing_hides_the_row() {
        // No agent of "alpha" uses "codex" other than the first one, and the
        // project itself does not match "codex".
        let items = filtered(&projects(), &sessions(), "alpha codex");
        assert_eq!(items, vec![LeftItem::Project(0), LeftItem::Session(0)]);
    }

    #[test]
    fn provider_and_status_are_searchable() {
        let items = filtered(&projects(), &sessions(), "claude");
        assert_eq!(items, vec![LeftItem::Project(0), LeftItem::Session(1)]);

        let items = filtered(&projects(), &sessions(), "detached");
        assert_eq!(items.len(), 5);
    }

    #[test]
    fn short_terms_have_to_match_literally() {
        // "cx" is a subsequence of "codex" but not a literal substring.
        assert!(filtered(&projects(), &sessions(), "cx").is_empty());
        assert_eq!(filtered(&projects(), &sessions(), "cod").len(), 5);
    }

    #[test]
    fn long_terms_match_as_a_subsequence() {
        // "claud" is a subsequence of "claude".
        let items = filtered(&projects(), &sessions(), "claud");
        assert_eq!(items, vec![LeftItem::Project(0), LeftItem::Session(1)]);
    }

    #[test]
    fn scratch_agents_are_searchable() {
        let mut projects = projects();
        projects.push(project(SCRATCH_PROJECT_ID, "Scratch"));
        let mut sessions = sessions();
        sessions.push(session(
            "scratch-1",
            SCRATCH_PROJECT_ID,
            "adhoc",
            "adhoc",
            "codex",
        ));

        let items = filtered(&projects, &sessions, "adhoc");
        assert_eq!(items, vec![LeftItem::ScratchHeader, LeftItem::Session(3)]);
    }

    #[test]
    fn collapsed_projects_are_expanded_while_filtering() {
        let mut collapsed = HashSet::new();
        collapsed.insert("p1".to_string());

        let items =
            build_filtered_left_items(&projects(), &sessions(), &[], &collapsed, 5, "second");
        assert_eq!(items, vec![LeftItem::Project(0), LeftItem::Session(1)]);

        // Without a filter the collapsed project keeps its collapsed shape.
        let items = build_filtered_left_items(&projects(), &sessions(), &[], &collapsed, 5, "");
        assert_eq!(
            items,
            vec![
                LeftItem::Project(0),
                LeftItem::Project(1),
                LeftItem::Session(2)
            ]
        );
    }

    #[test]
    fn the_empty_project_grouping_is_skipped_while_filtering() {
        let projects = vec![project("p1", "alpha"), project("p2", "beta")];
        let sessions = vec![session("s1", "p1", "first", "first", "codex")];

        // The separator threshold is reached, but a filtered list has no
        // empty-project section.
        let items =
            build_filtered_left_items(&projects, &sessions, &[], &HashSet::new(), 1, "first");
        assert_eq!(items, vec![LeftItem::Project(0), LeftItem::Session(0)]);
    }

    #[test]
    fn path_missing_projects_keep_their_agents_hidden() {
        let mut projects = projects();
        projects[0].path_missing = true;

        // The agent title matches, but the project checkout is missing, so
        // the agent row stays hidden just like in the regular tree.
        assert!(filtered(&projects, &sessions(), "first").is_empty());

        // The project header is still shown when the project itself matches.
        assert_eq!(
            filtered(&projects, &sessions(), "alpha"),
            vec![LeftItem::Project(0)]
        );
    }

    #[test]
    fn paths_only_match_literally() {
        let mut sessions = sessions();
        // "worktrees" supplies t-r-e and "agent" supplies the final a, so a
        // subsequence match would treat this as a hit for "trea" even though
        // nothing visible contains the query.
        sessions[0].worktree_path =
            "/home/david/.config/dux/worktrees/alpha/agent/first".to_string();
        assert!(
            filtered(&projects(), &sessions, "trea").is_empty(),
            "path fields must not match as a subsequence"
        );

        // Literal path fragments still match.
        sessions[0].worktree_path = "/home/david/projects/treatment/agent".to_string();
        assert_eq!(
            filtered(&projects(), &sessions, "treatment"),
            vec![LeftItem::Project(0), LeftItem::Session(0)]
        );
    }

    #[test]
    fn multi_repo_work_moves_the_origin_and_members_into_their_own_section() {
        let projects = projects();
        let mut sessions = sessions();
        let mut group = AgentGroup::new("first".to_string(), Some("s1".to_string()));
        group.id = "group-1".to_string();
        sessions[0].group_id = Some(group.id.clone());
        sessions[2].group_id = Some(group.id.clone());
        sessions[2].parent_session_id = Some("s1".to_string());

        let items = build_filtered_left_items(
            &projects,
            &sessions,
            std::slice::from_ref(&group),
            &HashSet::new(),
            5,
            "",
        );
        assert_eq!(
            items,
            vec![
                LeftItem::Project(0),
                LeftItem::Session(1),
                LeftItem::Project(1),
                LeftItem::RelatedSeparator,
                LeftItem::Session(0),
                LeftItem::Session(2),
            ]
        );
        // The origin and child each appear exactly once, in the new section.
        assert_eq!(
            items
                .iter()
                .filter(|item| **item == LeftItem::Session(0))
                .count(),
            1
        );
        assert_eq!(
            items
                .iter()
                .filter(|item| **item == LeftItem::Session(2))
                .count(),
            1
        );

        // Searching by group label shows the origin and all of its members.
        let items = build_filtered_left_items(
            &projects,
            &sessions,
            std::slice::from_ref(&group),
            &HashSet::new(),
            5,
            "first",
        );
        assert_eq!(items, vec![LeftItem::Session(0), LeftItem::Session(2)]);

        // Searching by a related member still shows the origin heading, but
        // does not leave an empty project header for a project that only has
        // related members.
        let items = build_filtered_left_items(
            &projects,
            &sessions,
            std::slice::from_ref(&group),
            &HashSet::new(),
            5,
            "other",
        );
        assert_eq!(items, vec![LeftItem::Session(0), LeftItem::Session(2)]);
        assert!(!items.contains(&LeftItem::Project(1)));
    }

    #[test]
    fn related_search_keywords_only_match_sessions_in_the_multi_repo_section() {
        let projects = projects();

        // An origin whose group has no children stays in its project tree and
        // must not be discoverable through the section keywords.
        let mut sessions = sessions();
        let mut childless_group = AgentGroup::new("first".to_string(), Some("s1".to_string()));
        childless_group.id = "g1".to_string();
        sessions[0].group_id = Some("g1".to_string());
        let items = build_filtered_left_items(
            &projects,
            &sessions,
            std::slice::from_ref(&childless_group),
            &HashSet::new(),
            5,
            "multi-repo",
        );
        assert!(
            items.is_empty(),
            "an origin that is still in its project tree is not multi-repo work: {items:?}"
        );

        // Once the group has a split-out child, the origin moves into the
        // section and the keyword should reveal the whole group.
        sessions[2].group_id = Some("g1".to_string());
        sessions[2].parent_session_id = Some("s1".to_string());
        let items = build_filtered_left_items(
            &projects,
            &sessions,
            std::slice::from_ref(&childless_group),
            &HashSet::new(),
            5,
            "multi-repo",
        );
        assert_eq!(items, vec![LeftItem::Session(0), LeftItem::Session(2)]);
    }

    #[test]
    fn scratch_results_get_the_usual_separator() {
        let mut projects = projects();
        projects.push(project(SCRATCH_PROJECT_ID, "Scratch"));
        let mut sessions = sessions();
        sessions.push(session(
            "scratch-1",
            SCRATCH_PROJECT_ID,
            "adhoc",
            "adhoc",
            "codex",
        ));

        let items = filtered(&projects, &sessions, "codex");
        assert_eq!(
            items.last(),
            Some(&LeftItem::Session(3)),
            "the scratch agent should be the last row"
        );
        assert!(
            items.contains(&LeftItem::ScratchSeparator),
            "the scratch group should keep its separator: {items:?}"
        );
    }
}
