---
name: dux-orchestrate
description: Delegate parts of a coding task to child agents in dux, then wait for their results and integrate or follow up. Use when a task spans multiple worktrees, repositories, or independent chunks of work.
---

# Dux Orchestrate

Use this skill when the current task is large enough that independent child
agents should work in parallel or in separate repositories. Dux owns the
worktrees, branches, and sessions; do not run `git worktree add` yourself.

## Workflow

1. Identify independent chunks. Each chunk should have a clear deliverable and
   a small set of files or a single repository.
2. Decide the base for each child:
   - `--same-repo --base-mode parent-head` to continue from your committed work.
   - `--same-repo --base-mode leading` to start from the project leading branch.
   - `--same-repo --base-mode parent-snapshot` to include your uncommitted work.
   - `--project <name>` for another dux project.
   - `--repo <path>` for an unregistered checkout.
3. Write a self-contained brief. The child does not inherit your conversation.
   Include context, target files, acceptance criteria, boundaries, and the
   source branch/worktree for reference.
4. Spawn the children. `delegate` returns after queueing by default:

```bash
"${DUX_BIN:-dux}" agent delegate \
  --same-repo \
  --title "Add ledger entries" \
  --name ledger-entries \
  --base-mode parent-head \
  --brief-file - \
  --json <<'BRIEF'
Implement the ledger entries and tests. Do not touch the checkout UI.
BRIEF
```

5. Wait for results. Keep each wait below your provider's tool timeout. Exit
   code 2 means the wait timed out and you should call it again, not that the
   children failed:

```bash
"${DUX_BIN:-dux}" agent wait --all --timeout 100 --json
```

6. Read the results and inspect the delegated diffs in dux:

```bash
"${DUX_BIN:-dux}" agent result --delegation <id> --json
```

7. If a child is `blocked`, answer it and wait again:

```bash
"${DUX_BIN:-dux}" agent followup --delegation <id> --text-file -
```

8. Integrate completed same-repo children only after review:

```bash
"${DUX_BIN:-dux}" agent integrate --session <child-session-id> --json
```

Cross-repository children land through their own commits and pull requests.
Dux does not merge across repositories.

## Rules

- Work only in your own worktree. Do not edit a child's worktree.
- Give each child a non-overlapping scope when possible. Parallel children in
  one repository can conflict when integrated.
- Use `--name` for deterministic retries. If a request times out, retry with
  the same name/request key instead of creating duplicate work.
- Do not use `git worktree add`, `git branch -D`, or manual worktree cleanup.
- Report the child ids, results, and any integration outcome to the user.
- If `DUX_BIN` or `DUX_SESSION_ID` is missing, you are not running inside dux;
  ask the user to start the agent from dux.

## Command Reference

```text
dux agent delegate [options]
  --same-repo | --project <id|name> | --repo <path>
  --title <text> --name <branch> --brief-file <path|->
  --base-mode leading|ref|parent-head|parent-snapshot|sibling-head
  --base <ref> --base-session <id>
  --provider <name> --request-key <key>
  --wait-for-launch --no-wait --timeout <seconds> --json

dux agent status [--session <id>|--delegation <id>] [--json]
dux agent tree   [--session <id>|--delegation <id>]
dux agent wait   [--all|--any|--until started] [--timeout <seconds>]
dux agent result --delegation <id> [--full] [--json]
dux agent finish --status success|failed|blocked --summary-file <path|->
dux agent followup --delegation <id> --text-file <path|->
dux agent cancel --delegation <id> [--subtree]
dux agent integrate --session <child-session-id>
```
