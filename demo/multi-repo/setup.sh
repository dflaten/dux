#!/usr/bin/env bash
# Build the isolated repositories, worktrees, config, and seed sessions for the
# multi-repo screenshot demo. Safe to re-run; all state lives under DEMO_ROOT.
set -euo pipefail

DEMO_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=env.sh
source "$DEMO_DIR/env.sh"

if [ ! -x "$DEMO_DUX_BIN" ]; then
  echo "dux binary not found at $DEMO_DUX_BIN" >&2
  echo "Build it first with: cargo build" >&2
  exit 1
fi
command -v tmux >/dev/null 2>&1 || {
  echo "tmux is required to initialize and capture the demo." >&2
  exit 1
}
command -v python3 >/dev/null 2>&1 || {
  echo "python3 is required to seed the demo database." >&2
  exit 1
}

REPOS=$DEMO_ROOT/repos
WORKTREES=$DUX_HOME/worktrees
INIT_TMUX=dux-demo-init

export GIT_AUTHOR_NAME="dux demo"
export GIT_AUTHOR_EMAIL="demo@dux.local"
export GIT_COMMITTER_NAME="dux demo"
export GIT_COMMITTER_EMAIL="demo@dux.local"

tmux -L "$INIT_TMUX" kill-server 2>/dev/null || true
tmux -L dux-demo-capture kill-server 2>/dev/null || true

# Never delete the demo state out from under a running dux instance.
if [ -f "$DUX_HOME/dux.lock" ]; then
  pid=$(cat "$DUX_HOME/dux.lock" 2>/dev/null || true)
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    echo "A dux instance is already running for $DUX_HOME (pid $pid)." >&2
    echo "Close it before rebuilding the demo, or set DUX_DEMO_ROOT to a different directory." >&2
    exit 1
  fi
fi

pkill -f "$DEMO_DIR/bin/dux-demo-agent" 2>/dev/null || true

rm -rf "$REPOS" "$DUX_HOME"
mkdir -p "$REPOS" "$WORKTREES" "$DUX_HOME"

create_repo() {
  local name=$1
  local dir=$REPOS/$name
  mkdir -p "$dir"
  cp -R "$DEMO_DIR/fixtures/$name/." "$dir/"
  git -C "$dir" init -q -b main
  git -C "$dir" remote add origin "git@github.com:acme/$name.git"
  git -C "$dir" add -A
  git -C "$dir" commit -qm "$name baseline"
}

create_repo storefront
create_repo checkout-api
create_repo billing-service

# The origin agent already has storefront changes when dux starts.
git -C "$REPOS/storefront" worktree add -q \
  -b feature/gift-cards \
  "$WORKTREES/storefront/gift-cards" main
cp -R "$DEMO_DIR/fixtures/storefront-origin/." "$WORKTREES/storefront/gift-cards/"

# A second, unrelated storefront agent keeps the project tree visible while the
# origin agent appears under Multi-repo work.
git -C "$REPOS/storefront" worktree add -q \
  -b feature/footer-spacing \
  "$WORKTREES/storefront/footer-spacing" main

# Render config.toml with the concrete demo root so the database never has to
# expand a placeholder path.
template=$(cat "$DEMO_DIR/config.toml.in")
demo_root_toml=${DEMO_ROOT//\\/\\\\}
demo_root_toml=${demo_root_toml//\"/\\\"}
printf '%s\n' "${template//__DEMO_ROOT__/$demo_root_toml}" >"$DUX_HOME/config.toml"

db_ready() {
  python3 - "$DUX_HOME/sessions.sqlite3" >/dev/null 2>&1 <<'PY'
import sqlite3
import sys

conn = sqlite3.connect(sys.argv[1])
tables = {row[0] for row in conn.execute("select name from sqlite_master where type='table'")}
raise SystemExit(0 if {"projects", "agent_sessions"} <= tables else 1)
PY
}

# Open dux once inside tmux so it creates and migrates the session database.
# The loop watches for the schema instead of assuming a fixed startup delay.
# Run through a relative symlink so a repo path with spaces does not get split
# by tmux's shell command handling.
ln -sf "$DEMO_DUX_BIN" "$DEMO_ROOT/dux-demo-bin"
tmux -L "$INIT_TMUX" -f /dev/null new-session -d -s init -x 200 -y 50 \
  -c "$DEMO_ROOT" ./dux-demo-bin
for _ in $(seq 1 100); do
  db_ready && break
  sleep 0.1
done
tmux -L "$INIT_TMUX" kill-server 2>/dev/null || true
rm -f "$DEMO_ROOT/dux-demo-bin"

if ! db_ready; then
  echo "dux did not initialize $DUX_HOME/sessions.sqlite3" >&2
  echo "Check $DUX_HOME/dux.log for details." >&2
  exit 1
fi

python3 - "$DUX_HOME/sessions.sqlite3" "$DEMO_ROOT" <<'PY'
import os
import sqlite3
import sys
from datetime import datetime, timezone

db, root = sys.argv[1], sys.argv[2]
home = os.path.join(root, "home")
storefront = os.path.join(root, "repos", "storefront")
now = datetime.now(timezone.utc).isoformat()

conn = sqlite3.connect(db)
conn.execute("update projects set leading_branch = 'main'")

rows = [
    (
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        "11111111-1111-4111-8111-111111111111",
        storefront,
        "claude",
        "main",
        "feature/gift-cards",
        os.path.join(home, "worktrees", "storefront", "gift-cards"),
        "Gift cards across checkout",
        '["claude"]',
        1,
        "detached",
    ),
    (
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        "11111111-1111-4111-8111-111111111111",
        storefront,
        "claude",
        "main",
        "feature/footer-spacing",
        os.path.join(home, "worktrees", "storefront", "footer-spacing"),
        "Tighten footer spacing",
        "[]",
        0,
        "exited",
    ),
]

for session_id, project_id, project_path, provider, source_branch, branch, worktree, title, started, desired, status in rows:
    conn.execute(
        """
        insert or replace into agent_sessions
            (id, project_id, project_path, provider, source_branch, branch_name,
             worktree_path, title, started_providers, provider_session_ids,
             desired_running, auto_reopen_enabled, status, group_id,
             parent_session_id, created_at, updated_at)
        values (?, ?, ?, ?, ?, ?, ?, ?, ?, '{}', ?, 1, ?, null, null, ?, ?)
        """,
        (
            session_id,
            project_id,
            project_path,
            provider,
            source_branch,
            branch,
            worktree,
            title,
            started,
            desired,
            status,
            now,
            now,
        ),
    )

conn.commit()
print(f"seeded {len(rows)} sessions")
PY

echo "Demo ready under $DEMO_ROOT"
echo "  dux binary: $DEMO_DUX_BIN"
echo "  run:        $DEMO_DIR/run.sh --fresh"
echo "  capture:    $DEMO_DIR/capture.sh"
