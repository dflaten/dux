#!/usr/bin/env bash
# Shared environment for the multi-repo screenshot demo. Source this file,
# do not execute it directly.

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  echo "env.sh is meant to be sourced by the demo scripts." >&2
  exit 1
fi

DEMO_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(git -C "$DEMO_DIR" rev-parse --show-toplevel)

DEMO_ROOT=${YVO_DEMO_ROOT:-${TMPDIR:-/tmp}/yvo-multi-repo-demo}
mkdir -p "$DEMO_ROOT"
DEMO_ROOT=$(cd "$DEMO_ROOT" && pwd)

DEMO_YVO_BIN=${DEMO_YVO_BIN:-$REPO_ROOT/target/debug/yvo}
if [ ! -x "$DEMO_YVO_BIN" ] && [ -x "$REPO_ROOT/target/release/yvo" ]; then
  DEMO_YVO_BIN=$REPO_ROOT/target/release/yvo
fi

export DEMO_DIR REPO_ROOT DEMO_ROOT DEMO_YVO_BIN
export YVO_HOME="$DEMO_ROOT/home"
export YVO_SOCKET="$YVO_HOME/yvo.sock"
export YVO_DEMO_ROOT="$DEMO_ROOT"
export YVO_DEMO_FIXTURES="$DEMO_DIR/fixtures"
export PATH="$DEMO_DIR/bin:$PATH"
