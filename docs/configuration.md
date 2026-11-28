# yvo Configuration

[Back to README](../README.md)

## Table Of Contents

- [Configuration File](#configuration-file)
- [Migrating From dux](#migrating-from-dux)
- [Agent Control](#agent-control)
- [Orchestration](#orchestration)
- [Themes](#themes)
- [Keybindings](#keybindings)

## Configuration File

The config file at `~/.config/yvo/config.toml` (Linux) or `~/.yvo/config.toml` (macOS) is exhaustively commented. Every setting is explained inline, so you should never need to leave the file to understand an option. Every keybinding is rebindable. Every pane width, scrollback limit, default provider, and startup agent reopening behavior is configurable.

```bash
yvo config path          # Print the config file path
yvo config diff          # Show what you've changed from defaults
yvo config diff --raw    # Unified diff against the default config
yvo config reset         # Remove config and logs (keeps agents)
yvo config reset --all   # Full factory reset
yvo config regenerate    # Preview a fresh default config
yvo config migrate       # Preview moving legacy dux data into yvo
yvo config migrate --yes # Apply the move and rewrite stored paths
```

Override the config directory with the `YVO_HOME` environment variable.

### Migrating From dux

If you used this project before the rename, the old state lives at `~/.config/dux/config.toml` (Linux) or `~/.dux/config.toml` (macOS). Close all dux agents, run the command from outside the old directory, and review the plan before applying it:

```bash
yvo config migrate
yvo config migrate --yes
```

The migration moves the whole state directory, renames the runtime files, rewrites stored worktree and project paths, updates `[ui].theme` and `[logging].path`, and repairs git worktrees. Custom themes that define `dux.*` tokens need those tokens renamed to `yvo.*`. Re-copy the renamed skills afterward and replace `DUX_*` environment variables with `YVO_*` in your own scripts.

## Agent Control

yvo exposes a user-only Unix socket at `<config_dir>/yvo.sock` so agents running
inside yvo can request related worktrees through `yvo agent split-out`.

```toml
[agent_control]
enabled = true
allow_unregistered_repos = true
```

- `enabled` allows agent-initiated workspace changes. Set it to `false` to reject
  control requests.
- `allow_unregistered_repos` lets `--repo <path>` add an unregistered git
  checkout as a project before creating the related worktree. Set it to `false`
  if agents should only target projects you added manually.

## Orchestration

Yvo can create delegation trees where a parent agent dispatches work to child
agents in the same repository or another project. Policy lives in
`[orchestration]`:

```toml
[orchestration]
enabled = true
allow_same_repo = true
allow_nested = true
max_depth = 3
max_children_per_agent = 6
max_active_descendants_per_mission = 8
max_total_descendants_per_mission = 24
same_repo_default_base = "parent-head"
cross_repo_default_base = "leading"
allow_parent_snapshot = true
allow_agent_integration = false
wait_default_timeout_secs = 100
brief_max_bytes = 8388608
result_max_bytes = 262144
```

- `max_depth` is measured below a mission root; direct children are depth 1.
- `same_repo_default_base` controls whether same-repo children start from the
  parent's HEAD, the project leading branch, or a working-tree snapshot.
- `allow_parent_snapshot` permits `--base-mode parent-snapshot`, which captures
  tracked and untracked, non-ignored files in a synthetic baseline commit.
- `allow_agent_integration` is off by default; a human can still run
  `yvo agent integrate` from a terminal or the UI.
- `wait_default_timeout_secs` should stay below your agent CLI's tool-call
  timeout. A `yvo agent wait` timeout exits 2 and means "poll again".

The bundled `skills/yvo-split-out/SKILL.md` and
`skills/yvo-orchestrate/SKILL.md` teach supported agents when and how to use
delegation. Yvo does not install skills automatically; copy the files into your
agent's global skill directory.

## Themes

yvo writes `config.toml` the first time it launches, so theme setup starts from a real, editable file instead of a guessing game. The generated config includes `[ui].theme = "yvo_dark"`, plus comments with built-in theme examples. Edit that value, or use the `change-theme` command from the palette to preview and save a theme from inside the app.

Custom themes live next to the config file:

```text
~/.config/yvo/themes/my_theme.toml  # Linux
~/.yvo/themes/my_theme.toml         # macOS
```

Then set:

```toml
[ui]
theme = "my_theme"
```

Theme names resolve in this order: your `themes/<name>.toml` file wins first, then the bundled `yvo_dark`, then built-in [Opaline](https://github.com/hyperb1iss/opaline) themes such as `catppuccin_mocha`, `catppuccin_frappe`, `nord`, `dracula`, `gruvbox_dark`, `tokyo_night`, `solarized_dark`, `one_dark`, and `rose_pine`. If the name cannot be loaded, yvo falls back to `yvo_dark` and writes a warning to the log.

Themes use the [Opaline](https://github.com/hyperb1iss/opaline) TOML format. A small theme only needs semantic tokens; yvo derives its app-specific `yvo.*` colors from those so you do not have to define every button, gutter, and diff color by hand:

```toml
[meta]
name = "cyber_peacock"
author = "you"
variant = "dark"
description = "A vivid dark theme for yvo."

[palette]
base = "#101018"
panel = "#171725"
highlight = "#24243a"
active = "#303050"
text = "#f4f7ff"
muted = "#aab2d5"
dim = "#6f7899"
accent = "#00d4ff"
accent_secondary = "#ff4fd8"
border = "#5b6ee1"
success = "#4ade80"
error = "#fb7185"
warning = "#facc15"
info = "#38bdf8"

[tokens]
"text.primary" = "text"
"text.muted" = "muted"
"text.dim" = "dim"
"bg.base" = "base"
"bg.panel" = "panel"
"bg.highlight" = "highlight"
"bg.active" = "active"
"accent.primary" = "accent"
"accent.secondary" = "accent_secondary"
"border.focused" = "border"
"border.unfocused" = "dim"
success = "success"
error = "error"
warning = "warning"
info = "info"
```

Want full control? Add explicit `yvo.*` tokens. The bundled `assets/themes/yvo_dark.toml` is the complete reference, including header chrome, overlays, hints, diffs, help, inputs, and PR colors. PR state colors intentionally default to GitHub-style green, purple, and red so merged, open, and closed states stay recognizable, but you can override `yvo.pr_*` tokens too.

## Keybindings

All keybindings live in the `[keys]` section of the config. Key format supports single characters (`"j"`), special names (`"enter"`, `"pageup"`, `"shift-tab"`), and modifier combos (`"ctrl-d"`, `"ctrl-p"`). Each action takes an array of key combos:

```toml
[keys]
quit = ["ctrl-q"]
open_palette = ["ctrl-k"]
```

Press `?` in the app for the full keybinding reference. The help overlay is the authoritative source. These docs intentionally don't list individual bindings because they're yours to change.
