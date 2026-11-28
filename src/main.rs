mod app;
mod browser;
mod cli;
mod clipboard;
mod config;
mod diff;
mod editor;
mod git;
mod io_retry;
mod ipc;
mod keybindings;
mod lockfile;
mod logger;
mod migrate;
mod model;
mod provider;
mod pty;
mod raw_input;
mod session_discovery;
mod startup;
mod statusline;
mod storage;
mod theme;

use std::path::Path;

use anyhow::Result;

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();

    if wants_top_level_help(&args) {
        print_help();
        return Ok(());
    }

    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("yvo {}", env!("YVO_DISPLAY_VERSION"));
        return Ok(());
    }

    let paths = config::YvoPaths::discover()?;

    // Do not silently create a fresh yvo workspace while the old dux
    // directory still owns the user's config, sessions, and worktrees. The
    // migration command and read-only config inspection are allowed so the
    // user can see what will move before committing.
    if let Some(legacy_root) = config::legacy_data_pending(&paths)?
        && !legacy_pending_allowed(&args)
    {
        eprintln!(
            "Found legacy dux data at {}.\n\
             Run `yvo config migrate` to review the move, then `yvo config migrate --yes` \
             to apply it.\n\
             yvo will not start until the legacy data is migrated or moved aside.\n\
             Set YVO_HOME to a different directory to start a separate workspace.",
            legacy_root.display()
        );
        std::process::exit(1);
    }

    if args.first().map(|s| s.as_str()) == Some("agent") {
        return cli::run_agent(&args[1..], &paths);
    }

    if args.first().map(|s| s.as_str()) == Some("pr") {
        return cli::run_pr(&args[1..], &paths);
    }

    if args.first().map(|s| s.as_str()) == Some("config") {
        let config_args = &args[1..];
        let sub = config_args.first().map(|s| s.as_str()).unwrap_or("");

        // Acquire the single-instance lock only for subcommands that
        // mutate shared on-disk state. Read-only operations (path, diff,
        // regenerate preview) skip the lock entirely.
        let _lock = match sub {
            // reset mutates state when root exists. When root is absent,
            // run_reset's fast-path reports "nothing to reset" and exits,
            // so we avoid creating the directory just to take a lock.
            "reset" if paths.root.exists() => Some(acquire_lock_or_exit(&paths.lock_path)),

            // regenerate --yes creates directories and writes config.
            // Create root (so the lockfile can be opened) and lock before
            // any writes, preventing a concurrent TUI from starting
            // between directory creation and the config write.
            "regenerate" if config_args.iter().any(|a| a == "--yes") => {
                std::fs::create_dir_all(&paths.root)?;
                Some(acquire_lock_or_exit(&paths.lock_path))
            }

            // Everything else is read-only or prints help — no shared
            // state to protect. This includes: path, diff, diff --raw,
            // regenerate (preview without --yes), --help, and empty.
            _ => None,
        };

        return cli::run(config_args, &paths);
    }

    // TUI: always create the root directory (so the lockfile can be
    // opened), acquire the lock, then let bootstrap create everything
    // else. A losing process never touches shared state beyond the
    // empty root.
    std::fs::create_dir_all(&paths.root)?;
    let lock = acquire_lock_or_exit(&paths.lock_path);
    let mut app = app::App::bootstrap_with_lock(paths, lock)?;
    app.run()
}

/// Only the help flags as the first argument print top-level help. Running
/// `yvo` with no arguments must launch the TUI, and subcommands such as
/// `yvo agent --help` and `yvo config diff --help` handle their own help.
fn wants_top_level_help(args: &[String]) -> bool {
    matches!(
        args.first().map(|arg| arg.as_str()),
        Some("--help") | Some("-h")
    )
}

/// Commands that are safe to run while legacy dux data is still waiting.
/// Everything else is blocked so a fresh yvo directory cannot be created
/// beside the real data.
fn legacy_pending_allowed(args: &[String]) -> bool {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return true;
    }
    if args.first().map(String::as_str) != Some("config") {
        return false;
    }
    matches!(
        args.get(1).map(String::as_str),
        None | Some("migrate") | Some("path") | Some("diff")
    )
}

fn print_help() {
    println!(
        "yvo\n\n\
         Terminal UI for AI worktree sessions.\n\n\
         Usage:\n\
          yvo              Launch the TUI\n\
          yvo --version    Print the version\n\
          yvo agent        Create, inspect, and coordinate delegated agents\n\
          yvo pr           List or link pull requests for an agent\n\
          yvo config       Manage the configuration file\n\n\
         Agent subcommands:\n\
          yvo agent delegate      Create a delegated worktree in this or another repo\n\
          yvo agent status        Show the delegation tree for an agent\n\
          yvo agent wait          Wait for delegations to finish\n\
          yvo agent finish        Report a delegated child's result\n\
          yvo agent --help        Full delegation commands and options\n\n\
         PR subcommands:\n\
          yvo pr list             List pull requests associated with an agent\n\
          yvo pr link <pr>        Associate a PR with an agent worktree\n\
          yvo pr --help           Full PR options\n\n\
         Config subcommands:\n\
          yvo config path          Print the config file path\n\
          yvo config diff          Show settings that differ from defaults\n\
          yvo config diff --raw    Show a unified diff against the default config\n\
          yvo config reset         Remove config and logs (keeps agents and worktrees)\n\
          yvo config reset --all   Full factory reset (config, logs, sessions, worktrees)\n\
          yvo config regenerate    Preview a fresh default config (shows diff)\n\
          yvo config regenerate --yes\n\
                                   Overwrite the config file with fresh defaults\n\
          yvo config migrate       Preview moving legacy dux data into yvo\n\
          yvo config migrate --yes Apply the move and rewrite stored paths\n\n\
         Environment variables:\n\
           YVO_HOME    Override the config directory (must be an absolute path).\n\
                       When unset, defaults to:\n\
                         macOS: ~/.yvo/\n\
                         Linux: $XDG_CONFIG_HOME/yvo/ or ~/.config/yvo/\n\
           YVO_SOCKET  Override the agent control socket path (absolute).\n\n\
         First run writes a full default config to:\n\
           macOS: ~/.yvo/config.toml\n\
           Linux: $XDG_CONFIG_HOME/yvo/config.toml or ~/.config/yvo/config.toml\n\
         Session state is stored in:\n\
           macOS: ~/.yvo/sessions.sqlite3\n\
           Linux: $XDG_CONFIG_HOME/yvo/sessions.sqlite3 or ~/.config/yvo/sessions.sqlite3"
    );
}

fn acquire_lock_or_exit(path: &Path) -> lockfile::SingleInstanceLock {
    match lockfile::SingleInstanceLock::acquire(path) {
        Ok(lock) => lock,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arguments_launches_the_tui_instead_of_printing_help() {
        assert!(!wants_top_level_help(&[]));
    }

    #[test]
    fn help_flags_as_the_first_argument_print_top_level_help() {
        assert!(wants_top_level_help(&["--help".to_string()]));
        assert!(wants_top_level_help(&["-h".to_string()]));
    }

    #[test]
    fn help_flags_after_a_subcommand_are_left_to_the_subcommand() {
        assert!(!wants_top_level_help(&[
            "config".to_string(),
            "diff".to_string(),
            "--help".to_string(),
        ]));
        assert!(!wants_top_level_help(&[
            "agent".to_string(),
            "split-out".to_string(),
            "--help".to_string(),
        ]));
    }

    #[test]
    fn legacy_guard_allows_only_migration_and_read_only_config_commands() {
        assert!(legacy_pending_allowed(&["config".to_string()]));
        assert!(legacy_pending_allowed(&[
            "config".to_string(),
            "migrate".to_string()
        ]));
        assert!(legacy_pending_allowed(&[
            "config".to_string(),
            "path".to_string()
        ]));
        assert!(legacy_pending_allowed(&[
            "config".to_string(),
            "diff".to_string()
        ]));
        assert!(!legacy_pending_allowed(&[
            "config".to_string(),
            "reset".to_string()
        ]));
        assert!(!legacy_pending_allowed(&[
            "agent".to_string(),
            "status".to_string()
        ]));
        assert!(legacy_pending_allowed(&[
            "agent".to_string(),
            "--help".to_string()
        ]));
        assert!(!legacy_pending_allowed(&[]));
    }
}
