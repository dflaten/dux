# dux

<p align="center">
  <img alt="Platform" src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS-0f766e" />
  <img alt="Language" src="https://img.shields.io/badge/language-Rust-334155" />
  <img alt="Interface" src="https://img.shields.io/badge/interface-terminal%20UI-2563eb" />
  <img alt="Providers" src="https://img.shields.io/badge/providers-Claude%20%2B%20Codex%20%2B%20OpenCode-7c3aed" />
  <img alt="Validation" src="https://img.shields.io/badge/validation-cargo%20test-16a34a" />
  <img alt="License" src="https://img.shields.io/github/license/dflaten/dux" />
</p>

<img src="assets/dux-logo.png" width="200" align="right" />

`dux` is a terminal UI for orchestrating multiple AI coding agents, each in its own git worktree, with live agent terminals, companion terminals, macros, commit generation, and a command palette.

This repository is a personal fork of [patrickdappollonio/dux](https://github.com/patrickdappollonio/dux). The original package was great, but I needed a few more workflow features to use it as my day-to-day agent manager. This fork is intended to be installed from source rather than through upstream release channels.

<br clear="right" />

[![asciicast](assets/dux-screenshot.svg)](https://asciinema.org/a/IvqL89rXvwCzvSxQ)

## Table Of Contents

- [Overview](#overview)
- [Status](#status)
- [Documentation](#documentation)
- [Install](#install)
- [Prerequisites](#prerequisites)
- [Development Setup](#development-setup)

## Overview

| Icon | Item | Details |
| --- | --- | --- |
| 🖥 | Interface | Terminal UI with projects and agents, live agent output, changed files, and diffs |
| 🌿 | Worktrees | One isolated git worktree per agent, with fork and existing-worktree workflows |
| 🤖 | Providers | Any CLI through a PTY; Claude, Codex, and OpenCode by default |
| 🧰 | Workflow | Companion terminals, macros, queued diff comments, AI commit messages, and PR tracking |
| 🎨 | Configuration | Fully commented config, rebindable keys, and a theme engine |
| 🧪 | Validation | `cargo fmt`, Clippy, unit tests, and PTY integration tests |

## Status

`dux` currently supports macOS and Linux, with Windows users expected to run it through WSL2. The app is under active development as a personal fork.

Recent fork additions include:

- Copy and paste between agent and terminal panes.
- A searchable existing-worktree picker for creating or reconnecting agents.
- Cleanup for inactive managed worktrees.
- Always-visible diffs against the base branch, with easy rebasing.
- Queued comments on selected diff lines that can be sent back to an agent.
- OpenCode session resume and discovery.
- Cross-pane macros that run in the agent or companion terminal, launching the target when it is missing.
- Per-macro Enter submission, background delivery badges, and configurable macro focus.

## Documentation

User and setup documentation:

- [User Guide](docs/user-guide.md)
- [Configuration](docs/configuration.md)

## Install

Clone the repository and install the binary from source:

```bash
git clone https://github.com/dflaten/dux.git
cd dux
cargo build --release
mkdir -p ~/.local/bin
install -m 755 target/release/dux ~/.local/bin/dux
```

Make sure `~/.local/bin` is on your `PATH`, then run:

```bash
dux
```

On first launch, dux creates a fully commented config file. That file is the documentation for every setting.

## Prerequisites

- **`git`** — dux is built around git worktrees, so git is non-negotiable.
- **Rust and Cargo** — required to build this fork from source.
- **`gh` CLI** *(optional)* — authenticate it with your GitHub account and dux can pull PR statuses, check details, and show them in the interface.

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

For an interactive smoke test, run `cargo run`.
