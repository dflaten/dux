use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};

use crate::config::ProjectConfig;
use crate::model::{AgentSession, SessionStatus};

/// An agent session retained after deletion so it can be recovered later.
#[derive(Clone, Debug)]
pub struct DeletedAgentSession {
    pub session: AgentSession,
    pub deleted_at: DateTime<Utc>,
}

const UPSERT_SESSION_SQL: &str = r#"
    insert into agent_sessions
        (id, project_id, project_path, provider, source_branch, branch_name, worktree_path, title, started_providers, provider_session_ids, desired_running, auto_reopen_enabled, status, group_id, parent_session_id, created_at, updated_at)
    values
        (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
    on conflict(id) do update set
        project_path=excluded.project_path,
        provider=excluded.provider,
        source_branch=excluded.source_branch,
        branch_name=excluded.branch_name,
        worktree_path=excluded.worktree_path,
        title=excluded.title,
        started_providers=excluded.started_providers,
        provider_session_ids=excluded.provider_session_ids,
        desired_running=excluded.desired_running,
        auto_reopen_enabled=excluded.auto_reopen_enabled,
        status=excluded.status,
        group_id=excluded.group_id,
        parent_session_id=excluded.parent_session_id,
        updated_at=excluded.updated_at
    "#;

/// Canonical pull request data, independent of any worktree session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredPullRequest {
    pub host: String,
    pub owner_repo: String,
    pub number: u64,
    pub state: String,
    pub is_draft: bool,
    pub title: String,
    pub url: String,
    pub head_ref: String,
    pub base_ref: String,
    pub head_repo: String,
}

/// A link between a worktree session and a canonical pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredPrLink {
    pub session_id: String,
    pub key: crate::model::PrKey,
    pub source: crate::model::PrLinkSource,
}

/// A stored stack edge between two pull requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredPrStack {
    pub child: crate::model::PrKey,
    pub parent: crate::model::PrKey,
    pub source: crate::model::PrRelationSource,
}

pub struct SessionStore {
    conn: Connection,
}

impl SessionStore {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn =
            Connection::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            create table if not exists agent_sessions (
                id text primary key,
                project_id text not null,
                provider text not null,
                source_branch text not null,
                branch_name text not null,
                worktree_path text not null,
                title text,
                project_path text,
                status text not null,
                group_id text,
                parent_session_id text,
                created_at text not null,
                updated_at text not null
            );
            "#,
        )?;
        self.conn.execute_batch(
            r#"
            create table if not exists projects (
                id text primary key,
                path text not null unique,
                name text,
                default_provider text,
                leading_branch text,
                auto_reopen_agents integer,
                startup_command text,
                env text not null default '{}',
                sort_order integer not null default 0,
                created_at text not null,
                updated_at text not null
            );
            "#,
        )?;
        ensure_column(&self.conn, "projects", "name", "text")?;
        ensure_column(&self.conn, "projects", "default_provider", "text")?;
        ensure_column(&self.conn, "projects", "leading_branch", "text")?;
        ensure_column(&self.conn, "projects", "auto_reopen_agents", "integer")?;
        ensure_column(&self.conn, "projects", "startup_command", "text")?;
        ensure_column(&self.conn, "projects", "env", "text not null default '{}'")?;
        ensure_column(
            &self.conn,
            "projects",
            "sort_order",
            "integer not null default 0",
        )?;
        ensure_column(
            &self.conn,
            "projects",
            "created_at",
            "text not null default ''",
        )?;
        ensure_column(
            &self.conn,
            "projects",
            "updated_at",
            "text not null default ''",
        )?;
        ensure_column(&self.conn, "agent_sessions", "title", "text")?;
        ensure_column(&self.conn, "agent_sessions", "project_path", "text")?;
        ensure_column(
            &self.conn,
            "agent_sessions",
            "started_providers",
            "text not null default '[]'",
        )?;
        ensure_column(
            &self.conn,
            "agent_sessions",
            "provider_session_ids",
            "text not null default '{}'",
        )?;
        ensure_column(
            &self.conn,
            "agent_sessions",
            "desired_running",
            "integer not null default 0",
        )?;
        ensure_column(
            &self.conn,
            "agent_sessions",
            "auto_reopen_enabled",
            "integer not null default 1",
        )?;
        ensure_column(&self.conn, "agent_sessions", "group_id", "text")?;
        ensure_column(&self.conn, "agent_sessions", "parent_session_id", "text")?;
        self.conn.execute_batch(
            r#"
            create table if not exists agent_groups (
                id text primary key,
                label text not null,
                origin_session_id text,
                created_at text not null,
                updated_at text not null
            );
            "#,
        )?;
        self.conn.execute_batch(
            r#"
            create table if not exists deleted_agent_sessions (
                id text primary key,
                project_id text not null,
                project_path text,
                provider text not null,
                source_branch text not null,
                branch_name text not null,
                worktree_path text not null,
                title text,
                started_providers text not null default '[]',
                provider_session_ids text not null default '{}',
                desired_running integer not null default 0,
                auto_reopen_enabled integer not null default 1,
                status text not null,
                group_id text,
                parent_session_id text,
                created_at text not null,
                updated_at text not null,
                deleted_at text not null
            );
            "#,
        )?;
        ensure_column(&self.conn, "deleted_agent_sessions", "group_id", "text")?;
        ensure_column(
            &self.conn,
            "deleted_agent_sessions",
            "parent_session_id",
            "text",
        )?;
        self.conn.execute_batch(
            r#"
            create table if not exists session_prs (
                session_id text not null,
                pr_number integer not null,
                host text not null default 'github.com',
                owner_repo text not null,
                state text not null default 'OPEN',
                primary key (session_id, pr_number),
                foreign key (session_id) references agent_sessions(id) on delete cascade
            );
            "#,
        )?;
        ensure_column(
            &self.conn,
            "session_prs",
            "host",
            "text not null default 'github.com'",
        )?;
        ensure_column(
            &self.conn,
            "session_prs",
            "state",
            "text not null default 'OPEN'",
        )?;
        ensure_column(
            &self.conn,
            "session_prs",
            "title",
            "text not null default ''",
        )?;
        ensure_column(&self.conn, "session_prs", "url", "text not null default ''")?;
        self.conn.execute_batch(
            r#"
            create table if not exists pull_requests (
                host text not null,
                owner_repo text not null,
                number integer not null,
                state text not null default 'OPEN',
                is_draft integer not null default 0,
                title text not null default '',
                url text not null default '',
                head_ref text not null default '',
                base_ref text not null default '',
                head_repo text not null default '',
                updated_at text not null,
                checked_at text not null,
                primary key (host, owner_repo, number)
            );
            create index if not exists idx_pull_requests_head
                on pull_requests(host, owner_repo, head_ref);
            create table if not exists worktree_pull_requests (
                session_id text not null,
                host text not null,
                owner_repo text not null,
                number integer not null,
                source text not null default 'discovered',
                linked_at text not null,
                primary key (session_id, host, owner_repo, number)
            );
            create index if not exists idx_worktree_pull_requests_pr
                on worktree_pull_requests(host, owner_repo, number);
            create table if not exists pull_request_stacks (
                child_host text not null,
                child_owner_repo text not null,
                child_number integer not null,
                parent_host text not null,
                parent_owner_repo text not null,
                parent_number integer not null,
                source text not null default 'detected',
                created_at text not null,
                updated_at text not null,
                primary key (child_host, child_owner_repo, child_number)
            );
            create index if not exists idx_pull_request_stacks_parent
                on pull_request_stacks(parent_host, parent_owner_repo, parent_number);
            "#,
        )?;
        // Copy legacy session_prs rows into the canonical tables. This is
        // idempotent so it is safe to run on every startup.
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            insert or ignore into pull_requests
                (host, owner_repo, number, state, is_draft, title, url, head_ref, base_ref, head_repo, updated_at, checked_at)
            select lower(host), lower(owner_repo), pr_number, state, 0, title, url, '', '', '', ?1, ?1
            from session_prs
            "#,
            params![now],
        )?;
        self.conn.execute(
            r#"
            insert or ignore into worktree_pull_requests
                (session_id, host, owner_repo, number, source, linked_at)
            select session_id, lower(host), lower(owner_repo), pr_number, 'discovered', ?1
            from session_prs
            "#,
            params![now],
        )?;
        Ok(())
    }

    /// Insert or update a project. If a project with the same path already
    /// exists under a different id, keep the existing id so sessions remain
    /// attached and refresh the editable metadata.
    pub fn upsert_project(&self, project: &ProjectConfig) -> Result<()> {
        let sort_order = self.next_project_sort_order()?;
        self.upsert_project_at(project, sort_order)
    }

    pub fn upsert_project_at(&self, project: &ProjectConfig, sort_order: i64) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            r#"
            update projects
            set path = ?2,
                name = ?3,
                default_provider = ?4,
                leading_branch = ?5,
                auto_reopen_agents = ?6,
                startup_command = ?7,
                env = ?8,
                sort_order = ?9,
                updated_at = ?10
            where id = ?1
            "#,
            params![
                project.id,
                project.path,
                project.name,
                project.default_provider,
                project.leading_branch,
                project.auto_reopen_agents,
                project.startup_command,
                serialize_project_env(&project.env),
                sort_order,
                now,
            ],
        )?;
        if updated > 0 {
            return Ok(());
        }

        self.conn.execute(
            r#"
            insert into projects
                (id, path, name, default_provider, leading_branch, auto_reopen_agents, startup_command, env, sort_order, created_at, updated_at)
            values
                (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
            on conflict(path) do update set
                name=excluded.name,
                default_provider=excluded.default_provider,
                leading_branch=excluded.leading_branch,
                auto_reopen_agents=excluded.auto_reopen_agents,
                startup_command=excluded.startup_command,
                env=excluded.env,
                sort_order=excluded.sort_order,
                updated_at=excluded.updated_at
            "#,
            params![
                project.id,
                project.path,
                project.name,
                project.default_provider,
                project.leading_branch,
                project.auto_reopen_agents,
                project.startup_command,
                serialize_project_env(&project.env),
                sort_order,
                now,
            ],
        )?;
        Ok(())
    }

    fn next_project_sort_order(&self) -> Result<i64> {
        self.conn
            .query_row(
                "select coalesce(max(sort_order) + 1, 0) from projects",
                [],
                |row| row.get(0),
            )
            .context("failed to compute next project sort order")
    }

    pub fn load_projects(&self) -> Result<Vec<ProjectConfig>> {
        let mut stmt = self.conn.prepare(
            r#"
            select id, path, name, default_provider, leading_branch, auto_reopen_agents, startup_command, env
            from projects
            order by sort_order, name collate nocase, path collate nocase
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ProjectConfig {
                id: row.get(0)?,
                path: row.get(1)?,
                name: row.get(2)?,
                default_provider: row.get(3)?,
                leading_branch: row.get(4)?,
                auto_reopen_agents: row.get(5)?,
                startup_command: row.get(6)?,
                env: deserialize_project_env(row.get::<_, String>(7)?.as_str()),
            })
        })?;

        let mut projects = Vec::new();
        for row in rows {
            projects.push(row?);
        }
        Ok(projects)
    }

    pub fn upsert_group(&self, group: &crate::model::AgentGroup) -> Result<()> {
        self.conn.execute(
            r#"
            insert into agent_groups (id, label, origin_session_id, created_at, updated_at)
            values (?1, ?2, ?3, ?4, ?5)
            on conflict(id) do update set
                label=excluded.label,
                origin_session_id=excluded.origin_session_id,
                updated_at=excluded.updated_at
            "#,
            params![
                group.id,
                group.label,
                group.origin_session_id,
                group.created_at.to_rfc3339(),
                group.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn load_groups(&self) -> Result<Vec<crate::model::AgentGroup>> {
        let mut stmt = self.conn.prepare(
            r#"
            select id, label, origin_session_id, created_at, updated_at
            from agent_groups
            order by created_at desc
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            let created_at: String = row.get(3)?;
            let updated_at: String = row.get(4)?;
            Ok(crate::model::AgentGroup {
                id: row.get(0)?,
                label: row.get(1)?,
                origin_session_id: row.get(2)?,
                created_at: parse_time(&created_at).unwrap_or_else(Utc::now),
                updated_at: parse_time(&updated_at).unwrap_or_else(Utc::now),
            })
        })?;
        let mut groups = Vec::new();
        for row in rows {
            groups.push(row?);
        }
        Ok(groups)
    }

    pub fn set_session_group(
        &self,
        id: &str,
        group_id: Option<&str>,
        parent_session_id: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            update agent_sessions
            set group_id = ?2, parent_session_id = ?3
            where id = ?1
            "#,
            params![id, group_id, parent_session_id],
        )?;
        Ok(())
    }

    /// Remove groups that no longer have any active or archived sessions.
    pub fn purge_empty_groups(&self) -> Result<()> {
        self.conn.execute(
            r#"
            delete from agent_groups
            where id not in (select group_id from agent_sessions where group_id is not null)
              and id not in (select group_id from deleted_agent_sessions where group_id is not null)
            "#,
            [],
        )?;
        Ok(())
    }

    pub fn update_project_default_provider(
        &self,
        project_id: &str,
        default_provider: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            update projects
            set default_provider = ?2,
                updated_at = ?3
            where id = ?1
            "#,
            params![project_id, default_provider, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn update_project_auto_reopen(
        &self,
        project_id: &str,
        auto_reopen_agents: Option<bool>,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            update projects
            set auto_reopen_agents = ?2,
                updated_at = ?3
            where id = ?1
            "#,
            params![project_id, auto_reopen_agents, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn update_project_startup_command(
        &self,
        project_id: &str,
        startup_command: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            update projects
            set startup_command = ?2,
                updated_at = ?3
            where id = ?1
            "#,
            params![project_id, startup_command, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn update_project_env(
        &self,
        project_id: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            update projects
            set env = ?2,
                updated_at = ?3
            where id = ?1
            "#,
            params![
                project_id,
                serialize_project_env(env),
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn delete_project(&self, id: &str) -> Result<()> {
        self.conn
            .execute("delete from projects where id = ?1", params![id])?;
        Ok(())
    }

    // -- Canonical pull request storage -------------------------------------

    pub fn upsert_pull_request(&self, pr: &StoredPullRequest) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            insert into pull_requests
                (host, owner_repo, number, state, is_draft, title, url, head_ref, base_ref, head_repo, updated_at, checked_at)
            values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
            on conflict(host, owner_repo, number) do update set
                state=excluded.state,
                is_draft=excluded.is_draft,
                title=excluded.title,
                url=excluded.url,
                head_ref=excluded.head_ref,
                base_ref=excluded.base_ref,
                head_repo=excluded.head_repo,
                updated_at=excluded.updated_at,
                checked_at=excluded.checked_at
            "#,
            params![
                pr.host.trim().to_ascii_lowercase(),
                pr.owner_repo.trim().to_ascii_lowercase(),
                pr.number as i64,
                pr.state,
                pr.is_draft,
                pr.title,
                pr.url,
                pr.head_ref,
                pr.base_ref,
                pr.head_repo,
                now,
            ],
        )?;
        Ok(())
    }

    pub fn load_pull_requests(&self) -> Result<Vec<StoredPullRequest>> {
        let mut stmt = self.conn.prepare(
            r#"
            select host, owner_repo, number, state, is_draft, title, url, head_ref, base_ref, head_repo
            from pull_requests
            order by updated_at desc
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StoredPullRequest {
                host: row.get(0)?,
                owner_repo: row.get(1)?,
                number: row.get::<_, i64>(2)? as u64,
                state: row.get(3)?,
                is_draft: row.get(4)?,
                title: row.get(5)?,
                url: row.get(6)?,
                head_ref: row.get(7)?,
                base_ref: row.get(8)?,
                head_repo: row.get(9)?,
            })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    pub fn link_pull_request(
        &self,
        session_id: &str,
        key: &crate::model::PrKey,
        source: crate::model::PrLinkSource,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            insert into worktree_pull_requests (session_id, host, owner_repo, number, source, linked_at)
            values (?1, ?2, ?3, ?4, ?5, ?6)
            on conflict(session_id, host, owner_repo, number) do update set
                source = case
                    when worktree_pull_requests.source = 'manual' then 'manual'
                    when excluded.source = 'manual' then 'manual'
                    when worktree_pull_requests.source = 'created'
                        and excluded.source in ('discovered', 'imported') then 'created'
                    when worktree_pull_requests.source = 'imported' and excluded.source = 'discovered' then 'imported'
                    else excluded.source
                end,
                linked_at = excluded.linked_at
            "#,
            params![
                session_id,
                key.host,
                key.owner_repo,
                key.number as i64,
                source.as_db_str(),
                now,
            ],
        )?;
        Ok(())
    }

    /// Remove the discovered association for a branch that no longer has a PR.
    /// Created, imported, and manually linked PRs are preserved.
    pub fn unlink_discovered_pr_for_branch(
        &self,
        session_id: &str,
        host: &str,
        owner_repo: &str,
        branch: &str,
    ) -> Result<()> {
        let host = host.trim().to_ascii_lowercase();
        let owner_repo = owner_repo.trim().to_ascii_lowercase();
        self.conn.execute(
            r#"
            delete from worktree_pull_requests
            where session_id = ?1
              and source = 'discovered'
              and host = ?2
              and owner_repo = ?3
              and number in (
                  select number from pull_requests
                  where host = ?2 and owner_repo = ?3 and head_ref = ?4
              )
            "#,
            params![session_id, host, owner_repo, branch],
        )?;
        Ok(())
    }

    pub fn load_pr_links(&self) -> Result<Vec<StoredPrLink>> {
        let mut stmt = self.conn.prepare(
            r#"
            select session_id, host, owner_repo, number, source
            from worktree_pull_requests
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StoredPrLink {
                session_id: row.get(0)?,
                key: crate::model::PrKey::new(
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)? as u64,
                ),
                source: crate::model::PrLinkSource::from_db_str(&row.get::<_, String>(4)?),
            })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    pub fn load_pr_stacks(&self) -> Result<Vec<StoredPrStack>> {
        let mut stmt = self.conn.prepare(
            r#"
            select child_host, child_owner_repo, child_number,
                   parent_host, parent_owner_repo, parent_number, source
            from pull_request_stacks
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StoredPrStack {
                child: crate::model::PrKey::new(
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? as u64,
                ),
                parent: crate::model::PrKey::new(
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)? as u64,
                ),
                source: crate::model::PrRelationSource::from_db_str(&row.get::<_, String>(6)?),
            })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    pub fn upsert_pr_stack(
        &self,
        child: &crate::model::PrKey,
        parent: &crate::model::PrKey,
        source: crate::model::PrRelationSource,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            insert into pull_request_stacks
                (child_host, child_owner_repo, child_number,
                 parent_host, parent_owner_repo, parent_number, source, created_at, updated_at)
            values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
            on conflict(child_host, child_owner_repo, child_number) do update set
                parent_host = case
                    when pull_request_stacks.source = 'manual' and excluded.source = 'detected'
                        then pull_request_stacks.parent_host
                    else excluded.parent_host
                end,
                parent_owner_repo = case
                    when pull_request_stacks.source = 'manual' and excluded.source = 'detected'
                        then pull_request_stacks.parent_owner_repo
                    else excluded.parent_owner_repo
                end,
                parent_number = case
                    when pull_request_stacks.source = 'manual' and excluded.source = 'detected'
                        then pull_request_stacks.parent_number
                    else excluded.parent_number
                end,
                source = case
                    when pull_request_stacks.source = 'manual' and excluded.source = 'detected' then 'manual'
                    else excluded.source
                end,
                updated_at=excluded.updated_at
            "#,
            params![
                child.host,
                child.owner_repo,
                child.number as i64,
                parent.host,
                parent.owner_repo,
                parent.number as i64,
                source.as_db_str(),
                now,
            ],
        )?;
        Ok(())
    }

    pub fn delete_pr_stack(&self, child: &crate::model::PrKey) -> Result<()> {
        self.conn.execute(
            r#"
            delete from pull_request_stacks
            where child_host = ?1 and child_owner_repo = ?2 and child_number = ?3
            "#,
            params![child.host, child.owner_repo, child.number as i64],
        )?;
        Ok(())
    }

    pub fn delete_detected_pr_stack(&self, child: &crate::model::PrKey) -> Result<()> {
        self.conn.execute(
            r#"
            delete from pull_request_stacks
            where child_host = ?1 and child_owner_repo = ?2 and child_number = ?3
              and source = 'detected'
            "#,
            params![child.host, child.owner_repo, child.number as i64],
        )?;
        Ok(())
    }

    /// Remove stack edges whose endpoints are gone, then remove canonical PRs
    /// that are neither linked to a worktree nor part of a stack.
    pub fn prune_orphaned_pull_requests(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            delete from pull_request_stacks
            where (child_host, child_owner_repo, child_number) not in (
                    select host, owner_repo, number from pull_requests
                  )
               or (parent_host, parent_owner_repo, parent_number) not in (
                    select host, owner_repo, number from pull_requests
                  );
            delete from pull_requests
            where not exists (
                    select 1 from worktree_pull_requests links
                    where links.host = pull_requests.host
                      and links.owner_repo = pull_requests.owner_repo
                      and links.number = pull_requests.number
                  )
              and not exists (
                    select 1 from pull_request_stacks stacks
                    where (stacks.child_host, stacks.child_owner_repo, stacks.child_number)
                          = (pull_requests.host, pull_requests.owner_repo, pull_requests.number)
                       or (stacks.parent_host, stacks.parent_owner_repo, stacks.parent_number)
                          = (pull_requests.host, pull_requests.owner_repo, pull_requests.number)
                  );
            "#,
        )?;
        Ok(())
    }

    pub fn upsert_session(&self, session: &AgentSession) -> Result<()> {
        self.conn.execute(
            UPSERT_SESSION_SQL,
            params_from_iter(session_values(session)),
        )?;
        Ok(())
    }

    pub fn load_sessions(&self) -> Result<Vec<AgentSession>> {
        let mut stmt = self.conn.prepare(
            r#"
            select id, project_id, provider, source_branch, branch_name, worktree_path, title, project_path, started_providers, provider_session_ids, desired_running, auto_reopen_enabled, status, group_id, parent_session_id, created_at, updated_at
            from agent_sessions
            order by updated_at desc
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            let started_providers: String = row.get(8)?;
            let provider_session_ids: String = row.get(9)?;
            let created_at: String = row.get(15)?;
            let updated_at: String = row.get(16)?;
            Ok(AgentSession {
                id: row.get(0)?,
                project_id: row.get::<_, String>(1).unwrap_or_default(),
                provider: crate::model::ProviderKind::from_str(row.get::<_, String>(2)?.as_str()),
                source_branch: row.get(3)?,
                branch_name: row.get(4)?,
                worktree_path: row.get(5)?,
                title: row.get(6)?,
                project_path: row.get(7)?,
                started_providers: parse_started_providers(&started_providers),
                provider_session_ids: parse_provider_session_ids(&provider_session_ids),
                desired_running: row.get(10)?,
                auto_reopen_enabled: row.get(11)?,
                status: SessionStatus::from_str(row.get::<_, String>(12)?.as_str()),
                group_id: row.get(13)?,
                parent_session_id: row.get(14)?,
                created_at: parse_time(&created_at).unwrap_or_else(Utc::now),
                updated_at: parse_time(&updated_at).unwrap_or_else(Utc::now),
            })
        })?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    pub fn archive_and_delete_session(&mut self, session: &AgentSession) -> Result<()> {
        let transaction = self.conn.transaction()?;
        let mut values = session_values(session);
        values.push(Value::Text(Utc::now().to_rfc3339()));
        transaction.execute(
            r#"
            insert into deleted_agent_sessions
                (id, project_id, project_path, provider, source_branch, branch_name, worktree_path, title, started_providers, provider_session_ids, desired_running, auto_reopen_enabled, status, group_id, parent_session_id, created_at, updated_at, deleted_at)
            values
                (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
            on conflict(id) do update set
                project_id=excluded.project_id,
                project_path=excluded.project_path,
                provider=excluded.provider,
                source_branch=excluded.source_branch,
                branch_name=excluded.branch_name,
                worktree_path=excluded.worktree_path,
                title=excluded.title,
                started_providers=excluded.started_providers,
                provider_session_ids=excluded.provider_session_ids,
                desired_running=excluded.desired_running,
                auto_reopen_enabled=excluded.auto_reopen_enabled,
                status=excluded.status,
                group_id=excluded.group_id,
                parent_session_id=excluded.parent_session_id,
                created_at=excluded.created_at,
                updated_at=excluded.updated_at,
                deleted_at=excluded.deleted_at
            "#,
            params_from_iter(values),
        )?;
        transaction.execute(
            "delete from agent_sessions where id = ?1",
            params![session.id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn permanently_delete_session(&mut self, id: &str) -> Result<()> {
        let transaction = self.conn.transaction()?;
        transaction.execute("delete from agent_sessions where id = ?1", params![id])?;
        transaction.execute(
            "delete from deleted_agent_sessions where id = ?1",
            params![id],
        )?;
        transaction.execute(
            "delete from worktree_pull_requests where session_id = ?1",
            params![id],
        )?;
        transaction.commit()?;
        self.prune_orphaned_pull_requests()?;
        Ok(())
    }

    pub fn purge_deleted_sessions_older_than(
        &mut self,
        cutoff: DateTime<Utc>,
    ) -> Result<Vec<DeletedAgentSession>> {
        let mut statement = self.conn.prepare(
            r#"
            select id, project_id, provider, source_branch, branch_name, worktree_path, title, project_path, started_providers, provider_session_ids, desired_running, auto_reopen_enabled, status, group_id, parent_session_id, created_at, updated_at, deleted_at
            from deleted_agent_sessions where deleted_at <= ?1
            "#,
        )?;
        let deleted = statement
            .query_map(params![cutoff.to_rfc3339()], |row| {
                Ok(DeletedAgentSession {
                    session: session_from_row(row)?,
                    deleted_at: parse_time(&row.get::<_, String>(17)?).unwrap_or_else(Utc::now),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        let transaction = self.conn.transaction()?;
        transaction.execute(
            r#"
            delete from worktree_pull_requests
            where session_id in (
                select id from deleted_agent_sessions where deleted_at <= ?1
            )
            "#,
            params![cutoff.to_rfc3339()],
        )?;
        transaction.execute(
            "delete from deleted_agent_sessions where deleted_at <= ?1",
            params![cutoff.to_rfc3339()],
        )?;
        transaction.commit()?;
        self.prune_orphaned_pull_requests()?;
        Ok(deleted)
    }

    pub fn load_deleted_sessions(&self) -> Result<Vec<DeletedAgentSession>> {
        let mut statement = self.conn.prepare(
            r#"
            select id, project_id, provider, source_branch, branch_name, worktree_path, title, project_path, started_providers, provider_session_ids, desired_running, auto_reopen_enabled, status, group_id, parent_session_id, created_at, updated_at, deleted_at
            from deleted_agent_sessions
            order by deleted_at desc
            "#,
        )?;
        let rows = statement.query_map([], |row| {
            Ok(DeletedAgentSession {
                session: session_from_row(row)?,
                deleted_at: parse_time(&row.get::<_, String>(15)?).unwrap_or_else(Utc::now),
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn restore_deleted_session(&mut self, id: &str) -> Result<Option<AgentSession>> {
        let transaction = self.conn.transaction()?;
        let deleted = load_deleted_session_in(&transaction, id)?;
        let Some(deleted) = deleted else {
            return Ok(None);
        };
        if transaction.execute(
            "delete from deleted_agent_sessions where id = ?1",
            params![id],
        )? != 1
        {
            return Ok(None);
        }
        upsert_session_in(&transaction, &deleted.session)?;
        transaction.commit()?;
        Ok(Some(deleted.session))
    }

    pub fn set_desired_running(&self, id: &str, desired_running: bool) -> Result<()> {
        self.conn.execute(
            "update agent_sessions set desired_running = ?2, updated_at = ?3 where id = ?1",
            params![id, desired_running, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn set_auto_reopen_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        self.conn.execute(
            "update agent_sessions set auto_reopen_enabled = ?2, updated_at = ?3 where id = ?1",
            params![id, enabled, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentSession> {
    let started_providers: String = row.get(8)?;
    let provider_session_ids: String = row.get(9)?;
    let created_at: String = row.get(15)?;
    let updated_at: String = row.get(16)?;
    Ok(AgentSession {
        id: row.get(0)?,
        project_id: row.get::<_, String>(1).unwrap_or_default(),
        provider: crate::model::ProviderKind::from_str(row.get::<_, String>(2)?.as_str()),
        source_branch: row.get(3)?,
        branch_name: row.get(4)?,
        worktree_path: row.get(5)?,
        title: row.get(6)?,
        project_path: row.get(7)?,
        started_providers: parse_started_providers(&started_providers),
        provider_session_ids: parse_provider_session_ids(&provider_session_ids),
        desired_running: row.get(10)?,
        auto_reopen_enabled: row.get(11)?,
        status: SessionStatus::from_str(row.get::<_, String>(12)?.as_str()),
        group_id: row.get(13)?,
        parent_session_id: row.get(14)?,
        created_at: parse_time(&created_at).unwrap_or_else(Utc::now),
        updated_at: parse_time(&updated_at).unwrap_or_else(Utc::now),
    })
}

fn upsert_session_in(
    transaction: &rusqlite::Transaction<'_>,
    session: &AgentSession,
) -> Result<()> {
    transaction.execute(
        UPSERT_SESSION_SQL,
        params_from_iter(session_values(session)),
    )?;
    Ok(())
}

fn session_values(session: &AgentSession) -> Vec<Value> {
    vec![
        Value::Text(session.id.clone()),
        Value::Text(session.project_id.clone()),
        session
            .project_path
            .clone()
            .map_or(Value::Null, Value::Text),
        Value::Text(session.provider.as_str().to_string()),
        Value::Text(session.source_branch.clone()),
        Value::Text(session.branch_name.clone()),
        Value::Text(session.worktree_path.clone()),
        session.title.clone().map_or(Value::Null, Value::Text),
        Value::Text(serialize_started_providers(&session.started_providers)),
        Value::Text(serialize_provider_session_ids(
            &session.provider_session_ids,
        )),
        Value::Integer(i64::from(session.desired_running)),
        Value::Integer(i64::from(session.auto_reopen_enabled)),
        Value::Text(session.status.as_str().to_string()),
        session.group_id.clone().map_or(Value::Null, Value::Text),
        session
            .parent_session_id
            .clone()
            .map_or(Value::Null, Value::Text),
        Value::Text(session.created_at.to_rfc3339()),
        Value::Text(session.updated_at.to_rfc3339()),
    ]
}

fn load_deleted_session_in(
    transaction: &rusqlite::Transaction<'_>,
    id: &str,
) -> Result<Option<DeletedAgentSession>> {
    transaction
        .query_row(
            r#"
            select id, project_id, provider, source_branch, branch_name, worktree_path, title, project_path, started_providers, provider_session_ids, desired_running, auto_reopen_enabled, status, group_id, parent_session_id, created_at, updated_at, deleted_at
            from deleted_agent_sessions where id = ?1
            "#,
            params![id],
            |row| {
                Ok(DeletedAgentSession {
                    session: session_from_row(row)?,
                    deleted_at: parse_time(&row.get::<_, String>(17)?).unwrap_or_else(Utc::now),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn serialize_project_env(env: &BTreeMap<String, String>) -> String {
    serde_json::to_string(env).unwrap_or_else(|_| "{}".to_string())
}

fn deserialize_project_env(value: &str) -> BTreeMap<String, String> {
    serde_json::from_str::<BTreeMap<String, String>>(value).unwrap_or_default()
}

fn serialize_started_providers(started_providers: &[String]) -> String {
    serde_json::to_string(started_providers).unwrap_or_else(|_| "[]".to_string())
}

fn parse_started_providers(value: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(value).unwrap_or_default()
}

fn serialize_provider_session_ids(provider_session_ids: &BTreeMap<String, String>) -> String {
    serde_json::to_string(provider_session_ids).unwrap_or_else(|_| "{}".to_string())
}

fn parse_provider_session_ids(value: &str) -> BTreeMap<String, String> {
    serde_json::from_str::<BTreeMap<String, String>>(value).unwrap_or_default()
}

pub fn fallback_pr_url(host: &str, owner_repo: &str, pr_number: u64) -> String {
    let host = if host.trim().is_empty() {
        "github.com"
    } else {
        host
    };
    format!("https://{host}/{owner_repo}/pull/{pr_number}")
}

/// Opens an in-memory session store for tests.
#[cfg(test)]
fn test_store() -> SessionStore {
    SessionStore::open(std::path::Path::new(":memory:")).unwrap()
}

/// Builds a minimal `AgentSession` with the given id, `created_at`, and `updated_at`.
#[cfg(test)]
fn test_session(
    id: &str,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
) -> crate::model::AgentSession {
    crate::model::AgentSession {
        id: id.to_string(),
        project_id: "proj".to_string(),
        project_path: None,
        provider: crate::model::ProviderKind::new("claude"),
        source_branch: "main".to_string(),
        branch_name: format!("branch-{id}"),
        worktree_path: format!("/tmp/{id}"),
        title: None,
        started_providers: Vec::new(),
        provider_session_ids: BTreeMap::new(),
        desired_running: false,
        auto_reopen_enabled: true,
        status: SessionStatus::Active,
        group_id: None,
        parent_session_id: None,
        created_at,
        updated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn load_sessions_ordered_by_updated_at_desc() {
        let store = test_store();
        let now = Utc::now();

        // Insert three sessions with different updated_at values.
        // oldest updated first, newest updated last.
        let s1 = test_session("a", now - Duration::hours(3), now - Duration::hours(3));
        let s2 = test_session("b", now - Duration::hours(2), now - Duration::hours(1));
        let s3 = test_session("c", now - Duration::hours(1), now - Duration::hours(2));

        store.upsert_session(&s1).unwrap();
        store.upsert_session(&s2).unwrap();
        store.upsert_session(&s3).unwrap();

        let loaded = store.load_sessions().unwrap();
        let ids: Vec<&str> = loaded.iter().map(|s| s.id.as_str()).collect();

        // s2 has the most recent updated_at, then s3, then s1.
        assert_eq!(ids, vec!["b", "c", "a"]);
    }

    #[test]
    fn archived_session_is_listed_newest_first_and_can_be_restored() {
        let mut store = test_store();
        let now = Utc::now();
        let older = test_session("older", now, now);
        let newer = test_session("newer", now, now);
        store.upsert_session(&older).unwrap();
        store.upsert_session(&newer).unwrap();
        store.archive_and_delete_session(&older).unwrap();
        store.archive_and_delete_session(&newer).unwrap();
        store
            .conn
            .execute(
                "update deleted_agent_sessions set deleted_at = ?2 where id = ?1",
                params!["older", (now - Duration::minutes(1)).to_rfc3339()],
            )
            .unwrap();

        let deleted = store.load_deleted_sessions().unwrap();
        assert_eq!(deleted.len(), 2);
        assert_eq!(deleted[0].session.id, "newer");
        assert!(store.load_sessions().unwrap().is_empty());

        let restored = store.restore_deleted_session("newer").unwrap().unwrap();
        assert_eq!(restored.id, "newer");
        assert_eq!(store.load_deleted_sessions().unwrap().len(), 1);
        assert_eq!(store.load_sessions().unwrap()[0].id, "newer");
        assert!(store.restore_deleted_session("newer").unwrap().is_none());
    }

    #[test]
    fn permanently_deleting_session_removes_active_and_archived_records() {
        let mut store = test_store();
        let now = Utc::now();
        let session = test_session("permanent", now, now);
        store.upsert_session(&session).unwrap();
        store.archive_and_delete_session(&session).unwrap();

        store.permanently_delete_session("permanent").unwrap();

        assert!(store.load_sessions().unwrap().is_empty());
        assert!(store.load_deleted_sessions().unwrap().is_empty());
    }

    #[test]
    fn purging_deleted_sessions_removes_only_records_at_or_before_cutoff() {
        let mut store = test_store();
        let now = Utc::now();
        let expired = test_session("expired", now, now);
        let retained = test_session("retained", now, now);
        store.upsert_session(&expired).unwrap();
        store.upsert_session(&retained).unwrap();
        store.archive_and_delete_session(&expired).unwrap();
        store.archive_and_delete_session(&retained).unwrap();
        store
            .conn
            .execute(
                "update deleted_agent_sessions set deleted_at = ?2 where id = ?1",
                params!["expired", (now - Duration::days(31)).to_rfc3339()],
            )
            .unwrap();

        let purged = store
            .purge_deleted_sessions_older_than(now - Duration::days(30))
            .unwrap();

        assert_eq!(purged.len(), 1);
        assert_eq!(purged[0].session.id, "expired");
        assert_eq!(
            store.load_deleted_sessions().unwrap()[0].session.id,
            "retained"
        );
    }

    #[test]
    fn upsert_without_changing_updated_at_preserves_order() {
        let store = test_store();
        let now = Utc::now();

        let s1 = test_session("a", now - Duration::hours(2), now - Duration::hours(2));
        let s2 = test_session("b", now - Duration::hours(1), now - Duration::hours(1));

        store.upsert_session(&s1).unwrap();
        store.upsert_session(&s2).unwrap();

        // Re-upsert s1 with same timestamps (simulating a no-op status update).
        store.upsert_session(&s1).unwrap();

        let loaded = store.load_sessions().unwrap();
        let ids: Vec<&str> = loaded.iter().map(|s| s.id.as_str()).collect();

        // Order unchanged: s2 still has the more recent updated_at.
        assert_eq!(ids, vec!["b", "a"]);
    }

    #[test]
    fn related_group_fields_round_trip_and_empty_groups_are_purged() {
        let mut store = test_store();
        let now = Utc::now();
        let group =
            crate::model::AgentGroup::new("origin agent".to_string(), Some("origin".to_string()));
        store.upsert_group(&group).unwrap();

        let mut origin = test_session("origin", now, now);
        origin.group_id = Some(group.id.clone());
        let mut child = test_session("child", now, now);
        child.id = "child".to_string();
        child.group_id = Some(group.id.clone());
        child.parent_session_id = Some("origin".to_string());
        store.upsert_session(&origin).unwrap();
        store.upsert_session(&child).unwrap();

        let loaded = store.load_sessions().unwrap();
        let loaded_child = loaded.iter().find(|session| session.id == "child").unwrap();
        assert_eq!(loaded_child.group_id.as_deref(), Some(group.id.as_str()));
        assert_eq!(loaded_child.parent_session_id.as_deref(), Some("origin"));

        let groups = store.load_groups().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, "origin agent");

        store.purge_empty_groups().unwrap();
        assert_eq!(store.load_groups().unwrap().len(), 1);

        store.permanently_delete_session("origin").unwrap();
        store.permanently_delete_session("child").unwrap();
        store.purge_empty_groups().unwrap();
        assert!(store.load_groups().unwrap().is_empty());
    }

    #[test]
    fn archived_sessions_keep_their_related_group() {
        let mut store = test_store();
        let now = Utc::now();
        let group = crate::model::AgentGroup::new("origin".to_string(), Some("origin".to_string()));
        store.upsert_group(&group).unwrap();
        let mut session = test_session("origin", now, now);
        session.group_id = Some(group.id.clone());
        store.upsert_session(&session).unwrap();

        store.archive_and_delete_session(&session).unwrap();
        let deleted = store.load_deleted_sessions().unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(
            deleted[0].session.group_id.as_deref(),
            Some(group.id.as_str())
        );

        let restored = store.restore_deleted_session("origin").unwrap().unwrap();
        assert_eq!(restored.group_id.as_deref(), Some(group.id.as_str()));
    }

    #[test]
    fn started_providers_round_trip() {
        let store = test_store();
        let now = Utc::now();
        let mut session = test_session("started", now, now);
        session.started_providers = vec!["claude".to_string(), "codex".to_string()];

        store.upsert_session(&session).unwrap();

        let loaded = store.load_sessions().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].started_providers,
            vec!["claude".to_string(), "codex".to_string()]
        );
    }

    #[test]
    fn provider_session_ids_round_trip() {
        let store = test_store();
        let now = Utc::now();
        let mut session = test_session("provider-session", now, now);
        session.provider_session_ids = BTreeMap::from([
            ("opencode".to_string(), "ses_opencode".to_string()),
            ("codex".to_string(), "codex-session".to_string()),
        ]);

        store.upsert_session(&session).unwrap();

        let loaded = store.load_sessions().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].provider_session_ids, session.provider_session_ids);
    }

    #[test]
    fn projects_round_trip_all_project_fields() {
        let store = test_store();
        let project = ProjectConfig {
            id: "project-1".to_string(),
            path: "$CODE/dux".to_string(),
            name: Some("dux".to_string()),
            default_provider: Some("codex".to_string()),
            leading_branch: Some("main".to_string()),
            auto_reopen_agents: Some(false),
            startup_command: Some("npm install".to_string()),
            env: BTreeMap::from([("EDITOR".to_string(), "true".to_string())]),
        };

        store.upsert_project(&project).unwrap();

        let loaded = store.load_projects().unwrap();
        assert_eq!(loaded, vec![project]);
    }

    #[test]
    fn project_path_conflict_keeps_existing_id() {
        let store = test_store();
        store
            .upsert_project(&ProjectConfig {
                id: "stable-id".to_string(),
                path: "/repo".to_string(),
                name: Some("old".to_string()),
                default_provider: None,
                leading_branch: Some("main".to_string()),
                auto_reopen_agents: None,
                startup_command: None,
                env: Default::default(),
            })
            .unwrap();

        store
            .upsert_project(&ProjectConfig {
                id: "new-id".to_string(),
                path: "/repo".to_string(),
                name: Some("new".to_string()),
                default_provider: Some("claude".to_string()),
                leading_branch: Some("trunk".to_string()),
                auto_reopen_agents: Some(false),
                startup_command: Some("echo setup".to_string()),
                env: BTreeMap::from([("API_KEY".to_string(), "${FOO_API_KEY}".to_string())]),
            })
            .unwrap();

        let loaded = store.load_projects().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "stable-id");
        assert_eq!(loaded[0].name.as_deref(), Some("new"));
        assert_eq!(loaded[0].default_provider.as_deref(), Some("claude"));
        assert_eq!(loaded[0].leading_branch.as_deref(), Some("trunk"));
        assert_eq!(loaded[0].auto_reopen_agents, Some(false));
        assert_eq!(loaded[0].startup_command.as_deref(), Some("echo setup"));
        assert_eq!(
            loaded[0].env.get("API_KEY").map(String::as_str),
            Some("${FOO_API_KEY}")
        );
    }

    #[test]
    fn auto_reopen_fields_round_trip() {
        let store = test_store();
        let now = Utc::now();
        let mut session = test_session("auto", now, now);
        session.desired_running = true;
        session.auto_reopen_enabled = false;

        store.upsert_session(&session).unwrap();

        let loaded = store.load_sessions().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].desired_running);
        assert!(!loaded[0].auto_reopen_enabled);

        store.set_auto_reopen_enabled("auto", true).unwrap();
        store.set_desired_running("auto", false).unwrap();
        let loaded = store.load_sessions().unwrap();
        assert!(!loaded[0].desired_running);
        assert!(loaded[0].auto_reopen_enabled);
    }

    #[test]
    fn auto_reopen_fields_migrate_from_old_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            create table agent_sessions (
                id text primary key,
                project_id text not null,
                provider text not null,
                source_branch text not null,
                branch_name text not null,
                worktree_path text not null,
                title text,
                project_path text,
                status text not null,
                created_at text not null,
                updated_at text not null
            );
            insert into agent_sessions (
                id, project_id, provider, source_branch, branch_name,
                worktree_path, title, project_path, status, created_at, updated_at
            ) values (
                'old', 'proj', 'claude', 'main', 'agent', '/tmp/agent',
                null, null, 'detached', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
            );
            "#,
        )
        .unwrap();

        let store = SessionStore { conn };
        store.migrate().unwrap();

        let loaded = store.load_sessions().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(!loaded[0].desired_running);
        assert!(loaded[0].auto_reopen_enabled);
        assert!(loaded[0].provider_session_ids.is_empty());
        assert!(loaded[0].group_id.is_none());
        assert!(loaded[0].parent_session_id.is_none());
        assert!(store.load_groups().unwrap().is_empty());
    }
}

#[cfg(test)]
mod pr_tests {
    use super::*;
    use crate::model::{PrKey, PrLinkSource, PrRelationSource};
    use chrono::Duration;

    fn spr(
        num: u64,
        host: &str,
        repo: &str,
        state: &str,
        head: &str,
        base: &str,
    ) -> StoredPullRequest {
        StoredPullRequest {
            host: host.to_string(),
            owner_repo: repo.to_string(),
            number: num,
            state: state.to_string(),
            is_draft: false,
            title: format!("PR {num}"),
            url: fallback_pr_url(host, repo, num),
            head_ref: head.to_string(),
            base_ref: base.to_string(),
            head_repo: String::new(),
        }
    }

    #[test]
    fn upsert_and_load_pull_requests() {
        let store = test_store();
        store
            .upsert_pull_request(&spr(10, "github.com", "owner/repo", "OPEN", "b10", "main"))
            .unwrap();
        store
            .upsert_pull_request(&spr(
                20,
                "github.com",
                "owner/repo",
                "MERGED",
                "b20",
                "main",
            ))
            .unwrap();

        let mut prs = store.load_pull_requests().unwrap();
        prs.sort_by_key(|pr| pr.number);
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].number, 10);
        assert_eq!(prs[0].head_ref, "b10");
        assert_eq!(prs[1].state, "MERGED");
    }

    #[test]
    fn upsert_pull_request_updates_fields() {
        let store = test_store();
        store
            .upsert_pull_request(&spr(42, "github.com", "owner/repo", "OPEN", "b", "main"))
            .unwrap();
        let mut updated = spr(42, "GitHub.com", "owner/repo", "MERGED", "b", "release");
        updated.title = "Updated title".to_string();
        store.upsert_pull_request(&updated).unwrap();

        let prs = store.load_pull_requests().unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].state, "MERGED");
        assert_eq!(prs[0].base_ref, "release");
        assert_eq!(prs[0].title, "Updated title");
    }

    #[test]
    fn link_source_does_not_downgrade() {
        let store = test_store();
        let key = PrKey::new("github.com", "owner/repo", 1);
        store
            .link_pull_request("s1", &key, PrLinkSource::Manual)
            .unwrap();
        store
            .link_pull_request("s1", &key, PrLinkSource::Discovered)
            .unwrap();
        let links = store.load_pr_links().unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].source, PrLinkSource::Manual);

        let created = PrKey::new("github.com", "owner/repo", 2);
        store
            .link_pull_request("s1", &created, PrLinkSource::Created)
            .unwrap();
        store
            .link_pull_request("s1", &created, PrLinkSource::Imported)
            .unwrap();
        let links = store.load_pr_links().unwrap();
        let created_link = links.iter().find(|link| link.key == created).unwrap();
        assert_eq!(created_link.source, PrLinkSource::Created);
    }

    #[test]
    fn manual_stack_edge_survives_detected_upsert() {
        let store = test_store();
        let child = PrKey::new("github.com", "owner/repo", 2);
        let parent = PrKey::new("github.com", "owner/repo", 1);
        let other = PrKey::new("github.com", "owner/repo", 3);
        store
            .upsert_pr_stack(&child, &parent, PrRelationSource::Manual)
            .unwrap();
        store
            .upsert_pr_stack(&child, &other, PrRelationSource::Detected)
            .unwrap();
        let stacks = store.load_pr_stacks().unwrap();
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].parent, parent);
        assert_eq!(stacks[0].source, PrRelationSource::Manual);
    }

    #[test]
    fn legacy_session_prs_are_migrated() {
        let store = test_store();
        let now = Utc::now();
        store.upsert_session(&test_session("s1", now, now)).unwrap();
        store
            .conn
            .execute(
                r#"
                insert into session_prs (session_id, pr_number, host, owner_repo, state, title, url)
                values ('s1', 7, 'GitHub.com', 'Owner/Repo', 'OPEN', 'Legacy', '')
                "#,
                [],
            )
            .unwrap();
        store.migrate().unwrap();

        let prs = store.load_pull_requests().unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].host, "github.com");
        assert_eq!(prs[0].owner_repo, "owner/repo");
        let links = store.load_pr_links().unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].session_id, "s1");
    }

    #[test]
    fn archive_preserves_links_and_permanent_delete_removes_them() {
        let store = test_store();
        let now = Utc::now();
        let session = test_session("s1", now, now);
        store.upsert_session(&session).unwrap();
        let key = PrKey::new("github.com", "owner/repo", 1);
        store
            .link_pull_request("s1", &key, PrLinkSource::Discovered)
            .unwrap();

        let mut store = store;
        store.archive_and_delete_session(&session).unwrap();
        assert_eq!(store.load_pr_links().unwrap().len(), 1);
        store.permanently_delete_session("s1").unwrap();
        assert!(store.load_pr_links().unwrap().is_empty());
    }

    #[test]
    fn permanent_delete_prunes_orphaned_prs_but_keeps_stack_parents() {
        let store = test_store();
        let now = Utc::now();
        let session = test_session("s1", now, now);
        store.upsert_session(&session).unwrap();
        let child = PrKey::new("github.com", "owner/repo", 2);
        let parent = PrKey::new("github.com", "owner/repo", 1);
        store
            .upsert_pull_request(&spr(1, "github.com", "owner/repo", "OPEN", "b1", "main"))
            .unwrap();
        store
            .upsert_pull_request(&spr(2, "github.com", "owner/repo", "OPEN", "b2", "b1"))
            .unwrap();
        store
            .link_pull_request("s1", &child, PrLinkSource::Discovered)
            .unwrap();
        store
            .upsert_pr_stack(&child, &parent, PrRelationSource::Detected)
            .unwrap();

        let mut store = store;
        store.permanently_delete_session("s1").unwrap();
        let prs = store.load_pull_requests().unwrap();
        assert_eq!(
            prs.len(),
            2,
            "a child with no links but a stack edge must not be pruned"
        );

        let lone = PrKey::new("github.com", "owner/repo", 9);
        store
            .upsert_pull_request(&spr(9, "github.com", "owner/repo", "OPEN", "b9", "main"))
            .unwrap();
        // Link the lone PR to a live session, then delete that session to
        // exercise the orphan-pruning path.
        let session2 = test_session("s2", now, now);
        store.upsert_session(&session2).unwrap();
        store
            .link_pull_request("s2", &lone, PrLinkSource::Discovered)
            .unwrap();
        store.permanently_delete_session("s2").unwrap();
        assert!(
            !store
                .load_pull_requests()
                .unwrap()
                .iter()
                .any(|pr| pr.number == 9),
            "an unlinked PR outside any stack should be pruned"
        );
    }

    #[test]
    fn purging_archived_sessions_removes_their_links() {
        let store = test_store();
        let now = Utc::now() - Duration::days(40);
        let session = test_session("s1", now, now);
        store.upsert_session(&session).unwrap();
        let key = PrKey::new("github.com", "owner/repo", 1);
        store
            .link_pull_request("s1", &key, PrLinkSource::Discovered)
            .unwrap();
        let mut store = store;
        store.archive_and_delete_session(&session).unwrap();
        store
            .purge_deleted_sessions_older_than(Utc::now() + Duration::days(1))
            .unwrap();
        assert!(store.load_pr_links().unwrap().is_empty());
    }
}

fn ensure_column(conn: &Connection, table: &str, column: &str, sql_type: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("pragma table_info({table})"))?;
    let existing = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if existing.iter().any(|name| name == column) {
        return Ok(());
    }
    conn.execute(
        &format!("alter table {table} add column {column} {sql_type}"),
        [],
    )?;
    Ok(())
}
