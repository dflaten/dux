use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use rusqlite::Connection;
use serde_json::Value;

pub(crate) fn data_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(xdg_data_home) = std::env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(xdg_data_home).join("opencode"));
    }
    if let Some(home) = home::home_dir() {
        roots.push(home.join(".local/share/opencode"));
        roots.push(home.join(".opencode"));
    }
    roots
}

pub(crate) fn latest_session_id_for_worktree(
    worktree_path: &Path,
    ignored_session_ids: &HashSet<String>,
) -> Option<String> {
    latest_session_id_for_worktree_in_roots(worktree_path, ignored_session_ids, &data_roots())
}

pub(crate) fn latest_session_id_for_worktree_in_roots(
    worktree_path: &Path,
    ignored_session_ids: &HashSet<String>,
    roots: &[PathBuf],
) -> Option<String> {
    let mut best: Option<OpenCodeSessionCandidate> = None;
    for root in roots {
        if let Some(candidate) = latest_sqlite_session_id(root, worktree_path, ignored_session_ids)
            .ok()
            .flatten()
        {
            keep_newest(&mut best, candidate);
        }
        if let Some(candidate) = latest_json_session_id(root, worktree_path, ignored_session_ids) {
            keep_newest(&mut best, candidate);
        }
    }
    best.map(|candidate| candidate.id)
}

pub(crate) fn session_ids_for_worktree(worktree_path: &Path) -> HashSet<String> {
    session_ids_for_worktree_in_roots(worktree_path, &data_roots())
}

pub(crate) fn session_ids_for_worktree_in_roots(
    worktree_path: &Path,
    roots: &[PathBuf],
) -> HashSet<String> {
    let mut ids = HashSet::new();
    for root in roots {
        ids.extend(sqlite_session_ids(root, worktree_path).unwrap_or_default());
        ids.extend(json_session_ids(root, worktree_path));
    }
    ids
}

#[derive(Debug)]
struct OpenCodeSessionCandidate {
    id: String,
    updated: SystemTime,
}

fn keep_newest(best: &mut Option<OpenCodeSessionCandidate>, candidate: OpenCodeSessionCandidate) {
    if best
        .as_ref()
        .is_none_or(|current| candidate.updated > current.updated)
    {
        *best = Some(candidate);
    }
}

fn latest_sqlite_session_id(
    root: &Path,
    worktree_path: &Path,
    ignored_session_ids: &HashSet<String>,
) -> Result<Option<OpenCodeSessionCandidate>> {
    let db_path = root.join("opencode.db");
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    for order_column in [
        "time_updated",
        "updated_at",
        "time_created",
        "created_at",
        "rowid",
    ] {
        let query =
            format!("select id, directory from session order by {order_column} desc limit 100");
        let mut stmt = match conn.prepare(&query) {
            Ok(stmt) => stmt,
            Err(_) => continue,
        };
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, directory) = row?;
            if ignored_session_ids.contains(&id) || !same_path(&directory, worktree_path) {
                continue;
            }
            let updated = fs::metadata(root.join("opencode.db"))
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            return Ok(Some(OpenCodeSessionCandidate { id, updated }));
        }
    }
    Ok(None)
}

fn sqlite_session_ids(root: &Path, worktree_path: &Path) -> Result<HashSet<String>> {
    let db_path = root.join("opencode.db");
    if !db_path.exists() {
        return Ok(HashSet::new());
    }
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = match conn.prepare("select id, directory from session") {
        Ok(stmt) => stmt,
        Err(_) => return Ok(HashSet::new()),
    };
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut ids = HashSet::new();
    for row in rows {
        let (id, directory) = row?;
        if same_path(&directory, worktree_path) {
            ids.insert(id);
        }
    }
    Ok(ids)
}

fn latest_json_session_id(
    root: &Path,
    worktree_path: &Path,
    ignored_session_ids: &HashSet<String>,
) -> Option<OpenCodeSessionCandidate> {
    let mut best = None;
    for session in json_sessions(root) {
        if ignored_session_ids.contains(&session.id)
            || !same_path(&session.directory, worktree_path)
        {
            continue;
        }
        keep_newest(
            &mut best,
            OpenCodeSessionCandidate {
                id: session.id,
                updated: session.updated,
            },
        );
    }
    best
}

fn json_session_ids(root: &Path, worktree_path: &Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    for session in json_sessions(root) {
        if same_path(&session.directory, worktree_path) {
            ids.insert(session.id);
        }
    }
    ids
}

struct JsonSession {
    id: String,
    directory: String,
    updated: SystemTime,
}

fn json_sessions(root: &Path) -> Vec<JsonSession> {
    let session_root = root.join("storage/session");
    let mut sessions = Vec::new();
    let Ok(project_dirs) = fs::read_dir(session_root) else {
        return sessions;
    };
    for project_dir in project_dirs {
        let Ok(project_dir) = project_dir else {
            continue;
        };
        let Ok(file_type) = project_dir.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(project_dir.path()) else {
            continue;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(value) = fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            else {
                continue;
            };
            let Some(id) = value.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(directory) = value
                .get("directory")
                .or_else(|| value.get("path"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let updated = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            sessions.push(JsonSession {
                id: id.to_string(),
                directory: directory.to_string(),
                updated,
            });
        }
    }
    sessions
}

fn same_path(stored: &str, worktree_path: &Path) -> bool {
    let stored_path = PathBuf::from(stored);
    if stored_path == worktree_path {
        return true;
    }
    let stored_canonical = stored_path.canonicalize().unwrap_or(stored_path);
    let worktree_canonical = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());
    stored_canonical == worktree_canonical
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn latest_session_id_for_worktree_reads_json_sessions() {
        let tmp = tempdir().expect("tempdir");
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).expect("worktree");
        let session_dir = tmp.path().join("storage/session/project");
        fs::create_dir_all(&session_dir).expect("session dir");
        fs::write(
            session_dir.join("ses_old.json"),
            format!(r#"{{"id":"ses_old","directory":"{}"}}"#, worktree.display()),
        )
        .expect("old session");
        fs::write(
            session_dir.join("ses_new.json"),
            format!(r#"{{"id":"ses_new","directory":"{}"}}"#, worktree.display()),
        )
        .expect("new session");

        let ignored_session_ids = HashSet::from(["ses_old".to_string()]);
        let found = latest_session_id_for_worktree_in_roots(
            &worktree,
            &ignored_session_ids,
            &[tmp.path().to_path_buf()],
        );

        assert_eq!(found.as_deref(), Some("ses_new"));
    }

    #[test]
    fn latest_session_id_for_worktree_ignores_all_existing_json_sessions() {
        let tmp = tempdir().expect("tempdir");
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).expect("worktree");
        let session_dir = tmp.path().join("storage/session/project");
        fs::create_dir_all(&session_dir).expect("session dir");
        fs::write(
            session_dir.join("ses_old_one.json"),
            format!(
                r#"{{"id":"ses_old_one","directory":"{}"}}"#,
                worktree.display()
            ),
        )
        .expect("old session one");
        fs::write(
            session_dir.join("ses_old_two.json"),
            format!(
                r#"{{"id":"ses_old_two","directory":"{}"}}"#,
                worktree.display()
            ),
        )
        .expect("old session two");

        let ignored_session_ids =
            session_ids_for_worktree_in_roots(&worktree, &[tmp.path().to_path_buf()]);
        assert_eq!(ignored_session_ids.len(), 2);

        fs::write(
            session_dir.join("ses_new.json"),
            format!(r#"{{"id":"ses_new","directory":"{}"}}"#, worktree.display()),
        )
        .expect("new session");

        let found = latest_session_id_for_worktree_in_roots(
            &worktree,
            &ignored_session_ids,
            &[tmp.path().to_path_buf()],
        );

        assert_eq!(found.as_deref(), Some("ses_new"));
    }

    #[test]
    fn latest_session_id_for_worktree_reads_sqlite_sessions() {
        let tmp = tempdir().expect("tempdir");
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).expect("worktree");
        let conn = Connection::open(tmp.path().join("opencode.db")).expect("db");
        conn.execute_batch(
            r#"
            create table session (
                id text primary key,
                directory text not null,
                time_updated integer not null
            );
            "#,
        )
        .expect("schema");
        conn.execute(
            "insert into session (id, directory, time_updated) values (?1, ?2, ?3)",
            ("ses_old", worktree.to_string_lossy().as_ref(), 1_i64),
        )
        .expect("old session");
        conn.execute(
            "insert into session (id, directory, time_updated) values (?1, ?2, ?3)",
            ("ses_new", worktree.to_string_lossy().as_ref(), 2_i64),
        )
        .expect("new session");

        let ignored_session_ids = HashSet::from(["ses_old".to_string()]);
        let found = latest_session_id_for_worktree_in_roots(
            &worktree,
            &ignored_session_ids,
            &[tmp.path().to_path_buf()],
        );

        assert_eq!(found.as_deref(), Some("ses_new"));
    }

    #[test]
    fn latest_session_id_for_worktree_ignores_all_existing_sqlite_sessions() {
        let tmp = tempdir().expect("tempdir");
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).expect("worktree");
        let conn = Connection::open(tmp.path().join("opencode.db")).expect("db");
        conn.execute_batch(
            r#"
            create table session (
                id text primary key,
                directory text not null,
                time_updated integer not null
            );
            "#,
        )
        .expect("schema");
        conn.execute(
            "insert into session (id, directory, time_updated) values (?1, ?2, ?3)",
            ("ses_old_one", worktree.to_string_lossy().as_ref(), 1_i64),
        )
        .expect("old session one");
        conn.execute(
            "insert into session (id, directory, time_updated) values (?1, ?2, ?3)",
            ("ses_old_two", worktree.to_string_lossy().as_ref(), 2_i64),
        )
        .expect("old session two");

        let ignored_session_ids =
            session_ids_for_worktree_in_roots(&worktree, &[tmp.path().to_path_buf()]);
        assert_eq!(ignored_session_ids.len(), 2);

        conn.execute(
            "insert into session (id, directory, time_updated) values (?1, ?2, ?3)",
            ("ses_new", worktree.to_string_lossy().as_ref(), 3_i64),
        )
        .expect("new session");

        let found = latest_session_id_for_worktree_in_roots(
            &worktree,
            &ignored_session_ids,
            &[tmp.path().to_path_buf()],
        );

        assert_eq!(found.as_deref(), Some("ses_new"));
    }
}
