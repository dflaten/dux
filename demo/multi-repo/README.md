# Multi-repo screenshot demo

This directory regenerates `assets/dux-multi-repo.png`, the README screenshot
that shows one agent splitting work into related agents in two other
repositories.

The demo runs a real dux instance against three local git repositories. The
agent terminal is a deterministic scripted provider, and pull requests come from
a small `gh` shim, so repeated captures stay stable while the UI changes.

## What it shows

- `acme/storefront` — origin project on `feature/gift-cards`
- `acme/checkout-api` — first split-out target
- `acme/billing-service` — second split-out target

The origin agent runs the real `dux agent split-out` command for both targets.
Dux creates the worktrees, launches the related agents, and links them under
**Multi-repo work**. The right pane shows the current PR plus both related PRs:

- `acme/storefront#412` — Add gift card redemption to the checkout flow
- `acme/checkout-api#98` — Add gift card apply endpoint (draft)
- `acme/billing-service#57` — Record gift card ledger entries

## Requirements

- Rust toolchain, with dux built at `target/debug/dux` or `target/release/dux`
- `git`
- `tmux`
- Python 3 with Pillow (`python3 -m pip install Pillow`)
- A monospace font with the glyphs dux uses. The renderer defaults to
  GeistMono Nerd Font Mono, DejaVu Sans Mono, DejaVu Sans, and Noto Sans Math.
  On macOS it also looks for Menlo and Apple Symbols.
- Optional: `fontconfig` (`fc-match`) for automatic font discovery on either
  platform.

## Regenerate the screenshot

```bash
cargo build
./demo/multi-repo/capture.sh
```

Or use the Makefile target:

```bash
make screenshots
```

The capture script rebuilds the demo from scratch by default, starts dux in a
`220x56` tmux pane, waits for the origin agent, both related agents, and all
three PRs, then renders the styled pane to:

```text
assets/dux-multi-repo.png
```

The styled tmux capture is kept at `$DUX_DEMO_ROOT/dux-multi-repo.ansi` for
debugging.

## Run the demo live

```bash
./demo/multi-repo/run.sh --fresh
```

`--fresh` rebuilds the repositories and seed sessions first, so the origin agent
performs the split-out again. Omit `--fresh` to reopen the existing state; the
origin agent detects the linked agents and skips duplicate split-outs.

Select the origin agent under **Multi-repo work** to see the split-out
transcript, or select either related agent to inspect its own worktree, changes,
and PR.

## Options

`capture.sh`:

```text
--output <path>  Write the PNG somewhere else.
--keep-state     Reuse the current demo state instead of rebuilding it.
-h, --help       Show this help.
```

Environment overrides:

- `DUX_DEMO_ROOT` — demo work directory. Defaults to
  `$TMPDIR/dux-multi-repo-demo`.
- `DEMO_DUX_BIN` — dux binary to run. Defaults to `target/debug/dux`, then
  `target/release/dux`.
- `DUX_DEMO_FONT`, `DUX_DEMO_FONT_BOLD`, `DUX_DEMO_SYMBOL_FONT`,
  `DUX_DEMO_SYMBOL_SANS_FONT`, `DUX_DEMO_MATH_FONT` — font overrides for the
  renderer.
- `KEEP_STATE=1` — same as `--keep-state`.

## How it is organized

- `fixtures/` — source files copied into the three demo repositories
- `fixtures/storefront-origin/` — uncommitted changes in the origin worktree
- `fixtures/checkout-api-after/` and `fixtures/billing-service-after/` — files
  the related agents add while the demo runs
- `bin/dux-demo-agent` — deterministic provider transcript and split-out calls
- `bin/gh` — minimal PR shim for dux's GitHub integration
- `setup.sh` — creates repositories, worktrees, config, and seed sessions
- `run.sh` — starts dux with an isolated `DUX_HOME`
- `capture.sh` — drives the demo and renders the screenshot
- `render_terminal.py` — converts a tmux ANSI capture to PNG

Everything is isolated under `DUX_DEMO_ROOT`; the demo never reads or writes the
real `~/.config/dux` state.

## Adding another screenshot scenario

Copy this directory to a new scenario, reuse `env.sh`, `render_terminal.py`, and
the capture pattern, and point the new fixtures at the UI state you want to
show. Keep the output assets under `assets/` so README links stay stable.
