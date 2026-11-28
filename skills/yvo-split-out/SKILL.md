---
name: yvo-split-out
description: Create a related worktree in another yvo project, or in the same repository, when the work you are doing requires a separate agent.
---

# Yvo Split Out

Use this skill when the current task in yvo requires a separate child worktree,
either in another repository or in the same repository, and the user wants that
work managed as an independent yvo agent.

Yvo owns worktrees and agent sessions. Do not run `git worktree add` yourself for
this. Ask the running yvo instance to create the related worktree so it appears
in the sidebar under **Delegated work** and stays linked to this agent.

## Workflow

1. Identify the target repository path (or use `--same-repo`) and a short
   kebab-case branch name.
2. Decide whether the target repository is already a yvo project. If not, pass
   `--repo` with its path; yvo can register it automatically when
   `[agent_control] allow_unregistered_repos` is enabled.
3. Choose a base. Same-repo children default to the parent's HEAD
   (`--base-mode parent-head`). Use `--base-mode leading` for the project
   leading branch, or `--base-mode parent-snapshot` to include the parent's
   uncommitted files. Cross-repo children default to the target's leading
   branch.
4. Write a self-contained handoff prompt. The child agent starts a fresh
   conversation and only receives this prompt, so include:
   - why the work is needed;
   - the target files or components, when known;
   - acceptance criteria and constraints;
   - what not to touch;
   - the relevant source worktree and branch for context.
5. Run the delegation through the yvo binary that launched you. Use
   `--same-repo` instead of `--repo` when the child belongs in your repository:

```bash
"${YVO_BIN:-yvo}" agent delegate \
  --repo /absolute/path/to/other-repo \
  --name descriptive-branch-name \
  --brief-file - \
  --json <<'PROMPT'
Explain the work here. Include context, files, acceptance criteria, and
boundaries.
PROMPT
```

6. Parse the JSON response. On success, report the delegation id, project,
   branch, worktree, session id, and whether the project was newly added. On
   failure, report the `error` and `code` fields and do not modify the target
   repository yourself.
7. Wait for the child with a short poll budget. Exit code 2 means the wait timed
   out; call it again. Exit code 4 means the child is blocked and needs an
   answer.

```bash
"${YVO_BIN:-yvo}" agent wait --delegation <id> --timeout 100 --json
"${YVO_BIN:-yvo}" agent result --delegation <id> --json
```

8. If the child reports `blocked`, answer it, then wait again:

```bash
"${YVO_BIN:-yvo}" agent followup --delegation <id> --text-file -
```

## Important behavior

- Cross-repo delegations create the worktree from the target project's leading
  branch (or `--base <ref>`). Uncommitted changes in the target checkout are
  **not** copied. Same-repo children default to the parent's HEAD; use
  `parent-snapshot` for uncommitted parent work.
- The child agent is launched automatically and receives a short prompt that
  points at a durable brief file. Yvo flattens newlines to spaces before typing
  the prompt once the child starts producing output, so keep the typed prompt
  short. A provider's first-run trust or onboarding prompt can consume the typed
  text; if the child does not start the work, open it and send the prompt again.
- A child reports completion with `yvo agent finish --status success --summary-file -`.
  If it ignores the contract, the delegation becomes `exited` and its outcome is
  unknown; inspect the worktree and report that to the user.
- Use `--name` for deterministic retries. If you are unsure whether a previous
  request succeeded after a timeout, run `yvo agent status --json` before
  retrying so you do not create duplicate worktrees.
- If `YVO_BIN` and `YVO_SESSION_ID` are not set, you are not running inside a
  yvo-managed agent. Ask the user to run yvo and start the agent there.

## Options

```text
--repo <path>           Target git checkout (may be auto-registered)
--project <id|name>     Target project already registered in yvo
--same-repo             Target the source agent's repository
--name <branch>         Branch and worktree name
--title <text>          Human-readable delegation title
--base-mode <mode>      leading | ref | parent-head | parent-snapshot | sibling-head
--base <ref>            Explicit base ref
--base-session <id>     Sibling session for sibling-head
--provider <name>       Provider override
--brief <text>          Task brief
--brief-file <path|->   Read the task brief from a file or stdin
--source <session-id>   Source session (defaults to YVO_SESSION_ID)
--wait-for-launch       Block until the child launches
--no-wait               Return after queueing (default for delegate)
--timeout <seconds>     Request timeout for --wait-for-launch
--json                  Print the machine-readable response
```
