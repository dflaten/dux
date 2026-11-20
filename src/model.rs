use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// GitHub CLI availability status, checked once at startup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GhStatus {
    /// Not yet checked.
    #[default]
    Unknown,
    /// `gh` binary not found on PATH.
    NotInstalled,
    /// `gh` found but `gh auth status` failed.
    NotAuthenticated,
    /// `gh` installed and authenticated.
    Available,
}

/// State of a GitHub pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

/// Stable identity for a pull request, independent of any agent session.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrKey {
    /// Lowercased GitHub host.
    pub host: String,
    /// Lowercased `owner/repo` of the repository the PR was opened against.
    pub owner_repo: String,
    pub number: u64,
}

impl PrKey {
    pub fn new(host: impl Into<String>, owner_repo: impl Into<String>, number: u64) -> Self {
        Self {
            host: host.into().trim().to_ascii_lowercase(),
            owner_repo: owner_repo.into().trim().to_ascii_lowercase(),
            number,
        }
    }
}

/// Cached information about a GitHub pull request.
#[derive(Clone, Debug)]
pub struct PrInfo {
    pub number: u64,
    pub state: PrState,
    pub title: String,
    pub host: String,
    pub owner_repo: String,
    pub url: String,
    pub is_draft: bool,
    pub head_ref: String,
    pub base_ref: String,
    /// Repository containing the head branch. Empty means it matches
    /// `owner_repo` (same-repository branch, not a fork).
    pub head_repo: String,
}

impl PrInfo {
    pub fn key(&self) -> PrKey {
        PrKey::new(self.host.clone(), self.owner_repo.clone(), self.number)
    }

    /// Ordering used to choose the PR to display for a worktree. Active PRs
    /// beat merged/closed history; drafts are still open but rank after a
    /// regular open PR.
    pub fn state_rank(&self) -> u8 {
        match self.state {
            PrState::Open if !self.is_draft => 4,
            PrState::Open => 3,
            PrState::Merged => 2,
            PrState::Closed => 1,
        }
    }
}

impl PrState {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Open => "OPEN",
            Self::Merged => "MERGED",
            Self::Closed => "CLOSED",
        }
    }

    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "OPEN" => Some(Self::Open),
            "MERGED" => Some(Self::Merged),
            "CLOSED" => Some(Self::Closed),
            _ => None,
        }
    }
}

/// How a pull request became associated with a worktree session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrLinkSource {
    Discovered,
    Created,
    Imported,
    Manual,
}

impl PrLinkSource {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::Created => "created",
            Self::Imported => "imported",
            Self::Manual => "manual",
        }
    }

    pub fn from_db_str(value: &str) -> Self {
        match value {
            "created" => Self::Created,
            "imported" => Self::Imported,
            "manual" => Self::Manual,
            _ => Self::Discovered,
        }
    }
}

/// How a parent relationship between two stacked PRs was established.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrRelationSource {
    Detected,
    Manual,
}

impl PrRelationSource {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Detected => "detected",
            Self::Manual => "manual",
        }
    }

    pub fn from_db_str(value: &str) -> Self {
        if value == "manual" {
            Self::Manual
        } else {
            Self::Detected
        }
    }
}

/// A PR-to-PR stack edge: `child` is based on `parent`'s head branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrStackEdge {
    pub child: PrKey,
    pub parent: PrKey,
    pub source: PrRelationSource,
}

/// Reserved project id for the built-in scratch workspace. Scratch agents are
/// stored in SQLite like any other agent but are never written to
/// `config.toml`, so the workspace stays runtime-only state.
pub const SCRATCH_PROJECT_ID: &str = "__dux_scratch__";

/// Display name for the built-in scratch workspace.
pub const SCRATCH_PROJECT_NAME: &str = "Scratch";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderKind(String);

impl ProviderKind {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn from_str(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub explicit_default_provider: Option<ProviderKind>,
    pub default_provider: ProviderKind,
    pub leading_branch: Option<String>,
    pub auto_reopen_agents: Option<bool>,
    pub startup_command: Option<String>,
    pub env: BTreeMap<String, String>,
    pub current_branch: String,
    pub branch_status: ProjectBranchStatus,
    pub path_missing: bool,
}

impl Project {
    /// Whether this is the built-in scratch workspace rather than a user project.
    pub fn is_scratch(&self) -> bool {
        self.id == SCRATCH_PROJECT_ID
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectBranchStatus {
    Leading,
    NotLeading,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Active,
    Detached,
    Exited,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Detached => "detached",
            Self::Exited => "exited",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "active" => Self::Active,
            "exited" => Self::Exited,
            _ => Self::Detached,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompanionTerminalStatus {
    NotLaunched,
    Running,
    Exited,
}

impl CompanionTerminalStatus {
    pub fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionSurface {
    Agent,
    Terminal,
}

/// A group of agent sessions that represent one piece of work spanning
/// multiple worktrees or repositories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentGroup {
    pub id: String,
    /// Fallback label used when the origin session no longer exists.
    pub label: String,
    pub origin_session_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AgentGroup {
    pub fn new(label: String, origin_session_id: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            label,
            origin_session_id,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AgentSession {
    pub id: String,
    pub project_id: String,
    pub project_path: Option<String>,
    pub provider: ProviderKind,
    pub source_branch: String,
    pub branch_name: String,
    pub worktree_path: String,
    pub title: Option<String>,
    pub started_providers: Vec<String>,
    pub provider_session_ids: BTreeMap<String, String>,
    pub desired_running: bool,
    pub auto_reopen_enabled: bool,
    pub status: SessionStatus,
    pub group_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AgentSession {
    /// Whether this agent runs in the built-in scratch workspace instead of a
    /// project worktree.
    pub fn is_scratch(&self) -> bool {
        self.project_id == SCRATCH_PROJECT_ID
    }

    pub fn has_started_provider(&self, provider: &ProviderKind) -> bool {
        self.started_providers
            .iter()
            .any(|started| started == provider.as_str())
    }

    pub fn mark_provider_started(&mut self, provider: &ProviderKind) -> bool {
        if self.has_started_provider(provider) {
            return false;
        }
        self.started_providers.push(provider.as_str().to_string());
        true
    }

    pub fn provider_session_id(&self, provider: &ProviderKind) -> Option<&str> {
        self.provider_session_ids
            .get(provider.as_str())
            .map(String::as_str)
            .filter(|id| !id.trim().is_empty())
    }

    pub fn set_provider_session_id(&mut self, provider: &ProviderKind, session_id: String) -> bool {
        if session_id.trim().is_empty() {
            return false;
        }
        let key = provider.as_str().to_string();
        if self.provider_session_ids.get(&key) == Some(&session_id) {
            return false;
        }
        self.provider_session_ids.insert(key, session_id);
        true
    }
}

/// How a delegated child worktree chooses its starting point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseMode {
    Leading,
    Ref,
    ParentHead,
    ParentSnapshot,
    SiblingHead,
}

impl BaseMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Leading => "leading",
            Self::Ref => "ref",
            Self::ParentHead => "parent-head",
            Self::ParentSnapshot => "parent-snapshot",
            Self::SiblingHead => "sibling-head",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "leading" | "project" => Some(Self::Leading),
            "ref" | "branch" | "commit" => Some(Self::Ref),
            "parent-head" | "parent" => Some(Self::ParentHead),
            "parent-snapshot" | "snapshot" => Some(Self::ParentSnapshot),
            "sibling-head" | "sibling" => Some(Self::SiblingHead),
            _ => None,
        }
    }

    pub fn requires_same_repo(self) -> bool {
        matches!(
            self,
            Self::ParentHead | Self::ParentSnapshot | Self::SiblingHead
        )
    }
}

/// Lifecycle of one parent-to-child delegation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelegationState {
    Queued,
    Preparing,
    Starting,
    Resuming,
    Running,
    Completed,
    Failed,
    Blocked,
    Cancelled,
    Exited,
    Interrupted,
}

impl DelegationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Preparing => "preparing",
            Self::Starting => "starting",
            Self::Resuming => "resuming",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
            Self::Exited => "exited",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "preparing" => Some(Self::Preparing),
            "starting" => Some(Self::Starting),
            "resuming" => Some(Self::Resuming),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "blocked" => Some(Self::Blocked),
            "cancelled" => Some(Self::Cancelled),
            "exited" => Some(Self::Exited),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }

    /// States a `wait` call returns on. `blocked` is terminal for a wait but
    /// resumable with a follow-up.
    pub fn is_wait_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Blocked
                | Self::Cancelled
                | Self::Exited
                | Self::Interrupted
        )
    }

    pub fn is_resumable(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Blocked)
    }

    pub fn is_active(self) -> bool {
        !self.is_wait_terminal()
    }
}

/// One task edge from a parent agent to a delegated child agent.
#[derive(Clone, Debug)]
pub struct Delegation {
    pub id: String,
    pub group_id: String,
    pub parent_session_id: String,
    pub child_session_id: Option<String>,
    pub depth: u32,
    pub iteration: u32,
    pub title: String,
    pub request_key: Option<String>,
    pub brief_path: String,
    pub project_id: Option<String>,
    pub repo_path: Option<String>,
    pub base_mode: BaseMode,
    pub base_ref: Option<String>,
    pub base_label: Option<String>,
    pub requested_name: Option<String>,
    pub provider: Option<String>,
    pub state: DelegationState,
    pub state_reason: Option<String>,
    pub result_id: Option<String>,
    pub result_status: Option<String>,
    pub result_summary: Option<String>,
    pub result_path: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub integrated_at: Option<DateTime<Utc>>,
    pub integration_commit: Option<String>,
}

impl Delegation {
    pub fn label(&self) -> &str {
        if self.title.trim().is_empty() {
            self.requested_name.as_deref().unwrap_or(&self.id)
        } else {
            &self.title
        }
    }

    pub fn is_for_session(&self, session_id: &str) -> bool {
        self.child_session_id.as_deref() == Some(session_id)
    }
}

/// The durable result a delegated child reports through `dux agent finish`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DelegationResult {
    #[serde(default)]
    pub result_id: String,
    pub status: String,
    pub summary: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub tests: Vec<DelegationTestResult>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub suggested_followups: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub reported_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DelegationTestResult {
    pub command: String,
    pub status: String,
}

impl DelegationResult {
    pub fn state(&self) -> DelegationState {
        match self.status.as_str() {
            "success" | "completed" => DelegationState::Completed,
            "blocked" => DelegationState::Blocked,
            _ => DelegationState::Failed,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChangedFile {
    pub status: String,
    pub path: String,
    pub additions: usize,
    pub deletions: usize,
    pub binary: bool,
    pub branch_only: bool,
}

impl ChangedFile {
    pub fn is_branch_only(&self) -> bool {
        self.branch_only
    }
}
