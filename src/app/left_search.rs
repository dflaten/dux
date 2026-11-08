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

/// Whether every term matches at least one of the candidates.
fn candidates_match(candidates: &[String], terms: &[String]) -> bool {
    terms.iter().all(|term| {
        candidates
            .iter()
            .any(|candidate| term_matches(term, candidate))
    })
}

/// The lowercased fields of a project that participate in matching.
fn project_candidates(project: &Project) -> Vec<String> {
    vec![project.name.to_lowercase(), project.path.to_lowercase()]
}

/// The lowercased fields of an agent session that participate in matching.
fn session_candidates(session: &AgentSession, project: &Project) -> Vec<String> {
    let label = session
        .title
        .as_deref()
        .unwrap_or(session.branch_name.as_str());
    let mut candidates = vec![
        label.to_lowercase(),
        session.branch_name.to_lowercase(),
        session.provider.as_str().to_lowercase(),
        session.status.as_str().to_lowercase(),
        session.worktree_path.to_lowercase(),
        project.name.to_lowercase(),
        project.path.to_lowercase(),
    ];
    if let Some(project_path) = session.project_path.as_deref() {
        candidates.push(project_path.to_lowercase());
    }
    if session.is_scratch() {
        candidates.push("scratch".to_string());
    }
    candidates
}

/// Whether the whole query matches the project.
fn project_matches(project: &Project, terms: &[String]) -> bool {
    candidates_match(&project_candidates(project), terms)
}

/// Whether the whole query matches the session.
fn session_matches(session: &AgentSession, project: &Project, terms: &[String]) -> bool {
    candidates_match(&session_candidates(session, project), terms)
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
    collapsed_projects: &HashSet<String>,
    empty_project_separator_min_projects: u16,
    query: &str,
) -> Vec<LeftItem> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return build_left_items(
            projects,
            sessions,
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

    let mut items = Vec::new();
    for (project_index, project) in projects.iter().enumerate() {
        if project.is_scratch() {
            continue;
        }
        let project_hit = project_matches(project, &terms);
        let matched = matching_sessions(
            project,
            sessions,
            sessions_by_project.get(project.id.as_str()),
            &terms,
            project_hit,
        );
        if !project_hit && matched.is_empty() {
            continue;
        }
        items.push(LeftItem::Project(project_index));
        items.extend(matched.into_iter().map(LeftItem::Session));
    }

    // Scratch always lives below the project groups.
    if let Some((_, project)) = projects.iter().enumerate().find(|(_, p)| p.is_scratch()) {
        let project_hit = project_matches(project, &terms);
        let matched = matching_sessions(
            project,
            sessions,
            sessions_by_project.get(SCRATCH_PROJECT_ID),
            &terms,
            project_hit,
        );
        if project_hit || !matched.is_empty() {
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
    session_indices: Option<&Vec<usize>>,
    terms: &[String],
    project_hit: bool,
) -> Vec<usize> {
    let Some(indices) = session_indices else {
        return Vec::new();
    };
    let mut matched = Vec::new();
    for &index in indices {
        let session = &sessions[index];
        if project_hit || session_matches(session, project, terms) {
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
        self.left_search.move_end();
        if !self.left_search_active {
            self.left_search_origin = self.selected_left_target();
        }
        self.left_search_active = true;
        self.set_info(
            "Project search: type to filter projects and agents. Enter applies the filter, Esc clears it.",
        );
    }

    /// Leaves the search field.
    ///
    /// Applying keeps the query as a filter on the sidebar; clearing drops
    /// the query and restores the row that was selected when the search
    /// opened.
    pub(crate) fn close_left_search(&mut self, apply: bool) {
        let target = if apply {
            self.selected_left_target()
        } else {
            self.left_search_origin.clone()
        };
        if !apply {
            self.left_search.clear();
        }
        self.left_search_active = false;
        self.left_search_origin = None;
        self.rebuild_left_items();
        if let Some(target) = target {
            self.select_left_target(&target);
        }
        if apply {
            self.set_info(
                "Project filter applied. Press Esc to clear it and show every project and agent again.",
            );
        } else {
            self.set_info("Project search cleared. The full project list is shown again.");
        }
    }

    /// Drops the sidebar filter entirely.
    pub(crate) fn clear_left_search(&mut self) {
        if !self.has_left_search() && !self.left_search_active {
            return;
        }
        let target = self.selected_left_target();
        self.left_search.clear();
        self.left_search_active = false;
        self.left_search_origin = None;
        self.rebuild_left_items();
        if let Some(target) = target {
            self.select_left_target(&target);
        }
    }

    /// Re-applies the query after the search text changed.
    ///
    /// The selected row is kept when it still matches, otherwise the first
    /// matching row is selected.
    pub(crate) fn update_left_search(&mut self) {
        let target = self.selected_left_target();
        self.rebuild_left_items();
        if let Some(target) = target
            && self.select_left_target(&target)
        {
            return;
        }
        self.select_first_left_item();
    }

    /// Ends search editing mode when the left pane is no longer focused.
    ///
    /// The filter itself stays applied so the user comes back to the same
    /// narrowed list.
    pub(crate) fn deactivate_left_search_if_unfocused(&mut self) {
        if self.left_search_active && self.focus != FocusPane::Left {
            self.left_search_active = false;
            self.left_search_origin = None;
        }
    }

    /// The identity of the currently selected left pane row.
    pub(crate) fn selected_left_target(&self) -> Option<LeftSelectionTarget> {
        match self.left_items().get(self.selected_left)? {
            LeftItem::Project(index) => {
                let project = self.projects.get(*index)?;
                if project.is_scratch() {
                    Some(LeftSelectionTarget::Scratch)
                } else {
                    Some(LeftSelectionTarget::Project(project.id.clone()))
                }
            }
            LeftItem::Session(index) => {
                let session = self.sessions.get(*index)?;
                Some(LeftSelectionTarget::Session(session.id.clone()))
            }
            _ => None,
        }
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
        AgentSession, Project, ProjectBranchStatus, ProviderKind, SCRATCH_PROJECT_ID, SessionStatus,
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
        build_filtered_left_items(projects, sessions, &HashSet::new(), 5, query)
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

        let items = build_filtered_left_items(&projects(), &sessions(), &collapsed, 5, "second");
        assert_eq!(items, vec![LeftItem::Project(0), LeftItem::Session(1)]);

        // Without a filter the collapsed project keeps its collapsed shape.
        let items = build_filtered_left_items(&projects(), &sessions(), &collapsed, 5, "");
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
        let items = build_filtered_left_items(&projects, &sessions, &HashSet::new(), 1, "first");
        assert_eq!(items, vec![LeftItem::Project(0), LeftItem::Session(0)]);
    }
}
