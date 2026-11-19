#!/usr/bin/env bash
# Regenerate assets/dux-multi-repo.png from a live dux session.
#
# The demo runs inside tmux, so no terminal emulator or X server is required.
# tmux's styled pane capture is rendered to PNG by render_terminal.py.
set -euo pipefail

DEMO_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=env.sh
source "$DEMO_DIR/env.sh"

OUTPUT=$REPO_ROOT/assets/dux-multi-repo.png
KEEP_STATE=${KEEP_STATE:-0}
TMUX=(tmux -L dux-demo-capture)

usage() {
  cat <<'EOF'
Usage: capture.sh [options]

Regenerates the multi-repo screenshot and writes it to
assets/dux-multi-repo.png by default.

Options:
  --output <path>  Write the PNG somewhere else.
  --keep-state     Reuse the current demo state instead of rebuilding it.
  -h, --help       Show this help.

Environment:
  DUX_DEMO_ROOT    Demo work directory (default: $TMPDIR/dux-multi-repo-demo)
  DEMO_DUX_BIN     dux binary to run (default: target/debug/dux)
  KEEP_STATE=1     Same as --keep-state
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --output)
      if [ $# -lt 2 ]; then
        echo "--output requires a path" >&2
        exit 2
      fi
      OUTPUT=$2
      shift 2
      ;;
    --keep-state)
      KEEP_STATE=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

cleanup() {
  "${TMUX[@]}" kill-server 2>/dev/null || true
  pkill -f "$DEMO_DIR/bin/dux-demo-agent" 2>/dev/null || true
}
trap cleanup EXIT

if [ "$KEEP_STATE" != "1" ] || [ ! -f "$DUX_HOME/sessions.sqlite3" ]; then
  "$DEMO_DIR/setup.sh" >/dev/null
fi

"${TMUX[@]}" kill-server 2>/dev/null || true

if [ -f "$DUX_HOME/dux.lock" ]; then
  pid=$(cat "$DUX_HOME/dux.lock" 2>/dev/null || true)
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    echo "A dux instance is already running for $DUX_HOME (pid $pid)." >&2
    echo "Close it before capturing, or set DUX_DEMO_ROOT to a different directory." >&2
    exit 1
  fi
fi

pkill -f "$DEMO_DIR/bin/dux-demo-agent" 2>/dev/null || true

"${TMUX[@]}" -f /dev/null new-session -d -s dux -x 220 -y 56 \
  -c "$DEMO_DIR" ./run.sh
"${TMUX[@]}" set-option -g status off
"${TMUX[@]}" set-option -g default-terminal "tmux-256color"
"${TMUX[@]}" set-option -g allow-rename off
"${TMUX[@]}" set-option -g history-limit 20000

# Wait for the origin and both split-out agents to appear in the sidebar.
for _ in $(seq 1 120); do
  pane=$("${TMUX[@]}" capture-pane -p -t dux 2>/dev/null || true)
  if printf '%s' "$pane" | grep -q 'Multi-repo work' \
    && printf '%s' "$pane" | grep -q 'add-gift-card-api' \
    && printf '%s' "$pane" | grep -q 'add-gift-card-ledger'; then
    break
  fi
  sleep 1
done

# Move the sidebar selection from the storefront project header to the origin
# agent. The footer agent comes first, then the Multi-repo work origin.
for _ in $(seq 1 8); do
  header=$("${TMUX[@]}" capture-pane -p -t dux 2>/dev/null | head -1 || true)
  case "$header" in
    *"agent: feature/gift-cards"*) break ;;
  esac
  "${TMUX[@]}" send-keys -t dux Down
  sleep 0.3
done

# Wait for the PR poller to discover the current PR and both related PRs.
for _ in $(seq 1 90); do
  pane=$("${TMUX[@]}" capture-pane -p -t dux 2>/dev/null || true)
  if printf '%s' "$pane" | grep -q 'Pull Requests (3)' \
    && printf '%s' "$pane" | grep -q 'checkout-api#98' \
    && printf '%s' "$pane" | grep -q 'billing-service#57'; then
    break
  fi
  sleep 1
done

pane=$("${TMUX[@]}" capture-pane -p -t dux 2>/dev/null || true)
if ! printf '%s' "$pane" | grep -q 'Pull Requests (3)'; then
  echo "capture: dux did not reach the expected multi-repo view" >&2
  printf '%s\n' "$pane" >&2
  exit 1
fi

# Let the transcript and status line settle, then capture the styled grid.
sleep 1
ansi=$DEMO_ROOT/dux-multi-repo.ansi
"${TMUX[@]}" capture-pane -ep -t dux >"$ansi"

mkdir -p "$(dirname "$OUTPUT")"
python3 "$DEMO_DIR/render_terminal.py" "$ansi" "$OUTPUT"

echo "Captured:"
echo "  $OUTPUT"
echo "  $ansi"
