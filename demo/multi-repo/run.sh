#!/usr/bin/env bash
# Run the isolated multi-repo demo. Nothing here reads or writes the real
# ~/.config/dux state.
set -euo pipefail

DEMO_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=env.sh
source "$DEMO_DIR/env.sh"

if [ "${1:-}" = "--fresh" ]; then
  shift
  "$DEMO_DIR/setup.sh" >/dev/null
elif [ ! -f "$DUX_HOME/sessions.sqlite3" ]; then
  "$DEMO_DIR/setup.sh"
fi

if [ ! -x "$DEMO_DUX_BIN" ]; then
  echo "dux binary not found at $DEMO_DUX_BIN" >&2
  echo "Build it first with: cargo build" >&2
  exit 1
fi

cd "$DEMO_ROOT"
exec "$DEMO_DUX_BIN" "$@"
