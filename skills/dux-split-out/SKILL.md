---
name: dux-split-out
description: Create a related worktree in another dux project when the work you are doing requires changes in a different repository.
---

# Dux Split Out

Use this skill when the current task in dux requires changes in another
repository and the user wants that work managed as an independent dux agent.

Dux owns worktrees and agent sessions. Do not run `git worktree add` yourself for
this. Ask the running dux instance to create the related worktree so it appears
in the sidebar under **Multi-repo work** and stays linked to this agent.

## Workflow

1. Identify the target repository path and a short kebab-case branch name.
2. Decide whether the target repository is already a dux project. If not, pass
   `--repo` with its path; dux can register it automatically when
   `[agent_control] allow_unregistered_repos` is enabled.
3. Write a self-contained handoff prompt. The child agent starts a fresh
   conversation and only receives this prompt, so include:
   - why the work is needed;
   - the target files or components, when known;
   - acceptance criteria and constraints;
   - what not to touch;
   - the relevant source worktree and branch for context.
4. Run split-out through the dux binary that launched you:

```bash
"${DUX_BIN:-dux}" agent split-out \
  --repo /absolute/path/to/other-repo \
  --name descriptive-branch-name \
  --prompt-file - \
  --json <<'PROMPT'
Explain the cross-repository work here. Include context, files, acceptance
criteria, and boundaries.
PROMPT
```

5. Parse the JSON response. On success, report the new project, branch, worktree,
   session id, and whether the project was newly added. On failure, report the
   `error` and `code` fields and do not modify the target repository yourself.

## Important behavior

- Split-out creates the worktree from the target project's leading branch (or
  `--base <branch>`). Uncommitted changes in the target checkout are **not**
  copied. If the other repository has local work that matters, ask the user to
  commit it or choose the appropriate base branch first.
- The child agent is launched automatically and receives the handoff prompt.
  Dux flattens newlines to spaces before typing it into the child terminal once
  the child starts producing output, so write the prompt as one continuous
  instruction. A provider's first-run trust or onboarding prompt can consume the
  typed text; if the child does not start the work, open it and send the prompt
  again. This agent should not wait for the child or edit its worktree.
- Use `--name` for deterministic retries. If you are unsure whether a previous
  request succeeded after a timeout, run `dux agent related --json` before
  retrying so you do not create duplicate worktrees.
- If `DUX_BIN` and `DUX_SESSION_ID` are not set, you are not running inside a
  dux-managed agent. Ask the user to run dux and start the agent there.

## Options

```text
--repo <path>           Target git checkout (may be auto-registered)
--project <id|name>     Target project already registered in dux
--name <branch>         Branch and worktree name
--base <branch>         Base branch
--provider <name>       Provider override
--prompt <text>         Handoff prompt
--prompt-file <path|->  Read the handoff prompt from a file or stdin
--source <session-id>   Source session (defaults to DUX_SESSION_ID)
--timeout <seconds>     How long to wait for dux (default 120)
--json                  Print the machine-readable response
```
