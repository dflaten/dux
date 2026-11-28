#!/usr/bin/env bash
# Run the isolated multi-repo demo. Nothing here reads or writes the real
# ~/.config/yvo state.
set -euo pipefail

DEMO_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=env.sh
source "$DEMO_DIR/env.sh"

if [ "${1:-}" = "--fresh" ]; then
  shift
  "$DEMO_DIR/setup.sh" >/dev/null
elif [ ! -f "$YVO_HOME/sessions.sqlite3" ]; then
  "$DEMO_DIR/setup.sh"
fi

if [ ! -x "$DEMO_YVO_BIN" ]; then
  echo "yvo binary not found at $DEMO_YVO_BIN" >&2
  echo "Build it first with: cargo build" >&2
  exit 1
fi

cd "$DEMO_ROOT"
exec "$DEMO_YVO_BIN" "$@"
