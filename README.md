# yvo

<p align="center">
  <img alt="Platform" src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS-0f766e" />
  <img alt="Language" src="https://img.shields.io/badge/language-Rust-334155" />
  <img alt="Interface" src="https://img.shields.io/badge/interface-terminal%20UI-2563eb" />
  <img alt="Providers" src="https://img.shields.io/badge/providers-Claude%20%2B%20Codex%20%2B%20OpenCode-7c3aed" />
  <img alt="Validation" src="https://img.shields.io/badge/validation-cargo%20test-16a34a" />
  <img alt="License" src="https://img.shields.io/github/license/dflaten/yvo" />
</p>

<img src="assets/yvo-logo.svg" width="200" align="right" alt="yvo logo" />

`yvo` is a terminal UI for orchestrating multiple AI coding agents, each project agent in its own git worktree, plus ad-hoc scratch agents that are not attached to a project, with live agent terminals, companion terminals, macros, commit generation, and a command palette.

yvo is a renamed personal fork of [patrickdappollonio/dux](https://github.com/patrickdappollonio/dux). The original package was great, but I needed a few more workflow features to use it as my day-to-day agent manager. This fork is intended to be installed from source rather than through upstream release channels.

<br clear="right" />

<p align="center">
  <img alt="An agent splits work into two related agents in other repositories" src="assets/yvo-multi-repo.png" width="100%" />
</p>

<p align="center">
  <a href="https://asciinema.org/a/IvqL89rXvwCzvSxQ">Watch the asciicast demo</a> <em>(recorded before the rename)</em>
</p>

## Table Of Contents

- [Overview](#overview)
- [Status](#status)
- [Delegated Worktrees](#delegated-worktrees)
- [Documentation](#documentation)
- [Install](#install)
- [Migrating From dux](#migrating-from-dux)
- [Prerequisites](#prerequisites)
- [Development Setup](#development-setup)

## Overview

| Icon | Item | Details |
| --- | --- | --- |
| 🖥 | Interface | Terminal UI with projects and agents, live agent output, changed files, and diffs |
| 🌿 | Worktrees | One isolated git worktree per project agent, with fork and existing-worktree workflows |
| ⇄ | Delegated work | Agents can delegate work into linked worktrees in the same repo or another project; the origin agent moves into this section with its delegation tree |
| ✦ | Scratch | Always-visible ad-hoc agents that run in their own folders with the diff pane hidden, and can be converted into projects later |
| 🤖 | Providers | Any CLI through a PTY; Claude, Codex, and OpenCode by default |
| 🧰 | Workflow | Companion terminals, macros, queued diff comments, AI commit messages, and worktree PR tracking with stacked-PR detection |
| 🎨 | Configuration | Fully commented config, rebindable keys, and a theme engine |
| 🧪 | Validation | `cargo fmt`, Clippy, unit tests, and PTY integration tests |

## Status

`yvo` currently supports macOS and Linux, with Windows users expected to run it through WSL2. The app is under active development as a personal fork.

Recent fork additions include:

- Copy and paste between agent and terminal panes.
- A searchable project and agent sidebar that narrows the tree as you type.
- A searchable existing-worktree picker for creating or reconnecting agents.
- Cleanup for inactive managed worktrees and unreferenced scratch workspaces.
- An always-visible Scratch workspace for ad-hoc agents, with the diff pane hidden and conversion into a new project or an existing project's worktree.
- Always-visible diffs against the base branch, with easy rebasing.
- Queued comments on selected diff lines that can be sent back to an agent.
- OpenCode session resume and discovery.
- Cross-pane macros with short descriptions, auto-created targets, optional Enter submission, background delivery badges, and configurable focus.
- Delegated worktrees: an agent can delegate work into a linked worktree in the
  same repository or another project, wait for a structured result, and collect
  the child's changes.

## Delegated Worktrees

An agent running inside yvo can delegate work to a child agent:

```bash
"$YVO_BIN" agent delegate \
  --repo /path/to/other-repo \
  --name fix-api-contract \
  --title "Fix the API contract" \
  --brief-file - \
  --json
```

Use `--same-repo` instead of `--repo`/`--project` to branch from the parent
agent's own repository. The default same-repo base is the parent's HEAD; use
`--base-mode leading` for the project leading branch or
`--base-mode parent-snapshot` to include the parent's uncommitted files.

Yvo creates the worktree, launches the child agent with a brief that points at a
durable task file, and links the child to the parent. The child reports back
with:

```bash
"$YVO_BIN" agent finish --status success --summary-file - <<'YVO'
What changed, how it was verified, and anything the parent must know.
YVO
```

The parent can then inspect the tree and result, wait for completion, or ask for
another iteration:

```bash
"$YVO_BIN" agent status --json
"$YVO_BIN" agent wait --all --timeout 100 --json
"$YVO_BIN" agent result --delegation <id> --json
"$YVO_BIN" agent followup --delegation <id> --text-file -
```

A `wait` timeout exits with code 2 and is not a failure; call it again to keep
polling. `blocked` children exit 4 and need a follow-up answer. `cancel` keeps
the worktree; `retry` is for failed launches; `integrate` merges a completed
same-repo child into its parent. The command palette also exposes
`cancel-delegation` and `integrate-delegation` for the selected child.

The target repo can already be a yvo project, or yvo can register it
automatically. Set `[agent_control] allow_unregistered_repos = false` in
`config.toml` if agents should only target projects you have added manually.
Delegation limits live under `[orchestration]`; set
`[agent_control] enabled = false` or `[orchestration] enabled = false` to
disable agent-initiated changes.

Agent control uses a user-only local socket at `<config_dir>/yvo.sock`. The
socket is not an agent protocol; it only lets the `yvo` CLI request workspace
changes from the running TUI.

The screenshot at the top of this README is generated by the reproducible
[multi-repo demo](demo/multi-repo/README.md). Run `make screenshots` after UI
changes to refresh it.

### Agent Skill

Yvo ships optional skills that teach agents how and when to delegate work:

```text
skills/yvo-split-out/SKILL.md    # one cross-repo child
skills/yvo-orchestrate/SKILL.md  # fan out, wait, follow up, integrate
```

Copy them into the global skill directory of the agent you use. For example:

```bash
mkdir -p ~/.config/opencode/skills/yvo-split-out
cp skills/yvo-split-out/SKILL.md ~/.config/opencode/skills/yvo-split-out/SKILL.md
mkdir -p ~/.config/opencode/skills/yvo-orchestrate
cp skills/yvo-orchestrate/SKILL.md ~/.config/opencode/skills/yvo-orchestrate/SKILL.md
```

OpenCode also discovers `~/.claude/skills` and `~/.agents/skills`. The skills are
not installed automatically. Run `yvo agent --help` for the authoritative
command reference.

## Documentation

User and setup documentation:

- [User Guide](docs/user-guide.md)
- [Configuration](docs/configuration.md)

## Install

Clone the repository and install the binary from source:

```bash
git clone https://github.com/dflaten/yvo.git
cd yvo
cargo build --release
mkdir -p ~/.local/bin
install -m 755 target/release/yvo ~/.local/bin/yvo
```

Make sure `~/.local/bin` is on your `PATH`, then run:

```bash
yvo
```

On first launch, yvo creates a fully commented config file. That file is the documentation for every setting.

## Migrating From dux

Existing installs keep their state under `~/.config/dux` (Linux) or `~/.dux` (macOS). yvo refuses to start until that data is moved, so the first launch cannot silently create an empty second workspace.

Close all dux agents, run the migration from outside the old directory, and preview it first:

```bash
yvo config migrate          # show what will move and change
yvo config migrate --yes    # move the data and rewrite stored paths
```

The migration moves the config directory, renames `dux.log`/`dux.lock`/`dux.sock` to their yvo names, rewrites project and worktree paths in `sessions.sqlite3`, updates the theme and log path in `config.toml`, and runs `git worktree repair` for affected repositories. Custom themes that define `dux.*` tokens need those tokens renamed to `yvo.*`. Afterward, re-copy `skills/yvo-orchestrate` and `skills/yvo-split-out` into your agent skill directory and update any scripts that use `DUX_BIN`, `DUX_SESSION_ID`, or other `DUX_*` variables to `YVO_*`.

## Prerequisites

- **`git`** — yvo is built around git worktrees, so git is non-negotiable.
- **Rust and Cargo** — required to build this fork from source.
- **`gh` CLI** *(optional)* — authenticate it with your GitHub account and yvo can pull PR statuses, check details, and show them in the interface.

## Development Setup

Recommended local tools:

- Stable Rust toolchain with Cargo
- `git`
- Optional: `gh` for PR tracking

Common commands:

```bash
cargo build
cargo run
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

To refresh the README screenshot after UI changes, run `make screenshots`. It
requires `tmux` and Python 3 with Pillow; see
[`demo/multi-repo/README.md`](demo/multi-repo/README.md) for details.

For an interactive smoke test, run `cargo run`.
