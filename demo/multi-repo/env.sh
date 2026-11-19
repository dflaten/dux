#!/usr/bin/env bash
# Shared environment for the multi-repo screenshot demo. Source this file,
# do not execute it directly.

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  echo "env.sh is meant to be sourced by the demo scripts." >&2
  exit 1
fi

DEMO_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(git -C "$DEMO_DIR" rev-parse --show-toplevel)

DEMO_ROOT=${DUX_DEMO_ROOT:-${TMPDIR:-/tmp}/dux-multi-repo-demo}
mkdir -p "$DEMO_ROOT"
DEMO_ROOT=$(cd "$DEMO_ROOT" && pwd)

DEMO_DUX_BIN=${DEMO_DUX_BIN:-$REPO_ROOT/target/debug/dux}
if [ ! -x "$DEMO_DUX_BIN" ] && [ -x "$REPO_ROOT/target/release/dux" ]; then
  DEMO_DUX_BIN=$REPO_ROOT/target/release/dux
fi

export DEMO_DIR REPO_ROOT DEMO_ROOT DEMO_DUX_BIN
export DUX_HOME="$DEMO_ROOT/home"
export DUX_SOCKET="$DUX_HOME/dux.sock"
export DUX_DEMO_ROOT="$DEMO_ROOT"
export DUX_DEMO_FIXTURES="$DEMO_DIR/fixtures"
export PATH="$DEMO_DIR/bin:$PATH"
