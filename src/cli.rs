use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use uuid::Uuid;

use crate::config::{self, Config, YvoPaths};
use crate::git;
use crate::keybindings::RuntimeBindings;
use crate::logger;
use crate::storage::SessionStore;

// ---------------------------------------------------------------------------
// CLI dispatch
// ---------------------------------------------------------------------------

pub fn run(args: &[String], paths: &YvoPaths) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_config_help();
        return Ok(());
    }
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "reset" => {
            let all = args[1..].iter().any(|a| a == "--all");
            reject_unknown_flags(&args[1..], &["--all"])?;
            run_reset(paths, all)
        }
        "diff" => {
            let raw = args[1..].iter().any(|a| a == "--raw");
            reject_unknown_flags(&args[1..], &["--raw"])?;
            run_diff(paths, raw)
        }
        "regenerate" => {
            let yes = args[1..].iter().any(|a| a == "--yes");
            reject_unknown_flags(&args[1..], &["--yes"])?;
            run_regenerate(paths, yes)
        }
        "migrate" => {
            let yes = args[1..].iter().any(|a| a == "--yes");
            reject_unknown_flags(&args[1..], &["--yes"])?;
            run_migrate(paths, yes)
        }
        "path" => {
            println!("{}", paths.config_path.display());
            Ok(())
        }
        "" | "--help" | "-h" => {
            print_config_help();
            Ok(())
        }
        other => bail!("unknown config subcommand: {other}\nRun `yvo config --help` for usage."),
    }
}

fn reject_unknown_flags(args: &[String], known: &[&str]) -> Result<()> {
    for arg in args {
        if arg.starts_with('-') && !known.contains(&arg.as_str()) {
            bail!("unknown flag: {arg}");
        }
    }
    Ok(())
}

fn print_config_help() {
    println!(
        "\
yvo config — manage the yvo configuration file

Subcommands:
  yvo config path          Print the config file path
  yvo config diff          Show settings that differ from defaults (summary)
  yvo config diff --raw    Show a unified diff against the default config
  yvo config reset         Remove config and logs (keeps agents and worktrees)
  yvo config reset --all   Full factory reset: remove config, logs, sessions, worktrees, and scratch workspaces
  yvo config regenerate    Preview a fresh default config (shows diff)
  yvo config regenerate --yes
                           Overwrite the config file with fresh defaults
  yvo config migrate       Preview moving legacy dux data into yvo
  yvo config migrate --yes Apply the move and rewrite stored paths"
    );
}

// ---------------------------------------------------------------------------
// yvo config reset
// ---------------------------------------------------------------------------

fn run_reset(paths: &YvoPaths, all: bool) -> Result<()> {
    if !paths.root.exists() {
        println!("nothing to reset: {} does not exist", paths.root.display());
        return Ok(());
    }

    let log_path = resolve_reset_log_path(paths);

    if all {
        reset_agent_data(paths)?;
    }

    remove_file_with_message(&log_path)?;
    prune_empty_ancestors(&log_path, &paths.root)?;
    remove_socket_with_message(&paths.control_socket_path)?;
    remove_file_with_message(&paths.config_path)?;
    prune_empty_ancestors(&paths.config_path, &paths.root)?;

    // The lockfile (`yvo.lock`) is intentionally left in place. Unlinking
    // it while holding the flock would orphan the inode: a new process
    // could create a fresh file at the same path (different inode) and
    // successfully flock it, breaking the single-instance guarantee. The
    // stale lockfile is harmless — the next launch takes it over
    // transparently — so `remove_root_if_empty` will simply skip removal
    // of root when the lockfile is the sole remaining entry.
    remove_root_if_empty_with_message(&paths.root)?;

    println!("reset complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// yvo config diff
// ---------------------------------------------------------------------------

fn run_diff(paths: &YvoPaths, raw: bool) -> Result<()> {
    if !paths.config_path.exists() {
        println!("no config file found at {}", paths.config_path.display());
        return Ok(());
    }

    let current_raw =
        fs::read_to_string(&paths.config_path).with_context_path(&paths.config_path)?;
    let current: Config = toml::from_str(&current_raw).with_context_path(&paths.config_path)?;

    if raw {
        run_diff_raw(&current_raw, &current)?;
    } else {
        run_diff_summary(&current)?;
    }
    Ok(())
}

fn run_diff_raw(_current_raw: &str, current: &Config) -> Result<()> {
    let bindings = RuntimeBindings::from_keys_config(&current.keys);
    let default_rendered = config::render_default_config();
    // Re-render current config to normalize it before diffing.
    let current_rendered = render_config_for_diff(current, &bindings);
    if current_rendered == default_rendered {
        println!("config matches defaults — no differences");
        return Ok(());
    }
    print_unified_diff("default", "current", &default_rendered, &current_rendered);
    Ok(())
}

fn run_diff_summary(current: &Config) -> Result<()> {
    let defaults = Config::default();
    let mut changes = Vec::new();

    // [defaults]
    diff_str(
        &mut changes,
        "defaults.provider",
        &defaults.defaults.provider,
        &current.defaults.provider,
    );
    diff_opt_str(
        &mut changes,
        "defaults.start_directory",
        defaults.defaults.start_directory.as_deref(),
        current.defaults.start_directory.as_deref(),
    );
    diff_opt_str(
        &mut changes,
        "defaults.commit_prompt",
        defaults.defaults.commit_prompt.as_deref(),
        current.defaults.commit_prompt.as_deref(),
    );

    // [agent_control]
    diff_bool(
        &mut changes,
        "agent_control.enabled",
        defaults.agent_control.enabled,
        current.agent_control.enabled,
    );
    diff_bool(
        &mut changes,
        "agent_control.allow_unregistered_repos",
        defaults.agent_control.allow_unregistered_repos,
        current.agent_control.allow_unregistered_repos,
    );

    // [orchestration]
    diff_bool(
        &mut changes,
        "orchestration.enabled",
        defaults.orchestration.enabled,
        current.orchestration.enabled,
    );
    diff_bool(
        &mut changes,
        "orchestration.allow_same_repo",
        defaults.orchestration.allow_same_repo,
        current.orchestration.allow_same_repo,
    );
    diff_bool(
        &mut changes,
        "orchestration.allow_nested",
        defaults.orchestration.allow_nested,
        current.orchestration.allow_nested,
    );
    diff_usize(
        &mut changes,
        "orchestration.max_depth",
        defaults.orchestration.max_depth,
        current.orchestration.max_depth,
    );
    diff_usize(
        &mut changes,
        "orchestration.max_children_per_agent",
        defaults.orchestration.max_children_per_agent,
        current.orchestration.max_children_per_agent,
    );
    diff_usize(
        &mut changes,
        "orchestration.max_active_descendants_per_mission",
        defaults.orchestration.max_active_descendants_per_mission,
        current.orchestration.max_active_descendants_per_mission,
    );
    diff_usize(
        &mut changes,
        "orchestration.max_total_descendants_per_mission",
        defaults.orchestration.max_total_descendants_per_mission,
        current.orchestration.max_total_descendants_per_mission,
    );
    diff_str(
        &mut changes,
        "orchestration.same_repo_default_base",
        &defaults.orchestration.same_repo_default_base,
        &current.orchestration.same_repo_default_base,
    );
    diff_str(
        &mut changes,
        "orchestration.cross_repo_default_base",
        &defaults.orchestration.cross_repo_default_base,
        &current.orchestration.cross_repo_default_base,
    );
    diff_bool(
        &mut changes,
        "orchestration.allow_parent_snapshot",
        defaults.orchestration.allow_parent_snapshot,
        current.orchestration.allow_parent_snapshot,
    );
    diff_bool(
        &mut changes,
        "orchestration.allow_agent_integration",
        defaults.orchestration.allow_agent_integration,
        current.orchestration.allow_agent_integration,
    );
    diff_usize(
        &mut changes,
        "orchestration.wait_default_timeout_secs",
        defaults.orchestration.wait_default_timeout_secs,
        current.orchestration.wait_default_timeout_secs,
    );
    diff_usize(
        &mut changes,
        "orchestration.brief_max_bytes",
        defaults.orchestration.brief_max_bytes,
        current.orchestration.brief_max_bytes,
    );
    diff_usize(
        &mut changes,
        "orchestration.result_max_bytes",
        defaults.orchestration.result_max_bytes,
        current.orchestration.result_max_bytes,
    );

    // [logging]
    diff_str(
        &mut changes,
        "logging.level",
        &defaults.logging.level,
        &current.logging.level,
    );
    diff_str(
        &mut changes,
        "logging.path",
        &defaults.logging.path,
        &current.logging.path,
    );

    // [ui]
    diff_u16(
        &mut changes,
        "ui.left_width_pct",
        defaults.ui.left_width_pct,
        current.ui.left_width_pct,
    );
    diff_u16(
        &mut changes,
        "ui.right_width_pct",
        defaults.ui.right_width_pct,
        current.ui.right_width_pct,
    );
    diff_bool(
        &mut changes,
        "ui.hide_scratch_right_pane",
        defaults.ui.hide_scratch_right_pane,
        current.ui.hide_scratch_right_pane,
    );
    diff_u16(
        &mut changes,
        "ui.terminal_pane_height_pct",
        defaults.ui.terminal_pane_height_pct,
        current.ui.terminal_pane_height_pct,
    );
    diff_u16(
        &mut changes,
        "ui.staged_pane_height_pct",
        defaults.ui.staged_pane_height_pct,
        current.ui.staged_pane_height_pct,
    );
    diff_u16(
        &mut changes,
        "ui.commit_pane_height_pct",
        defaults.ui.commit_pane_height_pct,
        current.ui.commit_pane_height_pct,
    );
    diff_usize(
        &mut changes,
        "ui.agent_scrollback_lines",
        defaults.ui.agent_scrollback_lines,
        current.ui.agent_scrollback_lines,
    );
    diff_u16(
        &mut changes,
        "ui.branch_sync_interval",
        defaults.ui.branch_sync_interval,
        current.ui.branch_sync_interval,
    );
    diff_bool(
        &mut changes,
        "ui.show_diff_line_numbers",
        defaults.ui.show_diff_line_numbers,
        current.ui.show_diff_line_numbers,
    );
    diff_u16(
        &mut changes,
        "ui.diff_tab_width",
        defaults.ui.diff_tab_width,
        current.ui.diff_tab_width,
    );
    diff_bool(
        &mut changes,
        "ui.github_integration",
        defaults.ui.github_integration,
        current.ui.github_integration,
    );
    diff_bool(
        &mut changes,
        "ui.auto_reopen_agents",
        defaults.ui.auto_reopen_agents,
        current.ui.auto_reopen_agents,
    );
    diff_u16(
        &mut changes,
        "ui.deleted_agent_retention_days",
        defaults.ui.deleted_agent_retention_days,
        current.ui.deleted_agent_retention_days,
    );
    diff_str(
        &mut changes,
        "ui.pr_banner_position",
        &defaults.ui.pr_banner_position,
        &current.ui.pr_banner_position,
    );
    diff_u16(
        &mut changes,
        "ui.github_poll_interval_seconds",
        defaults.ui.github_poll_interval_seconds,
        current.ui.github_poll_interval_seconds,
    );
    diff_bool(
        &mut changes,
        "ui.pr_section",
        defaults.ui.pr_section,
        current.ui.pr_section,
    );
    diff_u16(
        &mut changes,
        "ui.pr_section_max_rows",
        defaults.ui.pr_section_max_rows,
        current.ui.pr_section_max_rows,
    );
    diff_bool(
        &mut changes,
        "ui.pr_section_show_related",
        defaults.ui.pr_section_show_related,
        current.ui.pr_section_show_related,
    );
    diff_bool(
        &mut changes,
        "ui.macro_focus_on_launch",
        defaults.ui.macro_focus_on_launch,
        current.ui.macro_focus_on_launch,
    );
    diff_str(
        &mut changes,
        "ui.theme",
        &defaults.ui.theme,
        &current.ui.theme,
    );

    // [editor]
    diff_str(
        &mut changes,
        "editor.default",
        &defaults.editor.default,
        &current.editor.default,
    );

    // [terminal]
    diff_str(
        &mut changes,
        "terminal.command",
        &defaults.terminal.command,
        &current.terminal.command,
    );
    let default_args = defaults
        .terminal
        .args
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let current_args = current
        .terminal
        .args
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    diff_str(
        &mut changes,
        "terminal.args",
        &format!("[{default_args}]"),
        &format!("[{current_args}]"),
    );

    // [keys]
    diff_bool(
        &mut changes,
        "keys.show_terminal_keys",
        defaults.keys.show_terminal_keys,
        current.keys.show_terminal_keys,
    );
    diff_keybindings(
        &mut changes,
        &defaults.keys.bindings,
        &current.keys.bindings,
    );

    // [providers.*]
    diff_providers(&mut changes, &defaults, current);

    // [macros]
    if !current.macros.entries.is_empty() {
        changes.push(format!(
            "macros: {} macro(s) configured",
            current.macros.entries.len()
        ));
    }

    if changes.is_empty() {
        println!("config matches defaults — no differences");
    } else {
        for line in &changes {
            println!("  {line}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// yvo config regenerate
// ---------------------------------------------------------------------------

fn run_regenerate(paths: &YvoPaths, yes: bool) -> Result<()> {
    let fresh = config::render_default_config();

    if !yes {
        if paths.config_path.exists() {
            let current =
                fs::read_to_string(&paths.config_path).with_context_path(&paths.config_path)?;
            if current == fresh {
                println!("config already matches defaults — nothing to do");
                return Ok(());
            }
            print_unified_diff("current", "default", &current, &fresh);
            println!("\nRun `yvo config regenerate --yes` to overwrite with these defaults.");
        } else {
            println!("no config file exists; regenerate --yes will create one at:");
            println!("  {}", paths.config_path.display());
        }
        return Ok(());
    }

    paths.ensure_dirs()?;
    fs::write(&paths.config_path, fresh).with_context_path(&paths.config_path)?;
    println!("config regenerated at {}", paths.config_path.display());
    Ok(())
}

fn run_migrate(paths: &YvoPaths, yes: bool) -> Result<()> {
    let Some(plan) = crate::migrate::plan(paths)? else {
        println!("No legacy dux data found. Nothing to migrate.");
        return Ok(());
    };

    if !yes {
        println!("Legacy dux data found at {}", plan.old_root.display());
        println!("It will be moved to {}", plan.new_root.display());
        println!();
        println!("The migration will:");
        println!(
            "  - move config.toml, sessions.sqlite3, worktrees/, and scratch/ into the yvo directory"
        );
        println!("  - rename dux.log, dux.lock, and dux.sock to their yvo names");
        println!("  - rewrite stored project and worktree paths in sessions.sqlite3");
        println!("  - update the theme and logging.path settings in config.toml");
        println!("  - run `git worktree repair` for affected repositories");
        println!();
        println!("Close all dux agents first, then run `yvo config migrate --yes`.");
        return Ok(());
    }

    let new_root = plan.new_root.clone();
    let summary = crate::migrate::execute(plan)?;
    println!("Moved legacy data to {}", new_root.display());
    println!(
        "Rewrote {} session path(s), {} delegation path(s), {} project path(s), and {} scratch row(s).",
        summary.session_paths,
        summary.delegation_paths,
        summary.project_paths,
        summary.scratch_rows
    );
    if summary.config_updated {
        println!("Updated config.toml settings and comments.");
    }
    if summary.worktrees_repaired > 0 {
        println!(
            "Repaired {} git worktree link(s).",
            summary.worktrees_repaired
        );
    }
    for warning in &summary.warnings {
        eprintln!("Warning: {warning}");
    }
    println!();
    println!("Next steps:");
    println!("  1. Reinstall the binary as `yvo` so it resolves on PATH.");
    println!(
        "  2. Re-copy skills/yvo-orchestrate and skills/yvo-split-out into your agent skill directory."
    );
    println!(
        "  3. Update scripts that use DUX_BIN, DUX_SESSION_ID, or other DUX_* variables to YVO_*."
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Diff helpers
// ---------------------------------------------------------------------------

fn diff_str(changes: &mut Vec<String>, key: &str, default: &str, current: &str) {
    if default != current {
        let d = truncate_display(default, 40);
        let c = truncate_display(current, 40);
        changes.push(format!("{key}: {d} -> {c}"));
    }
}

fn diff_opt_str(
    changes: &mut Vec<String>,
    key: &str,
    default: Option<&str>,
    current: Option<&str>,
) {
    let d = default.unwrap_or("(unset)");
    let c = current.unwrap_or("(unset)");
    if d != c {
        let d = truncate_display(d, 40);
        let c = truncate_display(c, 40);
        changes.push(format!("{key}: {d} -> {c}"));
    }
}

fn diff_u16(changes: &mut Vec<String>, key: &str, default: u16, current: u16) {
    if default != current {
        changes.push(format!("{key}: {default} -> {current}"));
    }
}

fn diff_usize(changes: &mut Vec<String>, key: &str, default: usize, current: usize) {
    if default != current {
        changes.push(format!("{key}: {default} -> {current}"));
    }
}

fn diff_bool(changes: &mut Vec<String>, key: &str, default: bool, current: bool) {
    if default != current {
        changes.push(format!("{key}: {default} -> {current}"));
    }
}

fn diff_keybindings(
    changes: &mut Vec<String>,
    default: &BTreeMap<String, Vec<String>>,
    current: &BTreeMap<String, Vec<String>>,
) {
    for (action, default_keys) in default {
        match current.get(action) {
            Some(current_keys) if current_keys != default_keys => {
                changes.push(format!(
                    "keys.{action}: [{}] -> [{}]",
                    default_keys.join(", "),
                    current_keys.join(", "),
                ));
            }
            None => {
                changes.push(format!(
                    "keys.{action}: [{}] -> (removed)",
                    default_keys.join(", ")
                ));
            }
            _ => {}
        }
    }
    for action in current.keys() {
        if !default.contains_key(action) {
            let keys = &current[action];
            changes.push(format!("keys.{action}: (new) -> [{}]", keys.join(", "),));
        }
    }
}

fn diff_providers(changes: &mut Vec<String>, defaults: &Config, current: &Config) {
    for (name, default_cfg) in &defaults.providers.commands {
        match current.providers.get(name) {
            Some(current_cfg) => {
                if default_cfg.command != current_cfg.command {
                    changes.push(format!(
                        "providers.{name}.command: {} -> {}",
                        default_cfg.command, current_cfg.command
                    ));
                }
            }
            None => {
                changes.push(format!("providers.{name}: (removed)"));
            }
        }
    }
    for name in current.providers.commands.keys() {
        if !defaults.providers.commands.contains_key(name) {
            changes.push(format!("providers.{name}: (added)"));
        }
    }
}

/// Truncate a display string, replacing the end with "..." if too long.
fn truncate_display(s: &str, max: usize) -> String {
    // For multiline values just show first line.
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.chars().count() > max {
        let truncated: String = first_line.chars().take(max).collect();
        format!("{truncated}...")
    } else if s.contains('\n') {
        format!("{first_line}...")
    } else {
        s.to_string()
    }
}

fn render_config_for_diff(config: &Config, bindings: &RuntimeBindings) -> String {
    // Use the same render_config used for default to ensure comparable output.
    // This is a re-render of the current config through the canonical renderer.
    config::render_config_with(config, bindings)
}

fn print_unified_diff(label_a: &str, label_b: &str, a: &str, b: &str) {
    let diff = similar::TextDiff::from_lines(a, b);
    println!("--- {label_a}");
    println!("+++ {label_b}");
    for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
        println!("{hunk}");
    }
}

// ---------------------------------------------------------------------------
// Agent data reset
// ---------------------------------------------------------------------------

fn reset_agent_data(paths: &YvoPaths) -> Result<()> {
    if paths.sessions_db_path.exists() {
        match SessionStore::open(&paths.sessions_db_path) {
            Ok(store) => match store.load_sessions() {
                Ok(sessions) => {
                    for session in &sessions {
                        remove_session_worktree(paths, session);
                    }
                    println!("removed {} session worktree(s)", sessions.len());
                }
                Err(error) => {
                    eprintln!("warning: could not load sessions from database: {error}");
                }
            },
            Err(error) => {
                eprintln!("warning: could not open session database: {error}");
            }
        }
    }

    remove_dir_with_message(&paths.worktrees_root)?;
    remove_dir_with_message(&paths.scratch_root())?;
    remove_file_with_message(&paths.sessions_db_path)?;
    Ok(())
}

fn remove_session_worktree(paths: &YvoPaths, session: &crate::model::AgentSession) {
    // Scratch workspaces are removed wholesale with the scratch root below;
    // they have no git worktree or branch to detach first.
    if session.is_scratch() {
        return;
    }
    let worktree = Path::new(&session.worktree_path);
    if !git::is_under(&paths.worktrees_root, worktree) {
        eprintln!(
            "warning: skipping worktree outside of managed root: {}",
            session.worktree_path
        );
        return;
    }

    if worktree.exists() {
        let _ = std::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(worktree)
            .output();
        if worktree.exists() {
            let _ = fs::remove_dir_all(worktree);
        }
    }

    if let Some(project_path) = session.project_path.as_deref() {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(project_path)
            .arg("branch")
            .arg("-D")
            .arg(&session.branch_name)
            .output();
    }
}

// ---------------------------------------------------------------------------
// yvo agent
// ---------------------------------------------------------------------------

pub fn run_agent(args: &[String], paths: &YvoPaths) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    let wants_json = args.iter().any(|arg| arg == "--json");
    let result = match sub {
        "delegate" => run_agent_delegate(&args[1..], paths, false),
        "split-out" => run_agent_delegate(&args[1..], paths, true),
        "status" => run_agent_status(&args[1..], paths),
        "tree" => run_agent_tree(&args[1..], paths),
        "result" => run_agent_result(&args[1..], paths),
        "wait" => run_agent_wait(&args[1..], paths),
        "finish" => run_agent_finish(&args[1..], paths),
        "followup" => run_agent_followup(&args[1..], paths),
        "message" => run_agent_message(&args[1..], paths),
        "cancel" => run_agent_cancel(&args[1..], paths),
        "retry" => run_agent_retry(&args[1..], paths),
        "integrate" => run_agent_integrate(&args[1..], paths),
        "related" => run_agent_related(&args[1..], paths),
        "" | "--help" | "-h" => {
            print_agent_help();
            Ok(())
        }
        other => bail!("unknown agent subcommand: {other}\nRun `yvo agent --help` for usage."),
    };
    match result {
        Ok(()) => Ok(()),
        Err(err) if wants_json => {
            print_control_response(
                &crate::ipc::ControlResponse::error("invalid_request", format!("{err:#}")),
                true,
            );
            std::process::exit(1);
        }
        Err(err) => Err(err),
    }
}

fn print_agent_help() {
    println!(
        "\
yvo agent — work with agents from the command line

Subcommands:
  yvo agent delegate      Create a delegated worktree/agent in this or another repo
  yvo agent split-out     Legacy alias for delegate (waits for launch by default)
  yvo agent status        Show the delegation tree for an agent
  yvo agent tree          Human-readable delegation tree
  yvo agent result        Read a delegation result
  yvo agent wait          Wait for delegations to reach a terminal state
  yvo agent finish        Report a delegated child's result
  yvo agent followup      Send another iteration to a completed/blocked child
  yvo agent message       Best-effort message to a running child
  yvo agent cancel        Cancel a delegation or subtree (worktrees are kept)
  yvo agent retry         Retry a failed delegation launch
  yvo agent integrate     Merge a completed same-repo child into its parent
  yvo agent related       List agents related to the current or selected agent

delegate options:
  --repo <path>           Target git checkout (may be auto-registered as a project)
  --project <id|name>     Target project already registered in yvo
  --same-repo             Target the source session's project
  --name <branch>         Branch and worktree name (generated when omitted)
  --title <text>          Human-readable delegation title
  --base-mode <mode>      leading | ref | parent-head | parent-snapshot | sibling-head
  --base <ref>            Explicit base branch/tag/commit
  --base-session <id>     Sibling session for sibling-head
  --provider <name>       Provider override (defaults to the target project's provider)
  --brief <text>          Initial task brief for the child agent
  --brief-file <path|->   Read the task brief from a file or stdin
  --prompt <text>         Alias for --brief
  --prompt-file <path|->  Alias for --brief-file
  --request-key <key>     Idempotency key (defaults to the branch name)
  --source <session-id>   Source session (defaults to YVO_SESSION_ID)
  --wait-for-launch       Block until the child PTY is running or has failed
  --no-wait               Return after queueing (default for delegate)
  --timeout <seconds>     How long to wait for yvo (default 120)
  --json                  Print the machine-readable JSON response

wait options:
  --session <id>          Session to inspect (defaults to YVO_SESSION_ID)
  --delegation <id>       Delegation to inspect
  --all | --any           Wait for all / any current descendants
  --until started         Return when children are running or terminal
  --timeout <seconds>     Wall-clock wait budget (default from config, 100)
  --allow-empty           Succeed instead of erroring when nothing matches
  --json                  Print the machine-readable JSON response

result options:
  --session <id> | --delegation <id>
  --full                  Read the full result file from disk
  --json                  Print the machine-readable JSON response

finish options:
  --session <id>          Delegated session (defaults to YVO_SESSION_ID)
  --status success|failed|blocked
  --summary <text> | --summary-file <path|->
  --result-id <id>        Override the generated result id
  --blocker <text>        Record a blocker (repeatable)
  --suggest-followup <text> Record a suggested follow-up (repeatable)
  --json                  Print the machine-readable JSON response

cancel options:
  --session <id> | --delegation <id>
  --subtree               Cancel non-terminal descendants too
  --json                  Print the machine-readable JSON response

Environment:
  YVO_BIN                 Path to the yvo binary that launched the agent
  YVO_SESSION_ID          Current agent session id
  YVO_SOCKET              Override the control socket path"
    );
}

fn run_agent_delegate(args: &[String], paths: &YvoPaths, legacy_split_out: bool) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_agent_help();
        return Ok(());
    }

    let mut repo = None;
    let mut project = None;
    let mut same_repo = false;
    let mut name = None;
    let mut title = None;
    let mut base = None;
    let mut base_mode = None;
    let mut base_session = None;
    let mut provider = None;
    let mut prompt = None;
    let mut prompt_file = None;
    let mut request_key = None;
    let mut source_session_id = None;
    let mut timeout_secs = None;
    let mut no_wait = false;
    let mut wait_for_launch = legacy_split_out;
    let mut json = false;

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut take_value = |flag: &str| -> Result<String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| anyhow!("{flag} requires a value"))
        };
        match arg {
            "--repo" => repo = Some(take_value("--repo")?),
            "--project" => project = Some(take_value("--project")?),
            "--same-repo" => same_repo = true,
            "--name" => name = Some(take_value("--name")?),
            "--title" => title = Some(take_value("--title")?),
            "--base" => base = Some(take_value("--base")?),
            "--base-mode" => base_mode = Some(take_value("--base-mode")?),
            "--base-session" => base_session = Some(take_value("--base-session")?),
            "--provider" => provider = Some(take_value("--provider")?),
            "--brief" | "--prompt" => prompt = Some(take_value("--brief")?),
            "--brief-file" | "--prompt-file" => prompt_file = Some(take_value("--brief-file")?),
            "--request-key" => request_key = Some(take_value("--request-key")?),
            "--source" => source_session_id = Some(take_value("--source")?),
            "--wait-for-launch" => wait_for_launch = true,
            "--no-wait" => {
                no_wait = true;
                wait_for_launch = false;
            }
            "--timeout" => {
                let value = take_value("--timeout")?;
                timeout_secs = Some(
                    value
                        .parse::<u64>()
                        .with_context(|| format!("invalid --timeout value: {value}"))?,
                );
            }
            "--json" => json = true,
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => bail!("unexpected argument: {other}"),
        }
        index += 1;
    }

    if repo.is_none() && project.is_none() && !same_repo {
        bail!("delegate requires --repo <path>, --project <id|name>, or --same-repo.");
    }
    if same_repo && (repo.is_some() || project.is_some()) {
        bail!("use --same-repo by itself, not with --repo or --project.");
    }
    if prompt.is_some() && prompt_file.is_some() {
        bail!("use either --brief or --brief-file, not both.");
    }
    if let Some(name) = name.as_deref()
        && !git::is_valid_agent_name(name)
    {
        bail!(
            "invalid agent name \"{name}\". Use letters, numbers, dashes, underscores, and slashes."
        );
    }
    if let Some(file) = prompt_file {
        prompt = Some(read_brief_file(&file, 8 * 1024 * 1024)?);
    }
    if !legacy_split_out && prompt.as_deref().is_none_or(|text| text.trim().is_empty()) {
        bail!("delegate requires a non-empty --brief or --brief-file.");
    }
    if source_session_id.is_none() {
        source_session_id = std::env::var("YVO_SESSION_ID")
            .ok()
            .filter(|value| !value.trim().is_empty());
    }
    if source_session_id.is_none() {
        bail!("delegate must run inside a yvo agent, or be given --source <session-id>.");
    }
    let repo = repo.map(|raw| {
        if let Some(expanded) = crate::config::expand_path(&raw) {
            expanded
        } else {
            let path = PathBuf::from(&raw);
            if path.is_absolute() {
                raw
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(path).to_string_lossy().to_string())
                    .unwrap_or(raw)
            }
        }
    });

    // Stage large briefs outside the control socket. The TUI validates that the
    // path lives under the pending directory before adopting it.
    let (prompt, brief_path) = match prompt {
        Some(text) if text.len() > 64 * 1024 => {
            let path = stage_brief(paths, &text)?;
            (None, Some(path))
        }
        other => (other, None),
    };

    let mut params = crate::ipc::SplitOutParams {
        source_session_id,
        repo,
        project,
        name: name.clone(),
        base,
        provider,
        prompt,
        timeout_ms: timeout_secs.map(|seconds| seconds.saturating_mul(1000)),
        title,
        base_mode,
        base_session,
        request_key: request_key.or(name),
        no_wait,
        wait_for_launch,
        brief_path,
        brief_sha256: None,
    };
    // The server cannot see --same-repo; it infers the source project from the
    // source session when no target is provided.
    if same_repo {
        params.repo = None;
        params.project = None;
    }

    let request = if legacy_split_out {
        crate::ipc::ControlRequest::split_out(params)
    } else {
        crate::ipc::ControlRequest::delegate(params)
    };
    send_and_print_request(paths, request, json)
}

fn read_brief_file(path: &str, limit: usize) -> Result<String> {
    let text = if path == "-" {
        let mut text = String::new();
        use std::io::Read;
        std::io::stdin()
            .take(limit as u64 + 1)
            .read_to_string(&mut text)
            .context("failed to read delegation brief from stdin")?;
        text
    } else {
        let metadata = fs::metadata(path)
            .with_context(|| format!("failed to inspect delegation brief {path}"))?;
        if metadata.len() > limit as u64 {
            bail!(
                "delegation brief {path} is too large ({} bytes, limit {limit})",
                metadata.len()
            );
        }
        fs::read_to_string(path)
            .with_context(|| format!("failed to read delegation brief {path}"))?
    };
    if text.len() > limit {
        bail!(
            "delegation brief is too large ({} bytes, limit {limit})",
            text.len()
        );
    }
    Ok(text)
}

fn stage_brief(paths: &YvoPaths, text: &str) -> Result<String> {
    let dir = paths.root.join("delegations").join("pending");
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    let path = dir.join(format!("{}.md", Uuid::new_v4()));
    fs::write(&path, text)
        .with_context(|| format!("failed to stage delegation brief {}", path.display()))?;
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    Ok(path.to_string_lossy().to_string())
}

fn run_agent_related(args: &[String], paths: &YvoPaths) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_agent_help();
        return Ok(());
    }

    let mut source_session_id = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut take_value = |flag: &str| -> Result<String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| anyhow!("{flag} requires a value"))
        };
        match arg {
            "--session" => source_session_id = Some(take_value("--session")?),
            "--json" => json = true,
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => bail!("unexpected argument: {other}"),
        }
        index += 1;
    }
    if source_session_id.is_none() {
        source_session_id = std::env::var("YVO_SESSION_ID")
            .ok()
            .filter(|value| !value.trim().is_empty());
    }
    if source_session_id.is_none() {
        bail!("related needs a session id, either from YVO_SESSION_ID or --session <session-id>.");
    }
    let request = crate::ipc::ControlRequest::related(source_session_id);
    send_and_print_request(paths, request, json)
}

#[derive(Default)]
struct DelegationTargetArgs {
    source_session_id: Option<String>,
    session_id: Option<String>,
    delegation_id: Option<String>,
    json: bool,
}

fn parse_delegation_target_args(args: &[String]) -> Result<DelegationTargetArgs> {
    let mut parsed = DelegationTargetArgs::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut take_value = |flag: &str| -> Result<String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| anyhow!("{flag} requires a value"))
        };
        match arg {
            "--session" => parsed.session_id = Some(take_value("--session")?),
            "--delegation" => parsed.delegation_id = Some(take_value("--delegation")?),
            "--source" => parsed.source_session_id = Some(take_value("--source")?),
            "--json" => parsed.json = true,
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => bail!("unexpected argument: {other}"),
        }
        index += 1;
    }
    if parsed.source_session_id.is_none() {
        parsed.source_session_id = std::env::var("YVO_SESSION_ID")
            .ok()
            .filter(|value| !value.trim().is_empty());
    }
    Ok(parsed)
}

fn run_agent_status(args: &[String], paths: &YvoPaths) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_agent_help();
        return Ok(());
    }
    let parsed = parse_delegation_target_args(args)?;
    let request = crate::ipc::ControlRequest::delegation_status(
        parsed.source_session_id,
        parsed.session_id,
        parsed.delegation_id,
    );
    send_and_print_request(paths, request, parsed.json)
}

fn run_agent_tree(args: &[String], paths: &YvoPaths) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_agent_help();
        return Ok(());
    }
    let parsed = parse_delegation_target_args(args)?;
    let request = crate::ipc::ControlRequest::delegation_status(
        parsed.source_session_id,
        parsed.session_id,
        parsed.delegation_id,
    );
    let response = send_request_or_print(paths, &request)?;
    if !response.ok {
        print_control_response(&response, false);
        std::process::exit(1);
    }
    if parsed.json {
        print_control_response(&response, true);
        return Ok(());
    }
    print_delegation_tree(&response);
    Ok(())
}

fn run_agent_result(args: &[String], paths: &YvoPaths) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_agent_help();
        return Ok(());
    }
    let mut full = false;
    let mut filtered = Vec::new();
    for arg in args {
        if arg == "--full" {
            full = true;
        } else {
            filtered.push(arg.clone());
        }
    }
    let parsed = parse_delegation_target_args(&filtered)?;
    let request = crate::ipc::ControlRequest::delegation_result(
        parsed.source_session_id,
        parsed.session_id,
        parsed.delegation_id,
    );
    let response = send_request_or_print(paths, &request)?;
    if !response.ok {
        print_control_response(&response, parsed.json);
        std::process::exit(1);
    }
    if parsed.json {
        print_control_response(&response, true);
        return Ok(());
    }
    if let Some(summary) = response.result_summary.as_deref() {
        println!("{summary}");
    }
    if let Some(path) = response.result_path.as_deref() {
        if full {
            match std::fs::read_to_string(path) {
                Ok(content) => println!("{content}"),
                Err(err) => bail!("could not read result file {path}: {err}"),
            }
        } else {
            println!("\nresult file: {path}");
        }
    } else if response.result_summary.is_none() {
        bail!("this delegation has no result yet.");
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WaitMode {
    All,
    Any,
    UntilStarted,
}

fn run_agent_wait(args: &[String], paths: &YvoPaths) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_agent_help();
        return Ok(());
    }
    let mut parsed = DelegationTargetArgs::default();
    let mut all = false;
    let mut any = false;
    let mut until_started = false;
    let mut allow_empty = false;
    let mut quiet = false;
    let mut timeout_secs: Option<u64> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut take_value = |flag: &str| -> Result<String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| anyhow!("{flag} requires a value"))
        };
        match arg {
            "--session" => parsed.session_id = Some(take_value("--session")?),
            "--delegation" => parsed.delegation_id = Some(take_value("--delegation")?),
            "--source" => parsed.source_session_id = Some(take_value("--source")?),
            "--all" => all = true,
            "--any" => any = true,
            "--until" => {
                let value = take_value("--until")?;
                if value != "started" {
                    bail!("--until only supports \"started\".");
                }
                until_started = true;
            }
            "--allow-empty" => allow_empty = true,
            "--quiet" => quiet = true,
            "--json" => parsed.json = true,
            "--timeout" => {
                let value = take_value("--timeout")?;
                timeout_secs = Some(
                    value
                        .parse::<u64>()
                        .with_context(|| format!("invalid --timeout value: {value}"))?,
                );
            }
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => bail!("unexpected argument: {other}"),
        }
        index += 1;
    }
    if all && any {
        bail!("use either --all or --any, not both.");
    }
    if parsed.source_session_id.is_none() {
        parsed.source_session_id = std::env::var("YVO_SESSION_ID")
            .ok()
            .filter(|value| !value.trim().is_empty());
    }
    if parsed.source_session_id.is_none()
        && parsed.session_id.is_none()
        && parsed.delegation_id.is_none()
    {
        bail!("wait needs --session, --delegation, or a yvo agent environment.");
    }
    let mode = if until_started {
        WaitMode::UntilStarted
    } else if any {
        WaitMode::Any
    } else {
        WaitMode::All
    };
    let timeout = timeout_secs.unwrap_or_else(|| {
        fs::read_to_string(&paths.config_path)
            .ok()
            .and_then(|raw| toml::from_str::<Config>(&raw).ok())
            .map(|config| config.orchestration.wait_default_timeout_secs as u64)
            .unwrap_or(100)
    });
    let deadline =
        (timeout > 0).then(|| std::time::Instant::now() + std::time::Duration::from_secs(timeout));
    let mut delay = std::time::Duration::from_millis(250);
    loop {
        let request = crate::ipc::ControlRequest::delegation_status(
            parsed.source_session_id.clone(),
            parsed.session_id.clone(),
            parsed.delegation_id.clone(),
        );
        let response = match crate::ipc::send_request(
            &paths.control_socket_path,
            &request,
            crate::ipc::DEFAULT_CONTROL_TIMEOUT,
        ) {
            Ok(response) => response,
            Err(err) => {
                let response =
                    crate::ipc::ControlResponse::error("connection_failed", err.to_string());
                print_control_response(&response, parsed.json);
                std::process::exit(3);
            }
        };
        if !response.ok {
            print_control_response(&response, parsed.json);
            std::process::exit(1);
        }
        let mut response = response;
        if parsed.delegation_id.is_none()
            && let Some(root) = parsed
                .session_id
                .as_deref()
                .or(parsed.source_session_id.as_deref())
        {
            // A delegated child that is itself an orchestrator must not wait on
            // its own task row; otherwise `wait --all` deadlocks.
            response
                .delegations
                .retain(|delegation| delegation.child_session_id.as_deref() != Some(root));
        }
        if response.delegations.is_empty() && !allow_empty {
            print_control_response(
                &crate::ipc::ControlResponse::error(
                    "no_delegations",
                    "No matching delegations exist. Pass --allow-empty to treat that as success.",
                ),
                parsed.json,
            );
            std::process::exit(1);
        }
        if response.delegations.is_empty() && allow_empty {
            print_control_response(&response, parsed.json);
            std::process::exit(0);
        }
        let finished = match mode {
            WaitMode::All => response
                .delegations
                .iter()
                .all(|delegation| is_wait_terminal_state(&delegation.state)),
            WaitMode::Any => response
                .delegations
                .iter()
                .any(|delegation| is_wait_terminal_state(&delegation.state)),
            WaitMode::UntilStarted => response.delegations.iter().all(|delegation| {
                !matches!(
                    delegation.state.as_str(),
                    "queued" | "preparing" | "starting"
                )
            }),
        };
        if finished {
            if !quiet && !parsed.json {
                eprintln!("delegation wait finished.");
            }
            print_control_response(&response, parsed.json);
            std::process::exit(wait_exit_code(&response, mode));
        }
        if let Some(deadline) = deadline
            && std::time::Instant::now() >= deadline
        {
            if !quiet && !parsed.json {
                eprintln!("delegation wait timed out; call wait again to continue.");
            }
            print_control_response(&response, parsed.json);
            std::process::exit(2);
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(std::time::Duration::from_secs(2));
    }
}

fn is_wait_terminal_state(state: &str) -> bool {
    matches!(
        state,
        "completed" | "failed" | "blocked" | "cancelled" | "exited" | "interrupted"
    )
}

fn wait_exit_code(response: &crate::ipc::ControlResponse, mode: WaitMode) -> i32 {
    if mode == WaitMode::UntilStarted {
        if response.delegations.iter().any(|delegation| {
            matches!(
                delegation.state.as_str(),
                "failed" | "cancelled" | "exited" | "interrupted"
            )
        }) {
            return 1;
        }
        return 0;
    }
    if response.delegations.iter().any(|delegation| {
        matches!(
            delegation.state.as_str(),
            "failed" | "cancelled" | "exited" | "interrupted"
        )
    }) {
        return 1;
    }
    if response
        .delegations
        .iter()
        .any(|delegation| delegation.state == "blocked")
    {
        return 4;
    }
    0
}

fn run_agent_finish(args: &[String], paths: &YvoPaths) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_agent_help();
        return Ok(());
    }
    let mut session_id = None;
    let mut delegation_id = std::env::var("YVO_DELEGATION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let mut status = None;
    let mut summary = None;
    let mut summary_file = None;
    let mut result_id = None;
    let mut blockers = Vec::new();
    let mut suggested_followups = Vec::new();
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut take_value = |flag: &str| -> Result<String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| anyhow!("{flag} requires a value"))
        };
        match arg {
            "--session" => session_id = Some(take_value("--session")?),
            "--delegation" => delegation_id = Some(take_value("--delegation")?),
            "--status" => status = Some(take_value("--status")?),
            "--summary" => summary = Some(take_value("--summary")?),
            "--summary-file" => summary_file = Some(take_value("--summary-file")?),
            "--result-id" => result_id = Some(take_value("--result-id")?),
            "--blocker" => blockers.push(take_value("--blocker")?),
            "--suggest-followup" => suggested_followups.push(take_value("--suggest-followup")?),
            "--json" => json = true,
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => bail!("unexpected argument: {other}"),
        }
        index += 1;
    }
    if summary.is_some() && summary_file.is_some() {
        bail!("use either --summary or --summary-file, not both.");
    }
    if let Some(path) = summary_file {
        summary = Some(read_brief_file(&path, 1024 * 1024)?);
    }
    let status = status.unwrap_or_else(|| "success".to_string());
    if !matches!(status.as_str(), "success" | "failed" | "blocked") {
        bail!("--status must be success, failed, or blocked.");
    }
    let summary = summary.unwrap_or_else(|| format!("Delegation finished with status {status}."));
    let source_session_id = std::env::var("YVO_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let result = crate::model::DelegationResult {
        result_id: result_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        status,
        summary,
        details: String::new(),
        tests: Vec::new(),
        blockers,
        suggested_followups,
        artifacts: Vec::new(),
        reported_at: Some(Utc::now()),
    };
    let result_path = std::env::var("YVO_RESULT_FILE")
        .ok()
        .filter(|value| !value.trim().is_empty());
    if let Some(path) = result_path.as_deref() {
        if let Some(parent) = Path::new(path).parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(encoded) = serde_json::to_string_pretty(&result) {
            if let Err(err) = fs::write(path, encoded) {
                eprintln!("warning: could not write delegation result to {path}: {err}");
            } else {
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
            }
        }
    }
    let mut wire_result = result;
    if serde_json::to_vec(&wire_result)
        .map(|encoded| encoded.len() > 128 * 1024)
        .unwrap_or(false)
    {
        // The full payload is already durable on disk. Keep the control
        // message small; the TUI reads the file back for the full result.
        wire_result.details.clear();
        wire_result.tests.clear();
        wire_result.summary = wire_result.summary.chars().take(8192).collect();
    }
    let request = crate::ipc::ControlRequest::delegation_finish(
        source_session_id,
        session_id,
        delegation_id,
        wire_result,
        result_path,
    );
    send_and_print_request(paths, request, json)
}

fn run_agent_followup(args: &[String], paths: &YvoPaths) -> Result<()> {
    run_agent_followup_inner(args, paths, false)
}

fn run_agent_followup_inner(args: &[String], paths: &YvoPaths, message_only: bool) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_agent_help();
        return Ok(());
    }
    let mut source_session_id = std::env::var("YVO_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let mut session_id = None;
    let mut delegation_id = None;
    let mut text = None;
    let mut text_file = None;
    let mut submit = true;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut take_value = |flag: &str| -> Result<String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| anyhow!("{flag} requires a value"))
        };
        match arg {
            "--source" => source_session_id = Some(take_value("--source")?),
            "--session" => session_id = Some(take_value("--session")?),
            "--delegation" => delegation_id = Some(take_value("--delegation")?),
            "--text" => text = Some(take_value("--text")?),
            "--text-file" => text_file = Some(take_value("--text-file")?),
            "--no-submit" => submit = false,
            "--json" => json = true,
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => bail!("unexpected argument: {other}"),
        }
        index += 1;
    }
    if text.is_some() && text_file.is_some() {
        bail!("use either --text or --text-file, not both.");
    }
    if let Some(path) = text_file {
        text = Some(read_brief_file(&path, 256 * 1024)?);
    }
    let text = text.ok_or_else(|| anyhow!("followup requires --text or --text-file."))?;
    let request = if message_only {
        crate::ipc::ControlRequest::delegation_message(
            source_session_id,
            session_id,
            delegation_id,
            text,
            submit,
        )
    } else {
        crate::ipc::ControlRequest::delegation_followup(
            source_session_id,
            session_id,
            delegation_id,
            text,
            submit,
        )
    };
    send_and_print_request(paths, request, json)
}

fn run_agent_message(args: &[String], paths: &YvoPaths) -> Result<()> {
    run_agent_followup_inner(args, paths, true)
}

fn run_agent_cancel(args: &[String], paths: &YvoPaths) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_agent_help();
        return Ok(());
    }
    let mut source_session_id = std::env::var("YVO_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let mut session_id = None;
    let mut delegation_id = None;
    let mut subtree = false;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut take_value = |flag: &str| -> Result<String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| anyhow!("{flag} requires a value"))
        };
        match arg {
            "--source" => source_session_id = Some(take_value("--source")?),
            "--session" => session_id = Some(take_value("--session")?),
            "--delegation" => delegation_id = Some(take_value("--delegation")?),
            "--subtree" => subtree = true,
            "--json" => json = true,
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => bail!("unexpected argument: {other}"),
        }
        index += 1;
    }
    let request = crate::ipc::ControlRequest::delegation_cancel(
        source_session_id,
        session_id,
        delegation_id,
        subtree,
    );
    send_and_print_request(paths, request, json)
}

fn run_agent_retry(args: &[String], paths: &YvoPaths) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_agent_help();
        return Ok(());
    }
    let mut delegation_id = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut take_value = |flag: &str| -> Result<String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| anyhow!("{flag} requires a value"))
        };
        match arg {
            "--delegation" => delegation_id = Some(take_value("--delegation")?),
            "--json" => json = true,
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => bail!("unexpected argument: {other}"),
        }
        index += 1;
    }
    let delegation_id =
        delegation_id.ok_or_else(|| anyhow!("retry requires --delegation <id>."))?;
    send_and_print_request(
        paths,
        crate::ipc::ControlRequest::delegation_retry(delegation_id),
        json,
    )
}

fn run_agent_integrate(args: &[String], paths: &YvoPaths) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_agent_help();
        return Ok(());
    }
    let mut session_id = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut take_value = |flag: &str| -> Result<String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| anyhow!("{flag} requires a value"))
        };
        match arg {
            "--session" => session_id = Some(take_value("--session")?),
            "--json" => json = true,
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => bail!("unexpected argument: {other}"),
        }
        index += 1;
    }
    let session_id = session_id
        .or_else(|| {
            std::env::var("YVO_SESSION_ID")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .ok_or_else(|| anyhow!("integrate requires --session <id>."))?;
    let source_session_id = std::env::var("YVO_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty());
    send_and_print_request(
        paths,
        crate::ipc::ControlRequest::delegation_integrate(source_session_id, session_id),
        json,
    )
}

fn print_delegation_tree(response: &crate::ipc::ControlResponse) {
    if response.delegations.is_empty() {
        if let Some(message) = response.message.as_deref() {
            println!("{message}");
        } else {
            println!("No delegations.");
        }
        return;
    }
    let mut ordered = response.delegations.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|delegation| (delegation.depth, delegation.created_at.clone()));
    for delegation in ordered {
        let indent = "  ".repeat(delegation.depth.saturating_sub(1) as usize);
        let project = delegation
            .project_name
            .as_deref()
            .or(delegation.project_id.as_deref())
            .unwrap_or("unknown");
        let branch = delegation
            .branch
            .as_deref()
            .or(delegation.base_label.as_deref())
            .unwrap_or("-");
        println!(
            "{indent}- [{}] {} · {} · {} · {}",
            delegation.state, delegation.title, project, branch, delegation.id
        );
        if let Some(summary) = delegation.result_summary.as_deref() {
            println!("{indent}  {summary}");
        }
    }
}

// ---------------------------------------------------------------------------
// yvo pr
// ---------------------------------------------------------------------------

pub fn run_pr(args: &[String], paths: &YvoPaths) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    let wants_json = args.iter().any(|arg| arg == "--json");
    let result = match sub {
        "list" => run_pr_list(&args[1..], paths),
        "link" => run_pr_link(&args[1..], paths),
        "" | "--help" | "-h" => {
            print_pr_help();
            Ok(())
        }
        other => bail!("unknown pr subcommand: {other}\nRun `yvo pr --help` for usage."),
    };
    match result {
        Ok(()) => Ok(()),
        Err(err) if wants_json => {
            print_control_response(
                &crate::ipc::ControlResponse::error("invalid_request", format!("{err:#}")),
                true,
            );
            std::process::exit(1);
        }
        Err(err) => Err(err),
    }
}

fn print_pr_help() {
    println!(
        "\
yvo pr — inspect and link pull requests for an agent

Subcommands:
  yvo pr list             List pull requests for an agent
  yvo pr link <pr>        Associate a PR with an agent worktree

list options:
  --session <session-id>  Session to inspect (defaults to YVO_SESSION_ID)
  --json                  Print the machine-readable JSON response

link options:
  --session <session-id>  Session to link (defaults to YVO_SESSION_ID)
  --json                  Print the machine-readable JSON response

<pr> accepts a PR number, #number, or GitHub PR URL.

Environment:
  YVO_SESSION_ID          Current agent session id
  YVO_SOCKET              Override the control socket path"
    );
}

fn default_pr_session() -> Option<String> {
    std::env::var("YVO_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn run_pr_list(args: &[String], paths: &YvoPaths) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_pr_help();
        return Ok(());
    }
    let mut session_id = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut take_value = |flag: &str| -> Result<String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| anyhow!("{flag} requires a value"))
        };
        match arg {
            "--session" => session_id = Some(take_value("--session")?),
            "--json" => json = true,
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => bail!("unexpected argument: {other}"),
        }
        index += 1;
    }
    let session_id = session_id.or_else(default_pr_session).ok_or_else(|| {
        anyhow!("pr list needs a session id, either from YVO_SESSION_ID or --session <session-id>.")
    })?;
    send_and_print_request(
        paths,
        crate::ipc::ControlRequest::pull_requests(Some(session_id)),
        json,
    )
}

fn run_pr_link(args: &[String], paths: &YvoPaths) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_pr_help();
        return Ok(());
    }
    let mut session_id = None;
    let mut json = false;
    let mut input = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut take_value = |flag: &str| -> Result<String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| anyhow!("{flag} requires a value"))
        };
        match arg {
            "--session" => session_id = Some(take_value("--session")?),
            "--json" => json = true,
            other if other.starts_with('-') && !other.starts_with('#') => {
                bail!("unknown flag: {other}")
            }
            other => {
                if input.replace(other.to_string()).is_some() {
                    bail!("pr link accepts one PR URL or number.");
                }
            }
        }
        index += 1;
    }
    let input = input.ok_or_else(|| anyhow!("pr link requires a PR URL or number."))?;
    let session_id = session_id.or_else(default_pr_session).ok_or_else(|| {
        anyhow!("pr link needs a session id, either from YVO_SESSION_ID or --session <session-id>.")
    })?;
    send_and_print_request(
        paths,
        crate::ipc::ControlRequest::link_pull_request(Some(session_id), input),
        json,
    )
}

fn send_and_print_request(
    paths: &YvoPaths,
    request: crate::ipc::ControlRequest,
    json: bool,
) -> Result<()> {
    let response = send_request_or_print(paths, &request)?;
    print_control_response(&response, json);
    if !response.ok {
        std::process::exit(1);
    }
    Ok(())
}

fn send_request_or_print(
    paths: &YvoPaths,
    request: &crate::ipc::ControlRequest,
) -> Result<crate::ipc::ControlResponse> {
    let timeout = request.command.timeout();
    match crate::ipc::send_request(&paths.control_socket_path, request, timeout) {
        Ok(response) => Ok(response),
        Err(err) => {
            let response = crate::ipc::ControlResponse::error("connection_failed", err.to_string());
            let json = false;
            print_control_response(&response, json);
            std::process::exit(1);
        }
    }
}

fn print_control_response(response: &crate::ipc::ControlResponse, json: bool) {
    if json {
        match serde_json::to_string_pretty(response) {
            Ok(encoded) => println!("{encoded}"),
            Err(err) => eprintln!("failed to encode yvo response: {err}"),
        }
        return;
    }

    if !response.ok {
        eprintln!(
            "{}",
            response
                .error
                .as_deref()
                .unwrap_or("yvo could not complete the request.")
        );
        return;
    }

    if let Some(message) = response.message.as_deref() {
        println!("{message}");
    }
    if let Some(project) = response.project.as_deref() {
        println!("project:  {project}");
    }
    if let Some(branch) = response.branch.as_deref() {
        println!("branch:   {branch}");
    }
    if let Some(worktree) = response.worktree.as_deref() {
        println!("worktree: {worktree}");
    }
    if let Some(session_id) = response.session_id.as_deref() {
        println!("session:  {session_id}");
    }
    if let Some(group_id) = response.group_id.as_deref() {
        println!("group:    {group_id}");
    }
    if let Some(delegation_id) = response.delegation_id.as_deref() {
        println!("delegation: {delegation_id}");
    }
    if let Some(state) = response.delegation_state.as_deref() {
        println!("state:      {state}");
    }
    if let Some(summary) = response.result_summary.as_deref() {
        println!("result:     {summary}");
    }
    if let Some(path) = response.result_path.as_deref() {
        println!("result file: {path}");
    }
    if !response.delegations.is_empty() {
        println!();
        print_delegation_tree(response);
    }
    if !response.pull_requests.is_empty() {
        println!();
        for pr in &response.pull_requests {
            let indent = "  ".repeat(pr.depth);
            let current = if pr.is_current { " (current)" } else { "" };
            let related = if pr.is_related { " (related)" } else { "" };
            println!(
                "{indent}- [{}] {}#{} · {} · {} · {}{}{}",
                pr.state,
                pr.owner_repo,
                pr.number,
                pr.title,
                pr.project_name,
                pr.session_label,
                current,
                related,
            );
            println!("{indent}  {}", pr.url);
        }
    }
    if !response.related.is_empty() {
        println!();
        for agent in &response.related {
            let origin = if agent.is_origin { " (origin)" } else { "" };
            println!(
                "- [{}] {} · {} · {} · {}{}",
                agent.project_name,
                agent.label,
                agent.branch_name,
                agent.worktree_path,
                agent.status,
                origin
            );
        }
    }
}

// ---------------------------------------------------------------------------
// File / directory helpers
// ---------------------------------------------------------------------------

fn resolve_reset_log_path(paths: &YvoPaths) -> PathBuf {
    let logging = if paths.config_path.exists() {
        fs::read_to_string(&paths.config_path)
            .ok()
            .and_then(|raw| toml::from_str::<config::Config>(&raw).ok())
            .map(|config| config.logging)
            .unwrap_or_default()
    } else {
        config::LoggingConfig::default()
    };
    logger::resolve_log_path(&logging, paths)
}

fn remove_file_with_message(path: &Path) -> Result<()> {
    if remove_file_if_present(path)? {
        println!("removed {}", path.display());
    }
    Ok(())
}

/// Remove the agent control socket if it still exists. `YVO_SOCKET` may point
/// anywhere, so never delete a regular file that happens to live at that path.
fn remove_socket_with_message(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => remove_file_with_message(path),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(anyhow!("failed to inspect {}: {error}", path.display())),
    }
}

fn remove_dir_with_message(path: &Path) -> Result<()> {
    if remove_dir_if_present(path)? {
        println!("removed {}", path.display());
    }
    Ok(())
}

fn remove_root_if_empty_with_message(path: &Path) -> Result<()> {
    if remove_dir_if_empty(path)? {
        println!("removed {}", path.display());
    }
    Ok(())
}

fn remove_file_if_present(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(anyhow!("failed to remove {}: {error}", path.display())),
    }
}

fn remove_dir_if_present(path: &Path) -> Result<bool> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(anyhow!("failed to remove {}: {error}", path.display())),
    }
}

fn remove_dir_if_empty(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let mut entries = fs::read_dir(path)
        .map_err(|error| anyhow!("failed to inspect {}: {error}", path.display()))?;
    if entries.next().is_some() {
        return Ok(false);
    }
    fs::remove_dir(path)
        .map_err(|error| anyhow!("failed to remove {}: {error}", path.display()))?;
    Ok(true)
}

fn prune_empty_ancestors(path: &Path, root: &Path) -> Result<()> {
    let Ok(relative) = path.strip_prefix(root) else {
        return Ok(());
    };
    if relative.as_os_str().is_empty() {
        return Ok(());
    }

    let mut current = path.parent();
    while let Some(dir) = current {
        if dir == root {
            break;
        }
        if !remove_dir_if_empty(dir)? {
            break;
        }
        current = dir.parent();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Convenience extension for anyhow context on paths
// ---------------------------------------------------------------------------

trait WithContextPath<T> {
    fn with_context_path(self, path: &Path) -> Result<T>;
}

impl<T, E: std::fmt::Display> WithContextPath<T> for std::result::Result<T, E> {
    fn with_context_path(self, path: &Path) -> Result<T> {
        self.map_err(|e| anyhow!("{}: {e}", path.display()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::Utc;
    use tempfile::TempDir;

    use super::*;
    use crate::config::{self, Config};
    use crate::keybindings::RuntimeBindings;
    use crate::model::{AgentSession, ProviderKind, SessionStatus};

    #[test]
    fn reset_rejects_unknown_flags() {
        let error = reject_unknown_flags(&["--wat".to_string()], &["--all"]).unwrap_err();
        assert!(error.to_string().contains("unknown flag"));
    }

    #[test]
    fn default_reset_removes_config_and_logs_but_keeps_agent_data() {
        let harness = ResetHarness::new();
        harness.write_config_with_log_path("logs/custom.log");
        harness.write_log("logs/custom.log");
        let worktree = harness.create_session("agent-1");

        run_reset(&harness.paths, false).expect("reset");

        assert!(!harness.paths.config_path.exists());
        assert!(!harness.paths.root.join("logs/custom.log").exists());
        assert!(harness.paths.sessions_db_path.exists());
        assert!(worktree.exists());

        let _config = config::ensure_config(&harness.paths).expect("config recreated");
        let store = SessionStore::open(&harness.paths.sessions_db_path).expect("store");
        let sessions = store.load_sessions().expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].worktree_path, worktree.to_string_lossy());
    }

    #[test]
    fn reset_all_wipes_database_and_worktrees() {
        let harness = ResetHarness::new();
        harness.write_config_with_log_path("logs/custom.log");
        harness.write_log("logs/custom.log");
        harness.create_session("agent-1");

        run_reset(&harness.paths, true).expect("reset");

        assert!(!harness.paths.root.exists());
    }

    #[test]
    fn reset_succeeds_when_paths_are_already_missing() {
        let harness = ResetHarness::new();
        fs::create_dir_all(&harness.paths.root).expect("root");

        run_reset(&harness.paths, false).expect("reset");

        assert!(!harness.paths.root.exists());
    }

    #[test]
    fn reset_all_removes_worktrees_without_database() {
        let harness = ResetHarness::new();
        fs::create_dir_all(harness.paths.worktrees_root.join("orphan")).expect("orphan worktree");

        run_reset(&harness.paths, true).expect("reset");

        assert!(!harness.paths.root.exists());
    }

    #[test]
    fn reset_all_removes_scratch_workspaces() {
        let harness = ResetHarness::new();
        let scratch = harness.paths.scratch_root().join("ad-hoc");
        fs::create_dir_all(&scratch).expect("scratch workspace");
        fs::write(scratch.join("note.txt"), "scratch\n").expect("scratch file");

        run_reset(&harness.paths, true).expect("reset");

        assert!(!scratch.exists());
        assert!(!harness.paths.root.exists());
    }

    #[test]
    fn diff_summary_reports_no_differences_for_defaults() {
        // Just verify it runs without error on defaults.
        let defaults = Config::default();
        run_diff_summary(&defaults).expect("diff summary");
    }

    #[test]
    fn diff_str_records_change() {
        let mut changes = Vec::new();
        diff_str(&mut changes, "test.key", "old", "new");
        assert_eq!(changes.len(), 1);
        assert!(changes[0].contains("old -> new"));
    }

    #[test]
    fn diff_str_ignores_equal() {
        let mut changes = Vec::new();
        diff_str(&mut changes, "test.key", "same", "same");
        assert!(changes.is_empty());
    }

    #[test]
    fn config_path_subcommand() {
        // Just verify it doesn't error.
        let paths = YvoPaths {
            root: PathBuf::from("/tmp/test"),
            config_path: PathBuf::from("/tmp/test/config.toml"),
            sessions_db_path: PathBuf::from("/tmp/test/sessions.sqlite3"),
            worktrees_root: PathBuf::from("/tmp/test/worktrees"),
            control_socket_path: PathBuf::from("/tmp/test/yvo.sock"),
            lock_path: PathBuf::from("/tmp/test/yvo.lock"),
        };
        let result = run(&["path".to_string()], &paths);
        assert!(result.is_ok());
    }

    #[test]
    fn config_help_flag_works_after_a_subcommand() {
        let harness = ResetHarness::new();
        let result = run(&["diff".to_string(), "--help".to_string()], &harness.paths);
        assert!(result.is_ok());
    }

    #[test]
    fn reset_keeps_regular_files_at_the_control_socket_path() {
        let harness = ResetHarness::new();
        fs::write(&harness.paths.control_socket_path, "keep me").expect("regular file");

        run_reset(&harness.paths, false).expect("reset");

        assert_eq!(
            fs::read_to_string(&harness.paths.control_socket_path).expect("file survives"),
            "keep me"
        );
    }

    #[test]
    fn reset_removes_a_stale_control_socket() {
        let harness = ResetHarness::new();
        let listener = std::os::unix::net::UnixListener::bind(&harness.paths.control_socket_path)
            .expect("bind socket");
        drop(listener);
        assert!(harness.paths.control_socket_path.exists());

        run_reset(&harness.paths, false).expect("reset");

        assert!(!harness.paths.control_socket_path.exists());
    }

    struct ResetHarness {
        _tempdir: TempDir,
        paths: YvoPaths,
    }

    impl ResetHarness {
        fn new() -> Self {
            let tempdir = TempDir::new().expect("tempdir");
            let root = tempdir.path().join("yvo");
            fs::create_dir_all(&root).expect("root");
            let paths = YvoPaths {
                config_path: root.join("config.toml"),
                sessions_db_path: root.join("sessions.sqlite3"),
                worktrees_root: root.join("worktrees"),
                control_socket_path: root.join("yvo.sock"),
                lock_path: root.join("yvo.lock"),
                root,
            };
            Self {
                _tempdir: tempdir,
                paths,
            }
        }

        fn write_config_with_log_path(&self, log_path: &str) {
            let mut config = Config::default();
            config.logging.path = log_path.to_string();
            let bindings = RuntimeBindings::from_keys_config(&config.keys);
            let body = config::render_config_with(&config, &bindings);
            fs::write(&self.paths.config_path, body).expect("config");
        }

        fn write_log(&self, relative_path: &str) {
            let path = self.paths.root.join(relative_path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("log dir");
            }
            fs::write(path, "log").expect("log");
        }

        fn create_session(&self, id: &str) -> PathBuf {
            fs::create_dir_all(&self.paths.worktrees_root).expect("worktrees root");
            let worktree = self.paths.worktrees_root.join(id);
            fs::create_dir_all(&worktree).expect("worktree");

            let store = SessionStore::open(&self.paths.sessions_db_path).expect("store");
            let now = Utc::now();
            store
                .upsert_session(&AgentSession {
                    id: id.to_string(),
                    project_id: "proj".to_string(),
                    project_path: None,
                    provider: ProviderKind::new("claude"),
                    source_branch: "main".to_string(),
                    branch_name: format!("branch-{id}"),
                    worktree_path: worktree.to_string_lossy().to_string(),
                    title: None,
                    started_providers: Vec::new(),
                    provider_session_ids: Default::default(),
                    desired_running: false,
                    auto_reopen_enabled: true,
                    status: SessionStatus::Active,
                    group_id: None,
                    parent_session_id: None,
                    created_at: now,
                    updated_at: now,
                })
                .expect("session");
            worktree
        }
    }
}
