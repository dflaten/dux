//! One-time migration from the legacy dux config directory to yvo.
//!
//! The rename from dux to yvo changes the default user-state directory,
//! runtime file names, the scratch project id, and the theme/log settings in
//! `config.toml`. Existing installs keep their data under the old directory,
//! so this module moves that directory into place and rewrites the paths
//! stored in SQLite plus the git worktree admin files.
//!
//! The migration is deliberately explicit: `yvo config migrate` previews the
//! move and `yvo config migrate --yes` performs it. Startup refuses to create
//! a fresh yvo workspace while legacy dux data is still waiting, so users
//! cannot accidentally split their sessions across two directories.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::config::{self, YvoPaths};
use crate::lockfile::{AcquireError, SingleInstanceLock};

const LEGACY_SCRATCH_PROJECT_ID: &str = "__dux_scratch__";
const SCRATCH_PROJECT_ID: &str = "__yvo_scratch__";

/// A validated plan for moving the legacy dux directory into the yvo location.
#[derive(Clone, Debug)]
pub struct MigrationPlan {
    pub old_root: PathBuf,
    pub new_root: PathBuf,
}

/// Counts and warnings produced by [`execute`].
#[derive(Debug, Default)]
pub struct MigrationSummary {
    pub session_paths: usize,
    pub delegation_paths: usize,
    pub project_paths: usize,
    pub scratch_rows: usize,
    pub config_updated: bool,
    pub worktrees_repaired: usize,
    pub warnings: Vec<String>,
}

/// Build a plan, or return `None` when there is no legacy data to migrate.
///
/// This function only reads. It refuses to plan a move that would be unsafe or
/// ambiguous: the destination already holding state, or the current shell
/// sitting inside the source directory.
pub fn plan(paths: &YvoPaths) -> Result<Option<MigrationPlan>> {
    let old_root = config::legacy_root()?;
    let new_root = paths.root.clone();

    if old_root == new_root {
        return Ok(None);
    }
    if !config::has_legacy_state(&old_root) {
        return Ok(None);
    }
    if current_dir_is_inside(&old_root) {
        bail!(
            "refusing to move {} while the current directory is inside it. Run \
             `yvo config migrate` from a directory outside the legacy dux tree.",
            old_root.display()
        );
    }
    if new_root.exists() && !dir_is_empty(&new_root)? {
        bail!(
            "{} already exists and is not empty. Move it aside or remove it before \
             running the migration.",
            new_root.display()
        );
    }

    validate_legacy_config(&old_root)?;

    Ok(Some(MigrationPlan { old_root, new_root }))
}

/// Move the legacy directory into place and rewrite everything that stores a
/// path or the old scratch project id.
pub fn execute(plan: MigrationPlan) -> Result<MigrationSummary> {
    // Hold the legacy single-instance lock for the whole move. A stale
    // lockfile is harmless; a live dux process is not, and must be closed
    // before its data directory changes underneath it.
    let legacy_lock_path = plan.old_root.join("dux.lock");
    let _lock = SingleInstanceLock::acquire(&legacy_lock_path).map_err(|err| match err {
        AcquireError::AlreadyRunning { pid, .. } => match pid {
            Some(pid) => anyhow::anyhow!(
                "another dux instance is still running (PID {pid}). Close it before migrating."
            ),
            None => {
                anyhow::anyhow!("another dux instance is still running. Close it before migrating.")
            }
        },
        AcquireError::Io { path, err } => {
            anyhow::anyhow!("failed to lock {}: {err}", path.display())
        }
    })?;

    if plan.new_root.exists() {
        fs::remove_dir(&plan.new_root).with_context(|| {
            format!(
                "failed to remove empty destination {}",
                plan.new_root.display()
            )
        })?;
    }

    fs::rename(&plan.old_root, &plan.new_root).with_context(|| {
        format!(
            "failed to move {} to {}. If they are on different filesystems, move the \
             directory manually and then run the remaining steps from the new location.",
            plan.old_root.display(),
            plan.new_root.display()
        )
    })?;

    let new_root = &plan.new_root;
    rename_if_exists(&new_root.join("dux.lock"), &new_root.join("yvo.lock"))?;
    rename_if_exists(&new_root.join("dux.sock"), &new_root.join("yvo.sock"))?;
    rename_if_exists(&new_root.join("dux.log"), &new_root.join("yvo.log"))?;

    let mut summary = MigrationSummary::default();

    let db_path = new_root.join("sessions.sqlite3");
    if db_path.exists() {
        let db_summary = rewrite_database(&db_path, &plan.old_root, &plan.new_root)?;
        summary.session_paths = db_summary.session_paths;
        summary.delegation_paths = db_summary.delegation_paths;
        summary.project_paths = db_summary.project_paths;
        summary.scratch_rows = db_summary.scratch_rows;
    }

    // Repair every worktree under the moved root, not only the ones that still
    // have sessions. Old branches and archived worktrees are user data too.
    let (repaired, warnings) = repair_worktrees(&plan.old_root, &plan.new_root);
    summary.worktrees_repaired = repaired;
    summary.warnings.extend(warnings);

    let config_path = new_root.join("config.toml");
    if config_path.exists() {
        summary.config_updated = rewrite_config(&config_path, &plan.old_root, &plan.new_root)?;
    }

    Ok(summary)
}

fn validate_legacy_config(old_root: &Path) -> Result<()> {
    let config_path = old_root.join("config.toml");
    if !config_path.exists() {
        return Ok(());
    }
    let raw = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    raw.parse::<toml_edit::DocumentMut>().with_context(|| {
        format!(
            "{} is not valid TOML; fix it before migrating",
            config_path.display()
        )
    })?;
    Ok(())
}

fn current_dir_is_inside(old_root: &Path) -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    let canonical_cwd = cwd.canonicalize().unwrap_or(cwd);
    let canonical_old = old_root
        .canonicalize()
        .unwrap_or_else(|_| old_root.to_path_buf());
    canonical_cwd.starts_with(canonical_old)
}

fn dir_is_empty(path: &Path) -> Result<bool> {
    let mut entries =
        fs::read_dir(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    Ok(entries.next().is_none())
}

fn rename_if_exists(from: &Path, to: &Path) -> Result<()> {
    if !from.exists() {
        return Ok(());
    }
    if to.exists() {
        bail!(
            "cannot rename {} to {}: destination already exists",
            from.display(),
            to.display()
        );
    }
    fs::rename(from, to)
        .with_context(|| format!("failed to rename {} to {}", from.display(), to.display()))
}

#[derive(Debug, Default)]
struct DbSummary {
    session_paths: usize,
    delegation_paths: usize,
    project_paths: usize,
    scratch_rows: usize,
}

fn rewrite_database(db_path: &Path, old_root: &Path, new_root: &Path) -> Result<DbSummary> {
    let mut conn = Connection::open(db_path)
        .with_context(|| format!("failed to open {}", db_path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    let tx = conn.transaction()?;

    let old_prefix = path_string(old_root);
    let new_prefix = path_string(new_root);
    let mut summary = DbSummary::default();

    // (table, key column, path column, counter)
    let path_columns: &[(&str, &str, &str)] = &[
        ("agent_sessions", "worktree_path", "session"),
        ("agent_sessions", "project_path", "session"),
        ("deleted_agent_sessions", "worktree_path", "session"),
        ("deleted_agent_sessions", "project_path", "session"),
        ("agent_delegations", "brief_path", "delegation"),
        ("agent_delegations", "repo_path", "delegation"),
        ("agent_delegations", "result_path", "delegation"),
        ("projects", "path", "project"),
    ];

    for (table, column, counter) in path_columns {
        if !table_exists(&tx, table)? || !column_exists(&tx, table, column)? {
            continue;
        }
        let changed = rewrite_table_column(&tx, table, column, &old_prefix, &new_prefix)?;
        match *counter {
            "session" => summary.session_paths += changed,
            "delegation" => summary.delegation_paths += changed,
            "project" => summary.project_paths += changed,
            _ => {}
        }
    }

    let scratch_columns: &[(&str, &str)] = &[
        ("agent_sessions", "project_id"),
        ("deleted_agent_sessions", "project_id"),
        ("agent_delegations", "project_id"),
        ("projects", "id"),
    ];
    for (table, column) in scratch_columns {
        if !table_exists(&tx, table)? || !column_exists(&tx, table, column)? {
            continue;
        }
        let sql = format!("update {table} set {column} = ?1 where {column} = ?2");
        summary.scratch_rows +=
            tx.execute(&sql, params![SCRATCH_PROJECT_ID, LEGACY_SCRATCH_PROJECT_ID])?;
    }

    tx.commit()?;
    Ok(summary)
}

fn rewrite_table_column(
    conn: &Connection,
    table: &str,
    column: &str,
    old_prefix: &str,
    new_prefix: &str,
) -> Result<usize> {
    // The legacy tables all use a text `id` primary key.
    let select = format!("select id, {column} from {table} where {column} is not null");
    let mut stmt = conn.prepare(&select)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    let update = format!("update {table} set {column} = ?1 where id = ?2");
    let mut changed = 0;
    for (id, value) in rows {
        if let Some(rewritten) = rewrite_path_prefix(&value, old_prefix, new_prefix) {
            conn.execute(&update, params![rewritten, id])?;
            changed += 1;
        }
    }
    Ok(changed)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row(
            "select 1 from sqlite_master where type = 'table' and name = ?1",
            [table],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let sql = format!("pragma table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn path_string(path: &Path) -> String {
    let mut value = path.to_string_lossy().into_owned();
    while value.len() > 1 && value.ends_with('/') {
        value.pop();
    }
    value
}

/// Rewrite `value` when it is `old_prefix` itself or lives underneath it.
///
/// Prefix matching deliberately checks the path boundary, so a sibling such as
/// `/home/me/.config/dux-old` is not mistaken for a child of
/// `/home/me/.config/dux`.
fn rewrite_path_prefix(value: &str, old_prefix: &str, new_prefix: &str) -> Option<String> {
    if value == old_prefix {
        return Some(new_prefix.to_string());
    }
    let child_prefix = format!("{old_prefix}/");
    value
        .strip_prefix(&child_prefix)
        .map(|rest| format!("{new_prefix}/{rest}"))
}

fn rewrite_config(path: &Path, old_root: &Path, new_root: &Path) -> Result<bool> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .with_context(|| format!("failed to parse {}", path.display()))?;

    let old_prefix = path_string(old_root);
    let new_prefix = path_string(new_root);
    let mut changed = false;

    if let Some(ui) = doc.get_mut("ui").and_then(|item| item.as_table_mut())
        && let Some(theme) = ui.get_mut("theme").and_then(|item| item.as_value_mut())
        && matches!(theme.as_str(), Some("dux_dark") | Some("dux-dark"))
    {
        *theme = toml_edit::Value::from("yvo_dark");
        changed = true;
    }

    if let Some(logging) = doc.get_mut("logging").and_then(|item| item.as_table_mut())
        && let Some(log_path) = logging.get_mut("path").and_then(|item| item.as_value_mut())
    {
        if log_path.as_str() == Some("dux.log") {
            *log_path = toml_edit::Value::from("yvo.log");
            changed = true;
        } else if let Some(current) = log_path.as_str()
            && let Some(rewritten) = rewrite_path_prefix(current, &old_prefix, &new_prefix)
        {
            let rewritten = if Path::new(&rewritten)
                .file_name()
                .and_then(|name| name.to_str())
                == Some("dux.log")
            {
                Path::new(&rewritten)
                    .with_file_name("yvo.log")
                    .to_string_lossy()
                    .into_owned()
            } else {
                rewritten
            };
            *log_path = toml_edit::Value::from(rewritten);
            changed = true;
        }
    }

    if let Some(projects) = doc
        .get_mut("projects")
        .and_then(|item| item.as_array_of_tables_mut())
    {
        for project in projects.iter_mut() {
            if let Some(path_value) = project.get_mut("path").and_then(|item| item.as_value_mut())
                && rewrite_string_value(path_value, &old_prefix, &new_prefix)
            {
                changed = true;
            }
        }
    }

    let rendered = doc.to_string();
    let with_comments = rewrite_comment_lines(&rendered);
    if with_comments != raw {
        fs::write(path, with_comments)
            .with_context(|| format!("failed to write {}", path.display()))?;
        return Ok(true);
    }
    if changed {
        fs::write(path, rendered).with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(changed)
}

fn rewrite_string_value(value: &mut toml_edit::Value, old_prefix: &str, new_prefix: &str) -> bool {
    let Some(current) = value.as_str() else {
        return false;
    };
    let Some(rewritten) = rewrite_path_prefix(current, old_prefix, new_prefix) else {
        return false;
    };
    *value = toml_edit::Value::from(rewritten);
    true
}

/// Rewrite old product names only on comment lines. User data such as project
/// names, startup commands, and environment values is left alone; only paths
/// that moved with the state directory are rewritten.
fn rewrite_comment_lines(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for line in raw.split_inclusive('\n') {
        let (body, newline) = line
            .strip_suffix('\n')
            .map_or((line, ""), |body| (body, "\n"));
        if body.trim_start().starts_with('#') {
            out.push_str(
                &body
                    .replace("dux", "yvo")
                    .replace("Dux", "Yvo")
                    .replace("DUX", "YVO"),
            );
        } else {
            out.push_str(body);
        }
        out.push_str(newline);
    }
    out
}

/// Repair git worktree admin files for every worktree under the moved root.
///
/// Worktrees store an absolute `gitdir` pointer, so a directory move breaks
/// the link even when the project repository itself did not move. Walking the
/// worktree tree (rather than only session rows) also repairs archived and
/// unreferenced branches, which are still user data.
///
/// Failures are reported as warnings: the data has already moved, so aborting
/// would leave the user with a half-finished migration.
fn repair_worktrees(old_root: &Path, new_root: &Path) -> (usize, Vec<String>) {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    collect_moved_worktrees(
        &new_root.join("worktrees"),
        old_root,
        new_root,
        0,
        &mut grouped,
    );
    for worktrees in grouped.values_mut() {
        worktrees.sort();
        worktrees.dedup();
    }

    let mut repaired = 0;
    let mut warnings = Vec::new();
    for (project, worktrees) in grouped {
        if !Path::new(&project).exists() {
            warnings.push(format!(
                "skipped worktree repair for missing project {} ({} worktree(s))",
                project,
                worktrees.len()
            ));
            continue;
        }
        let output = Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(["worktree", "repair"])
            .args(&worktrees)
            .output();
        match output {
            Ok(output) if output.status.success() => repaired += worktrees.len(),
            Ok(output) => warnings.push(format!(
                "git worktree repair failed in {}: {}",
                project,
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(err) => warnings.push(format!(
                "could not run git worktree repair in {}: {err}",
                project
            )),
        }
    }
    (repaired, warnings)
}

fn collect_moved_worktrees(
    dir: &Path,
    old_root: &Path,
    new_root: &Path,
    depth: usize,
    grouped: &mut BTreeMap<String, Vec<String>>,
) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let git_file = path.join(".git");
        if git_file.is_file() {
            if let Some(repo) = worktree_repo(&git_file, old_root, new_root) {
                grouped
                    .entry(repo)
                    .or_default()
                    .push(path.to_string_lossy().into_owned());
            }
            continue;
        }
        collect_moved_worktrees(&path, old_root, new_root, depth + 1, grouped);
    }
}

/// Read a worktree's `.git` file and return the project repository it belongs
/// to. A `gitdir` pointing inside the old root is rewritten to the new root so
/// project repositories that moved with the migration are still found.
fn worktree_repo(git_file: &Path, old_root: &Path, new_root: &Path) -> Option<String> {
    let text = fs::read_to_string(git_file).ok()?;
    let gitdir = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir: "))?
        .trim();
    let repo = gitdir.split("/.git/worktrees/").next()?;
    if !repo.starts_with('/') {
        return None;
    }
    let old_prefix = path_string(old_root);
    let new_prefix = path_string(new_root);
    Some(rewrite_path_prefix(repo, &old_prefix, &new_prefix).unwrap_or_else(|| repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep().join(name);
        fs::create_dir_all(&path).expect("create root");
        path
    }

    #[test]
    fn rewrites_exact_prefix_and_children_but_not_siblings() {
        let old = "/home/me/.config/dux";
        let new = "/home/me/.config/yvo";

        assert_eq!(
            rewrite_path_prefix("/home/me/.config/dux", old, new).as_deref(),
            Some("/home/me/.config/yvo")
        );
        assert_eq!(
            rewrite_path_prefix("/home/me/.config/dux/worktrees/a", old, new).as_deref(),
            Some("/home/me/.config/yvo/worktrees/a")
        );
        assert_eq!(
            rewrite_path_prefix("/home/me/.config/dux-old/worktrees/a", old, new),
            None
        );
        assert_eq!(rewrite_path_prefix("/home/me/projects/dux", old, new), None);
    }

    #[test]
    fn rewrites_config_values_and_comments_without_touching_project_data() {
        let root = temp_root("config");
        let path = root.join("config.toml");
        let old_root = Path::new("/home/me/.config/dux");
        let new_root = Path::new("/home/me/.config/yvo");
        fs::write(
            &path,
            r#"# dux configuration
[ui]
theme = "dux_dark"

[logging]
path = "dux.log"

[[projects]]
id = "00000000-0000-0000-0000-000000000000"
path = "$HOME/projects/dux"
name = "dux"

[[projects]]
id = "00000000-0000-0000-0000-000000000001"
path = "/home/me/.config/dux/local-project"
name = "local"
"#,
        )
        .expect("write config");

        assert!(rewrite_config(&path, old_root, new_root).expect("rewrite config"));
        let rendered = fs::read_to_string(&path).expect("read config");
        assert!(rendered.contains("# yvo configuration"));
        assert!(rendered.contains("theme = \"yvo_dark\""));
        assert!(rendered.contains("path = \"yvo.log\""));
        // Project data that happens to contain the old name must survive, but a
        // project path that moved with the state directory is rewritten.
        assert!(rendered.contains("path = \"$HOME/projects/dux\""));
        assert!(rendered.contains("name = \"dux\""));
        assert!(rendered.contains("path = \"/home/me/.config/yvo/local-project\""));
    }

    #[test]
    fn rewrites_database_paths_and_scratch_id() {
        let root = temp_root("db");
        let db_path = root.join("sessions.sqlite3");
        let old_root = Path::new("/home/me/.config/dux");
        let new_root = Path::new("/home/me/.config/yvo");
        let conn = Connection::open(&db_path).expect("open db");
        conn.execute_batch(
            r#"
            create table agent_sessions (
                id text primary key,
                project_id text,
                worktree_path text,
                project_path text
            );
            create table deleted_agent_sessions (
                id text primary key,
                project_id text,
                worktree_path text,
                project_path text
            );
            create table agent_delegations (
                id text primary key,
                project_id text,
                brief_path text,
                repo_path text,
                result_path text
            );
            create table projects (
                id text primary key,
                path text
            );
            insert into agent_sessions values
                ('s1', '__dux_scratch__', '/home/me/.config/dux/worktrees/p/a', '/home/me/projects/p'),
                ('s2', 'p1', '/home/me/.config/dux-old/worktrees/p/a', null);
            insert into deleted_agent_sessions values
                ('d1', 'p1', '/home/me/.config/dux/worktrees/p/old', null);
            insert into agent_delegations values
                ('g1', 'p1', '/home/me/.config/dux/worktrees/p/a/brief.md',
                 '/home/me/projects/p', '/home/me/.config/dux/worktrees/p/a/result.json');
            insert into projects values ('p1', '/home/me/projects/p');
            "#,
        )
        .expect("seed db");
        drop(conn);

        let summary = rewrite_database(&db_path, old_root, new_root).expect("rewrite db");
        // One active worktree plus one archived worktree moved under the old root.
        assert_eq!(summary.session_paths, 2);
        assert_eq!(summary.delegation_paths, 2);
        assert_eq!(summary.project_paths, 0);
        assert_eq!(summary.scratch_rows, 1);

        let conn = Connection::open(&db_path).expect("reopen db");
        let worktree: String = conn
            .query_row(
                "select worktree_path from agent_sessions where id = 's1'",
                [],
                |row| row.get(0),
            )
            .expect("worktree");
        assert_eq!(worktree, "/home/me/.config/yvo/worktrees/p/a");
        let scratch: String = conn
            .query_row(
                "select project_id from agent_sessions where id = 's1'",
                [],
                |row| row.get(0),
            )
            .expect("scratch id");
        assert_eq!(scratch, SCRATCH_PROJECT_ID);
        let sibling: String = conn
            .query_row(
                "select worktree_path from agent_sessions where id = 's2'",
                [],
                |row| row.get(0),
            )
            .expect("sibling");
        assert_eq!(sibling, "/home/me/.config/dux-old/worktrees/p/a");
    }

    #[test]
    fn discovers_worktrees_under_the_new_root_and_maps_moved_repositories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let old_root = temp.path().join("dux");
        let new_root = temp.path().join("yvo");
        let worktree = new_root.join("worktrees/project/branch");
        fs::create_dir_all(&worktree).expect("worktree");
        fs::write(
            worktree.join(".git"),
            format!(
                "gitdir: {}/repo/.git/worktrees/branch\n",
                old_root.display()
            ),
        )
        .expect("gitfile");

        let mut grouped = BTreeMap::new();
        collect_moved_worktrees(
            &new_root.join("worktrees"),
            &old_root,
            &new_root,
            0,
            &mut grouped,
        );

        let repo = new_root.join("repo").to_string_lossy().into_owned();
        assert_eq!(
            grouped.get(&repo),
            Some(&vec![worktree.to_string_lossy().into_owned()])
        );
    }

    #[test]
    fn execute_moves_directory_and_renames_runtime_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let old_root = temp.path().join("dux");
        let new_root = temp.path().join("yvo");
        fs::create_dir_all(&old_root).expect("old root");
        fs::write(
            old_root.join("config.toml"),
            "# dux configuration\n[ui]\ntheme = \"dux_dark\"\n",
        )
        .expect("config");
        fs::write(old_root.join("dux.log"), "log\n").expect("log");
        fs::write(old_root.join("dux.lock"), "").expect("lock");

        let plan = MigrationPlan {
            old_root: old_root.clone(),
            new_root: new_root.clone(),
        };
        let summary = execute(plan).expect("migrate");

        assert!(!old_root.exists());
        assert!(new_root.join("config.toml").exists());
        assert!(new_root.join("yvo.log").exists());
        assert!(new_root.join("yvo.lock").exists());
        assert!(summary.config_updated);
    }
}
