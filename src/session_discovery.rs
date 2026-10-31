use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::Connection;
use serde_json::Value;

use crate::config::{
    JsonSessionDiscoveryConfig, SessionDiscoveryConfig, SqliteSessionDiscoveryConfig, expand_path,
};

#[derive(Debug)]
struct SessionCandidate {
    id: String,
    updated: i64,
}

pub(crate) fn latest_session_id_for_worktree(
    config: &SessionDiscoveryConfig,
    worktree_path: &Path,
    ignored_session_ids: &HashSet<String>,
) -> Option<String> {
    candidates_for_worktree(config, worktree_path)
        .into_iter()
        .filter(|candidate| !ignored_session_ids.contains(&candidate.id))
        .max_by_key(|candidate| candidate.updated)
        .map(|candidate| candidate.id)
}

pub(crate) fn session_ids_for_worktree(
    config: &SessionDiscoveryConfig,
    worktree_path: &Path,
) -> HashSet<String> {
    candidates_for_worktree(config, worktree_path)
        .into_iter()
        .map(|candidate| candidate.id)
        .collect()
}

fn candidates_for_worktree(
    config: &SessionDiscoveryConfig,
    worktree_path: &Path,
) -> Vec<SessionCandidate> {
    match config {
        SessionDiscoveryConfig::Sqlite(config) => sqlite_candidates(config, worktree_path),
        SessionDiscoveryConfig::Json(config) => json_candidates(config, worktree_path),
        SessionDiscoveryConfig::Disabled => Vec::new(),
    }
}

fn sqlite_candidates(
    config: &SqliteSessionDiscoveryConfig,
    worktree_path: &Path,
) -> Vec<SessionCandidate> {
    let mut candidates = Vec::new();
    for database in &config.databases {
        let Some(database) = expand_path(database) else {
            continue;
        };
        let database = PathBuf::from(database);
        if !database.is_file() {
            continue;
        }
        let Ok(connection) =
            Connection::open_with_flags(&database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        else {
            continue;
        };
        for table in &config.tables {
            candidates.extend(sqlite_table_candidates(
                &connection,
                table,
                config,
                worktree_path,
            ));
        }
    }
    candidates
}

fn sqlite_table_candidates(
    connection: &Connection,
    table: &str,
    config: &SqliteSessionDiscoveryConfig,
    worktree_path: &Path,
) -> Vec<SessionCandidate> {
    if !is_valid_identifier(table)
        || !is_valid_identifier(&config.id_column)
        || !is_valid_identifier(&config.directory_column)
    {
        return Vec::new();
    }

    let has_updated = is_valid_identifier(&config.updated_column)
        && table_has_column(connection, table, &config.updated_column);
    // Keep the scan bounded. Session stores can grow large, and discovery runs
    // repeatedly after a fresh launch until the provider writes its new session.
    let sql = if has_updated {
        format!(
            "select {}, {}, {} from {} order by {} desc limit 1000",
            config.id_column,
            config.directory_column,
            config.updated_column,
            table,
            config.updated_column
        )
    } else {
        format!(
            "select {}, {} from {} limit 1000",
            config.id_column, config.directory_column, table
        )
    };

    let Ok(mut statement) = connection.prepare(&sql) else {
        return Vec::new();
    };
    let Ok(mut rows) = statement.query([]) else {
        return Vec::new();
    };

    let mut candidates = Vec::new();
    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(_) => break,
        };
        let Ok(id) = row.get::<_, String>(0) else {
            continue;
        };
        let Ok(directory) = row.get::<_, String>(1) else {
            continue;
        };
        if !same_path(&directory, worktree_path) {
            continue;
        }
        let updated = if has_updated {
            row.get::<_, Option<i64>>(2).ok().flatten().unwrap_or(0)
        } else {
            0
        };
        candidates.push(SessionCandidate { id, updated });
    }
    candidates
}

fn table_has_column(connection: &Connection, table: &str, column: &str) -> bool {
    let sql = format!("pragma table_info({table})");
    let Ok(mut statement) = connection.prepare(&sql) else {
        return false;
    };
    let Ok(rows) = statement.query_map([], |row| row.get::<_, String>(1)) else {
        return false;
    };
    rows.flatten().any(|name| name == column)
}

fn json_candidates(
    config: &JsonSessionDiscoveryConfig,
    worktree_path: &Path,
) -> Vec<SessionCandidate> {
    let mut candidates = Vec::new();
    for root in &config.roots {
        let Some(root) = expand_path(root) else {
            continue;
        };
        let root = PathBuf::from(root);
        if !root.is_dir() {
            continue;
        }
        collect_json_candidates(&root, config, worktree_path, &mut candidates);
    }
    candidates
}

fn collect_json_candidates(
    directory: &Path,
    config: &JsonSessionDiscoveryConfig,
    worktree_path: &Path,
    candidates: &mut Vec<SessionCandidate>,
) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_json_candidates(&path, config, worktree_path, candidates);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let Some(value) = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        else {
            continue;
        };
        let Some(id) = value.get(&config.id_field).and_then(Value::as_str) else {
            continue;
        };
        let Some(store_directory) = config
            .directory_fields
            .iter()
            .find_map(|field| value.get(field).and_then(Value::as_str))
        else {
            continue;
        };
        if !same_path(store_directory, worktree_path) {
            continue;
        }
        let updated = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0);
        candidates.push(SessionCandidate {
            id: id.to_string(),
            updated,
        });
    }
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

fn is_valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{JsonSessionDiscoveryConfig, SqliteSessionDiscoveryConfig};
    use tempfile::tempdir;

    fn sqlite_config(databases: &[PathBuf], tables: &[&str]) -> SessionDiscoveryConfig {
        SessionDiscoveryConfig::Sqlite(SqliteSessionDiscoveryConfig {
            databases: databases
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect(),
            tables: tables.iter().map(|table| table.to_string()).collect(),
            ..Default::default()
        })
    }

    fn create_sessions_table(
        connection: &Connection,
        table: &str,
        sessions: &[(&str, &Path, i64)],
    ) {
        connection
            .execute_batch(&format!(
                "create table {table} (
                    id text primary key,
                    directory text not null,
                    time_updated integer not null
                );"
            ))
            .expect("schema");
        for (id, directory, updated) in sessions {
            connection
                .execute(
                    &format!(
                        "insert into {table} (id, directory, time_updated) values (?1, ?2, ?3)"
                    ),
                    (id, directory.to_string_lossy().as_ref(), updated),
                )
                .expect("insert session");
        }
    }

    #[test]
    fn sqlite_reads_v2_sessions() {
        let tmp = tempdir().expect("tempdir");
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).expect("worktree");
        let database = tmp.path().join("opencode.db");
        let connection = Connection::open(&database).expect("db");
        create_sessions_table(
            &connection,
            "session_v2",
            &[("ses_old", &worktree, 1), ("ses_new", &worktree, 2)],
        );

        let config = sqlite_config(&[database], &["session_v2", "session"]);
        let ignored = HashSet::from(["ses_old".to_string()]);

        assert_eq!(
            latest_session_id_for_worktree(&config, &worktree, &ignored).as_deref(),
            Some("ses_new")
        );
    }

    #[test]
    fn sqlite_prefers_newest_row_across_v2_and_legacy_tables() {
        let tmp = tempdir().expect("tempdir");
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).expect("worktree");
        let database = tmp.path().join("opencode.db");
        let connection = Connection::open(&database).expect("db");
        create_sessions_table(&connection, "session", &[("ses_legacy", &worktree, 1)]);
        create_sessions_table(&connection, "session_v2", &[("ses_current", &worktree, 2)]);

        let config = sqlite_config(&[database], &["session_v2", "session"]);

        assert_eq!(
            latest_session_id_for_worktree(&config, &worktree, &HashSet::new()).as_deref(),
            Some("ses_current")
        );
    }

    #[test]
    fn sqlite_falls_back_to_legacy_sessions() {
        let tmp = tempdir().expect("tempdir");
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).expect("worktree");
        let database = tmp.path().join("opencode.db");
        let connection = Connection::open(&database).expect("db");
        create_sessions_table(&connection, "session", &[("ses_legacy", &worktree, 1)]);

        let config = sqlite_config(&[database], &["session_v2", "session"]);

        assert_eq!(
            latest_session_id_for_worktree(&config, &worktree, &HashSet::new()).as_deref(),
            Some("ses_legacy")
        );
    }

    #[test]
    fn sqlite_snapshot_lists_all_sessions_for_worktree() {
        let tmp = tempdir().expect("tempdir");
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).expect("worktree");
        let database = tmp.path().join("opencode.db");
        let connection = Connection::open(&database).expect("db");
        create_sessions_table(
            &connection,
            "session_v2",
            &[("ses_one", &worktree, 1), ("ses_two", &worktree, 2)],
        );

        let config = sqlite_config(&[database], &["session_v2"]);
        let ids = session_ids_for_worktree(&config, &worktree);

        assert_eq!(
            ids,
            HashSet::from(["ses_one".to_string(), "ses_two".to_string()])
        );
    }

    #[test]
    fn sqlite_handles_table_without_updated_column() {
        let tmp = tempdir().expect("tempdir");
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).expect("worktree");
        let database = tmp.path().join("store.db");
        let connection = Connection::open(&database).expect("db");
        connection
            .execute_batch("create table sessions (id text primary key, directory text not null);")
            .expect("schema");
        connection
            .execute(
                "insert into sessions (id, directory) values (?1, ?2)",
                ("ses_one", worktree.to_string_lossy().as_ref()),
            )
            .expect("insert");

        let config = sqlite_config(&[database], &["sessions"]);

        assert_eq!(
            latest_session_id_for_worktree(&config, &worktree, &HashSet::new()).as_deref(),
            Some("ses_one")
        );
    }

    #[test]
    fn sqlite_missing_database_is_ignored() {
        let tmp = tempdir().expect("tempdir");
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).expect("worktree");
        let config = sqlite_config(&[tmp.path().join("missing.db")], &["session_v2"]);

        assert_eq!(
            latest_session_id_for_worktree(&config, &worktree, &HashSet::new()),
            None
        );
    }

    #[test]
    fn json_reads_sessions_recursively() {
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

        let config = SessionDiscoveryConfig::Json(JsonSessionDiscoveryConfig {
            roots: vec![tmp.path().to_string_lossy().to_string()],
            ..Default::default()
        });
        let ignored = HashSet::from(["ses_old".to_string()]);

        assert_eq!(
            latest_session_id_for_worktree(&config, &worktree, &ignored).as_deref(),
            Some("ses_new")
        );
    }

    #[test]
    fn json_uses_directory_fields_fallback() {
        let tmp = tempdir().expect("tempdir");
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).expect("worktree");
        let session_dir = tmp.path().join("sessions");
        fs::create_dir_all(&session_dir).expect("session dir");
        fs::write(
            session_dir.join("ses_one.json"),
            format!(r#"{{"id":"ses_one","path":"{}"}}"#, worktree.display()),
        )
        .expect("session");

        let config = SessionDiscoveryConfig::Json(JsonSessionDiscoveryConfig {
            roots: vec![tmp.path().to_string_lossy().to_string()],
            ..Default::default()
        });

        assert_eq!(
            latest_session_id_for_worktree(&config, &worktree, &HashSet::new()).as_deref(),
            Some("ses_one")
        );
    }

    #[test]
    fn invalid_sql_identifiers_are_ignored() {
        let tmp = tempdir().expect("tempdir");
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).expect("worktree");
        let database = tmp.path().join("store.db");
        let connection = Connection::open(&database).expect("db");
        create_sessions_table(&connection, "session_v2", &[("ses_one", &worktree, 1)]);

        let config = SessionDiscoveryConfig::Sqlite({
            let mut config = SqliteSessionDiscoveryConfig {
                databases: vec![database.to_string_lossy().to_string()],
                tables: vec!["session_v2; drop table session_v2".to_string()],
                ..Default::default()
            };
            config.tables.push("session_v2".to_string());
            config
        });

        assert_eq!(
            latest_session_id_for_worktree(&config, &worktree, &HashSet::new()).as_deref(),
            Some("ses_one")
        );
    }
}
