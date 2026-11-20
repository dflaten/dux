# Orchestrator Agents and Delegation Trees

Status: Implemented (core); deferred polish tracked below  
Date: 2026-11-20  
Scope: `dux` TUI, `dux` CLI, agent control socket, SQLite state, UI

## Implementation status

The core delegation pipeline is implemented:

- durable `agent_delegations` state, frozen base refs, and result files;
- same-repo and cross-repo delegation, including `leading`, `ref`,
  `parent-head`, `parent-snapshot`, and `sibling-head` bases;
- `delegate`, `status`, `tree`, `wait`, `result`, `finish`, `followup`,
  `message`, `cancel`, `retry`, and `integrate`;
- recursive sidebar rendering, a Delegations right-pane section, and palette
  actions for cancel and integrate;
- `[orchestration]` policy and generated config documentation;
- unit coverage for storage, snapshots, base resolution, tree building, and
  finish/result updates.

Still deferred from the full proposal:

- quiet-period follow-up delivery (follow-ups currently type immediately when
  the child PTY is alive);
- the interactive delegate-agent modal, editable mission text, result viewer,
  role badges, and search by delegation state;
- `allowed_repo_roots` and narrower agent-control repository policy;
- snapshot support for explicitly included ignored files;
- parent PTY notifications and a persistent follow-up delivery queue.

## TL;DR

Dux already has the seed of this feature: `dux agent split-out` lets an agent ask
the running TUI to create a related worktree in another project, launch a child
agent in it, and group both agents under **Multi-repo work**.

This design generalizes that primitive into a **delegation tree**:

- Any agent can become an **orchestrator** by delegating work to child agents.
- Children are ordinary dux agents in ordinary worktrees, so they can delegate
  further and form arbitrarily deep trees.
- Delegations can target the **same repository** or a **different repository**.
- The base for a child is explicit and frozen: project leading branch, an
  arbitrary ref, the parent's HEAD, a parent working-tree snapshot, or a
  sibling's HEAD.
- Each delegation has durable state, a durable brief, a completion contract, and
  a result. The parent can wait, read results, answer blockers, request another
  iteration, cancel a subtree, and integrate same-repo work.
- Dux remains the only writer of worktrees, sessions, and state. Agents still
  talk to dux through the existing JSON-over-Unix-socket CLI. There is no agent
  protocol and no new adapter layer.

The core interaction is **task-level turn-taking**, not live chat:

1. Orchestrator spawns a child with a self-contained brief.
2. Child works, then calls `dux agent finish` with `success`, `failed`, or
   `blocked`.
3. Orchestrator calls `dux agent wait`, receives the terminal result, inspects
   the diff/result, then either integrates, sends a follow-up iteration, or
   cancels.

That contract is reliable across Claude, Codex, OpenCode, and any other PTY
provider because it does not depend on detecting whether a TUI is "idle" or
parsing provider output.

---

## 1. What exists today

### 1.1 Control plane

`src/ipc.rs` implements a user-only Unix socket at `<config_dir>/dux.sock`.
`src/cli.rs` exposes `dux agent split-out` and `dux agent related`. The CLI
sends a `ControlRequest`, the TUI's `src/app/control.rs` handles it, the work
runs on the existing worker paths, and a JSON `ControlResponse` is written back.

Relevant constraints in the current implementation:

- `MAX_CONTROL_CONNECTIONS = 8`.
- `DEFAULT_CONTROL_TIMEOUT = 120s`; the server caps a request at 600s.
- `split_out_in_flight` and `create_agent_in_flight` are global booleans, so only
  one related worktree can be created at a time.
- The handoff prompt is flattened to one line and typed into the child PTY after
  the child produces its first output (`write_bytes_after_first_output`).
- `DUX_BIN`, `DUX_SESSION_ID`, `DUX_AGENT_ID`, `DUX_PROJECT_ID`,
  `DUX_WORKTREE_PATH`, `DUX_BRANCH`, `DUX_PROVIDER`, and `DUX_AGENT_TITLE` are
  injected into agent PTYs.

### 1.2 Worktree creation

`git::create_worktree_from_start_point` already accepts an arbitrary start point,
so branching from a parent HEAD, a sibling branch, or a commit SHA is mostly a
matter of resolving the right ref. `git::mirror_worktree_contents` and
`git::copy_worktree_contents_into` already provide snapshot-style copying for
forks.

### 1.3 Persistent tree data

`AgentSession` has `group_id` and `parent_session_id`. `AgentGroup` stores the
group label and origin session. SQLite already persists both, including archived
sessions. The sidebar renders one origin plus a flat list of children under
**Multi-repo work**.

### 1.4 Diff base

`App::diff_base_ref_for_session` currently derives the diff base from the
project's leading branch, falling back to `session.source_branch`. That means a
same-repo child branched from its parent would show the parent's changes in the
child's diff. A delegation-aware diff base is required.

### 1.5 What split-out does not do

- No same-repo topology: targeting the source project still branches from the
  project leading branch, not from the parent.
- No task identity, lifecycle, result, or wait primitive.
- No fan-out: global in-flight booleans serialize creation.
- No nested delegation semantics or limits.
- No completion signal; the parent cannot tell success from a provider crash.
- No follow-up loop, cancellation, or integration.
- No UI hierarchy beyond one flat row per child.
- Prompt delivery is fragile: a first-run trust prompt can eat the typed text,
  and the prompt must fit the socket limit and survive newline flattening.

---

## 2. Goals and non-goals

### Goals

1. Let an agent decompose a task into multiple child tasks in the same or other
   repositories, and let those children decompose further.
2. Keep every child a real dux agent: own worktree, own branch, own provider,
   own PTY, own diff, own companion terminals.
3. Give the parent a reliable way to know when a child is done, read its result,
   ask for another iteration, and collect its changes.
4. Make same-repo delegation useful: the child can start from the parent's
   committed state (and optionally its working tree), with a stable diff base.
5. Make cross-repo delegation useful: the child starts from the target project's
   leading branch or an explicit ref, and reports PRs like any other agent.
6. Persist the tree and task state across TUI restarts.
7. Render the tree and task state in the UI without turning the sidebar into a
   log viewer.
8. Keep the design safe: bounded fan-out, explicit cancellation, no automatic
   merging, no automatic worktree deletion.

### Non-goals

- No agent-to-agent RPC, JSON-RPC, ACP, MCP message bus, or adapter binary.
  Agents still talk to dux through the CLI and the local control socket.
- No automatic merge or auto-commit of delegated work. Integration is explicit
  and user-authorized.
- No cross-repository history merge. Cross-repo work lands through commits and
  pull requests.
- No scheduler for arbitrary DAGs in v1. The supported shape is a tree of agent
  sessions with sequential turn-taking; pipelines are expressed by branching a
  child from a sibling's frozen HEAD.
- No live streaming of child transcripts into the parent. The parent reads
  results and diffs, and can open the child agent when it needs to see the
  terminal.
- "Subtree" here means a subtree of agent work, not `git subtree`. If a project
  wants literal `git subtree`, agents can run it inside their own worktree; dux
  does not special-case it.

---

## 3. Terminology

| Term | Meaning |
| --- | --- |
| **Mission** | The whole tree of work rooted at one origin agent. Backed by `agent_groups`. |
| **Orchestrator** | An agent session that has at least one child. Not a separate provider or session type. |
| **Delegation** | One parent-to-child task. Stores the brief, target, base, state, and result. |
| **Worker** | An agent session created by a delegation. Workers can themselves delegate. |
| **Claim / brief** | The durable, self-contained task description stored outside the repository. |
| **Result** | The child's structured report, written to a durable file and summarized in SQLite. |
| **Base mode** | How the child's worktree start point is chosen: leading, ref, parent-head, parent-snapshot, sibling-head. |
| **Fork point** | The frozen commit SHA the child branch starts from. Used as the child's diff base. |
| **Integration** | Applying a same-repo child's work to its parent. Explicit, never automatic. |

The UI can keep calling the section **Multi-repo work** temporarily, but this
design recommends renaming it to **Delegated work** because same-repo trees are
now first-class.

---

## 4. Architecture

```text
┌──────────────────────────────┐
│ Orchestrator agent PTY       │
│  shell: dux agent delegate   │
│  shell: dux agent wait       │
└──────────────┬───────────────┘
               │ JSON over 0600 Unix socket
               ▼
┌──────────────────────────────┐
│ dux CLI                      │
│  delegate / status / wait    │
│  result / finish / followup  │
│  cancel / integrate          │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐        ┌───────────────────────────┐
│ dux TUI                      │        │ SQLite                    │
│  control.rs: validate/policy │◄──────►│  agent_groups             │
│  queue + workers             │        │  agent_delegations        │
│  UI tree + task pane         │        │  agent_sessions.base_ref  │
└──────┬───────────────┬───────┘        └───────────────────────────┘
       │               │
       │ git worktree  │ PTY spawn
       ▼               ▼
┌──────────────┐  ┌──────────────────────────────┐
│ Worktrees    │  │ Worker agent PTY             │
│ parent/child │  │  reads $DUX_BRIEF_FILE       │
│ other repo   │  │  runs dux agent finish       │
└──────────────┘  └──────────────┬───────────────┘
                                 │
                                 ▼
                  durable result file + control callback
```

### Control plane vs data plane

- **Control plane:** CLI ↔ TUI socket. Short JSON requests. The TUI owns all
  mutations, policy checks, and persistence.
- **Data plane:** the child's PTY. Dux types the initial prompt and follow-ups
  into it exactly like a user would. Dux never parses provider output for
  correctness.
- **Durable side channel:** briefs, results, and inboxes live under
  `<config_dir>/delegations/`. They survive a missed socket callback and keep
  large prompts out of the 256 KiB control message limit.

### Why polling for `wait`

`dux agent wait` is a CLI-side polling loop over `dux agent status`, not a
server-side long-poll. This:

- avoids holding one of the limited control connections for minutes;
- avoids adding a waiter registry and wake-up logic to the TUI event loop;
- survives a TUI restart because the CLI reconnects and reads persisted state;
- lets the same mechanism work for `--any`, `--all`, and single sessions.

Backoff starts at 250 ms and caps at 2 s. The TUI stays responsive because it
only handles short status requests.

---

## 5. Data model

All runtime state belongs in SQLite. Config stores policy only.

### 5.1 `agent_groups` (mission)

The core implementation keeps the existing `label`, `origin_session_id`, and
timestamps. An editable mission statement is deferred polish; the group label is
derived from the origin agent's title or branch. A future migration can add
`mission TEXT` without changing the delegation table or tree lookup.

### 5.2 New `agent_delegations`

```sql
CREATE TABLE IF NOT EXISTS agent_delegations (
    id                TEXT PRIMARY KEY,
    group_id          TEXT NOT NULL,
    parent_session_id TEXT NOT NULL,
    child_session_id  TEXT UNIQUE,
    depth             INTEGER NOT NULL DEFAULT 1,
    iteration         INTEGER NOT NULL DEFAULT 1,
    title             TEXT NOT NULL,
    request_key       TEXT,
    brief_path        TEXT NOT NULL,
    brief_sha256      TEXT,
    project_id        TEXT,
    repo_path         TEXT,           -- resolved during preparation
    base_mode         TEXT NOT NULL,
    base_ref          TEXT,           -- resolved during preparation
    base_label        TEXT,           -- resolved during preparation
    requested_name    TEXT,
    provider          TEXT,
    state             TEXT NOT NULL,
    state_reason      TEXT,
    result_id         TEXT,
    result_status     TEXT,
    result_summary    TEXT,
    result_path       TEXT,
    error             TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    started_at        TEXT,
    finished_at       TEXT,
    integrated_at     TEXT,
    integration_commit TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_delegations_request
    ON agent_delegations(parent_session_id, request_key)
    WHERE request_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_delegations_group
    ON agent_delegations(group_id);
CREATE INDEX IF NOT EXISTS idx_delegations_child
    ON agent_delegations(child_session_id);
```

The session table remains the canonical tree (`parent_session_id`). The
delegation row carries task metadata and the state that exists before, during,
and after the child session.

### 5.3 Session lookup

The implementation keeps the frozen base on `agent_delegations` and resolves it
through `child_session_id`; no extra columns are added to `agent_sessions`.
`App::delegation_for_session` is the lookup used by the diff pane, base-update
indicator, and tree rendering.

- `source_branch` is the human-readable base label, e.g. `main`,
  `parent: gift-cards`, or `sibling: ledger-tests`.
- `delegation.base_ref` is the frozen fork point used by the diff pane and
  base-relative UI. Legacy sessions have no delegation and keep the existing
  project-leading fallback.
- `delegation.child_session_id` binds the task row to the session once it
  launches. If a child reports before the launch event is processed,
  `DUX_DELEGATION_ID` lets the TUI bind it early.

`App::diff_base_ref_for_session` prefers the delegation base when present. This
makes a same-repo child's right pane show only the child's work rather than the
parent's whole branch. The base-update indicator deliberately treats delegated
bases as frozen: it does not fetch `origin/<sha>` and does not suggest rebasing
onto the project leading branch.

### 5.4 Delegation state

```text
queued ──► preparing ──► starting ──► running
   │           │            │            │
   │           │            │            ├──► completed
   │           │            │            ├──► failed
   │           │            │            ├──► blocked
   │           │            │            ├──► cancelled
   │           │            │            ├──► exited
   │           │            │            └──► interrupted
   │           │            └──► failed
   │           └──► failed
   └──► cancelled

blocked | completed | failed ──followup──► resuming ──delivered──► running
                                                    └──failure──► previous state
interrupted ──reattach──► running
```

| State | Meaning | `wait` returns? |
| --- | --- | --- |
| `queued` | Accepted, waiting for a launch slot. | No |
| `preparing` | Resolving project/refs, creating the worktree. | No |
| `starting` | Worktree exists, startup command or PTY spawn in progress. | No |
| `resuming` | Follow-up queued, waiting for a quiet child PTY to deliver it. | No |
| `running` | Child PTY is alive and the child has not finished. | No |
| `completed` | Child reported success. | Yes |
| `failed` | Child reported failure, or launch/startup failed. | Yes |
| `blocked` | Child needs a decision. Resumable with `followup`. | Yes |
| `cancelled` | Parent or user cancelled. Worktree is kept. | Yes |
| `exited` | PTY ended without a `finish` call. Outcome unknown. | Yes |
| `interrupted` | Dux restarted or lost the PTY while running. | Yes |

`completed`, `failed`, and `blocked` are resumable terminal states: a follow-up
moves the delegation to `resuming` and then `running` with a new iteration.
`resuming` is non-terminal so a `wait` issued immediately after `followup` does
not return the stale previous result. `cancelled` is final
unless the user creates a new delegation. Worktrees are never removed
automatically.

### 5.5 Durable files

```text
<config_dir>/delegations/
  pending/<uuid>.md              # staged brief from the CLI, cleaned on startup
  <delegation-id>/
    brief.md                     # generated preamble + user brief
    result.json                  # latest result; stable path for every iteration
    result-1.json                # archived copy after the TUI accepts iteration 1
    result-2.json                # archived copy of later iterations
    inbox.md                     # best-effort follow-up / notification log
    meta.json                    # non-authoritative recovery copy
```

Directory mode `0700`, file mode `0600`. Files are written with
write-to-temp-then-rename.

The brief is durable because:

- It survives PTY prompt loss, first-run onboarding prompts, and terminal
  scrollback limits.
- It keeps large briefs out of the 256 KiB socket message.
- It gives the child a stable reference it can re-read after context compaction.
- It lets the parent's task description outlive the child's provider session.

### 5.6 Environment injected into delegated agents

In addition to the existing `DUX_*` variables:

| Variable | Purpose |
| --- | --- |
| `DUX_DELEGATION_ID` | Delegation id. |
| `DUX_PARENT_SESSION_ID` | Parent session id. |
| `DUX_ROOT_SESSION_ID` | Mission origin session id. |
| `DUX_GROUP_ID` | Mission/group id. |
| `DUX_DELEGATION_DEPTH` | 1 for a direct child. |
| `DUX_BRIEF_FILE` | Absolute path to the generated brief. |
| `DUX_RESULT_FILE` | Absolute path where `dux agent finish` writes the result. |
| `DUX_INBOX_FILE` | Absolute path to the best-effort message log. |
| `DUX_PARENT_WORKTREE` | Parent's worktree, for context only. |
| `DUX_PARENT_BRANCH` | Parent's branch, for context only. |

These are additional to the existing `DUX_WORKTREE_PATH`, `DUX_BRANCH`,
`DUX_BIN`, `DUX_SESSION_ID`, and friends.

---

## 6. Spawning a delegation

### 6.1 CLI

`dux agent split-out` remains, but becomes an alias for the general command:

```bash
dux agent delegate \
  --same-repo \
  --title "Add gift-card ledger entries" \
  --name gift-card-ledger \
  --base-mode parent-head \
  --brief-file - \
  --json
```

Target selection:

```text
--repo <path>            Target checkout; may be auto-registered
--project <id|name>      Registered project
--same-repo              Sugar for the source session's project
```

Base selection:

```text
--base-mode leading          Target project's leading branch (default cross-repo)
--base-mode ref              Use --base <branch|tag|sha>
--base-mode parent-head      Branch from the parent worktree's HEAD (default same-repo)
--base-mode parent-snapshot  Branch from a synthetic commit of the parent worktree
--base-mode sibling-head     Branch from another session's HEAD (--base-session)
--base <ref>                 Explicit ref for ref mode, or override
--base-session <session-id>  Sibling session for sibling-head
```

Other options:

```text
--title <text>            Human-readable task title
--brief <text>            Inline brief; staged on disk like any other brief
--brief-file <path|->     Brief file; staged on disk outside the socket
--name <branch>           Branch and worktree name
--provider <name>         Provider override
--request-key <key>       Idempotency key (defaults to the branch name)
--wait-for-launch         Block until the child is running or has failed
--no-wait                 Explicitly return after queue acceptance (default)
--timeout <seconds>       Client timeout for --wait-for-launch (default 120)
--json                    Machine-readable response
```

A new delegation requires a non-empty brief (`--brief` or `--brief-file`); a
title alone is not enough. The legacy `split-out` alias keeps allowing an empty
prompt for compatibility. `--prompt` and `--prompt-file` remain aliases for
`--brief` and `--brief-file` so existing skills keep working.

Brief staging keeps large prompts off the socket and keeps the TUI from trusting
an arbitrary path:

1. The CLI checks `brief_max_bytes`, writes the brief to
   `<config_dir>/delegations/pending/<uuid>.md` with mode `0600`, and sends the
   path plus SHA-256.
2. The TUI canonicalizes the path, requires it to be inside the pending
   directory, verifies the hash and size, and moves it to
   `<delegation-id>/brief.md`.
3. Pending files older than 24 hours are removed at startup. A rejected request
   leaves its staged file for that cleanup, not for another delegation.

Inline `--brief` uses the same staging path, so the 256 KiB socket limit only
applies to control metadata, not task content.

The rest of the surface, covered later in this document:

```text
dux agent status [--session <id>] [--delegation <id>] [--json]
dux agent tree   [--session <id>] [--delegation <id>] [--json]
dux agent result --session <id> | --delegation <id> [--json]
dux agent wait   [--session <id> | --delegation <id> | --all | --any]
                 [--until started] [--timeout <s>] [--allow-empty]
dux agent finish [--session <id>] --status success|failed|blocked ...
dux agent followup --session <id> | --delegation <id> --text-file <path|->
dux agent message  --session <id> | --delegation <id> --text-file <path|->
dux agent cancel   --session <id> | --delegation <id> [--subtree]
dux agent retry    --delegation <id>
dux agent integrate --session <id> --into <parent|session-id> --strategy ...
```

`dux agent related` remains as the compatibility endpoint for the existing
flat related-agent listing; it returns the subtree as well.

### 6.2 Target resolution

1. Resolve `--project` by id or case-insensitive name.
2. Else resolve `--repo` to a canonical git root. Reject paths inside
   `dux`'s data directory. If the repo is not registered, require
   `[agent_control] allow_unregistered_repos = true`.
3. Else use the source session's project for `--same-repo`.
4. Reject Scratch as a target; Scratch has no git worktree.
5. Validate git repo, project path exists, provider exists, and policy allows
   same-repo/cross-repo work.
6. When `[agent_control] allowed_repo_roots` is non-empty, expand `~` and
   environment variables, canonicalize every existing root, and require the
   canonical target path to be inside one root using path-component comparison,
   not string-prefix matching. This prevents `/code/app-evil` from matching
   `/code/app` and prevents symlink escapes.

Same-repo detection uses the canonical repository root, not project ids, so an
auto-registered path that duplicates an existing project still behaves
predictably. Duplicate project registration should be rejected as it is today.

### 6.3 Base resolution

Resolve a base to a **frozen commit SHA** during preparation, before the worktree
is created. The delegation row can exist in `queued` with a null `base_ref`
until then; `status` reports the base as pending:

| Mode | Same repo | Cross repo | Resolution |
| --- | --- | --- | --- |
| `leading` | yes | yes | `leading_branch`, then `current_branch`, then `main`; if only `origin/<name>` exists, resolve that remote-tracking ref to a SHA. |
| `ref` | yes | yes | Resolve any ref `git rev-parse --verify` accepts, including remote-tracking refs, tags, and commit SHAs. |
| `parent-head` | yes | no | `git rev-parse <parent-worktree> HEAD`. |
| `parent-snapshot` | yes | no | Synthetic commit built from the parent worktree (see below). |
| `sibling-head` | yes | no | `git rev-parse <sibling-worktree> HEAD`; sibling must be in the same repo and the same mission. Uncommitted sibling work is not included and triggers the same warning as `parent-head`. |

Why freeze the SHA:

- The child's diff base must not move when the parent or leading branch advances.
- Reproducible diffs, results, and integration.
- `git worktree add -b <name> <sha>` works for a branch, tag, or commit.

The delegation-created session stores the description in `source_branch`
(`main`, `parent: gift-cards`) and the frozen SHA in `base_ref`. Existing
creation paths that do not set `base_ref` keep their current behavior.

**Dirty parent warning.** For `parent-head`, if the parent worktree has tracked
or untracked changes, the response and UI warn that those changes are not
included. The delegation still succeeds. `parent-snapshot` is the explicit
opt-in for uncommitted work. Detection must use `git status --porcelain=v1 -z`,
not `has_tracked_changes`, which ignores untracked files.

#### Parent snapshot

`parent-snapshot` builds a synthetic commit from the parent worktree without
touching the parent:

1. Create a temporary index file.
2. Seed it from `HEAD`.
3. `git add -A` with that index and `GIT_WORK_TREE` pointing at the parent
   worktree.
4. `git write-tree`; if the tree equals `HEAD`'s tree, use `HEAD` as the
   baseline instead of creating an empty commit.
5. Otherwise `git commit-tree` with a neutral `dux <dux@localhost>` identity
   passed via `-c user.name` / `-c user.email`, matching the existing scratch
   init path. A machine with no git identity must not break snapshot creation.
6. Create the child worktree branch at that baseline commit.

This captures tracked modifications plus untracked, non-ignored files. Ignored
files (`.env`, `node_modules`, `target`) are not copied; a project's startup
command is responsible for local setup, exactly as it is for a fresh agent
worktree. The baseline commit is reachable from the child branch, so it cannot
be garbage-collected. Because the snapshot is built in a temporary index, the
parent worktree's index, HEAD, and working files are untouched.

Integration for a snapshot child uses a patch of `baseline..child`, not a merge,
because the baseline contains the parent's uncommitted work and merging it back
would duplicate that work.

### 6.4 Branch and worktree naming

- `--name` is validated with the existing `git::is_valid_agent_name`.
- Generated names remain docker-style pet names.
- Worktree path remains `worktrees/<project>/<name>`.
- For same-repo delegations, the UI shows the parent branch next to the child
  branch. Dux does not require a slash-style branch name, because that would
  create nested worktree directories and complicate cleanup. The relationship
  is stored in SQLite, not encoded in the branch name.
- If the branch already exists:
  - If the request key/branch matches an existing delegation under the same
    parent, return that delegation as a deduplicated success.
  - Otherwise fail with `branch_exists` and suggest another name.
- Idempotency applies to terminal delegations too: a retry with the same request
  key returns the existing delegation and its state rather than creating a second
  worktree. To retry a failed launch, use `dux agent retry --delegation <id>`
  (or the UI's `retry-delegation` action). Retry reuses the same delegation id,
  resets it to `queued`, and picks a new request key so a stale retry cannot
  match the old attempt. It never removes a worktree that contains commits,
  tracked changes, or untracked files; it only cleans up a worktree that failed
  before any user data existed. This prevents a timed-out client from
  duplicating work.

### 6.5 Acceptance semantics

- `delegate` validates the request shape and policy limits synchronously, then
  writes the delegation row with state `queued`. Target canonicalization, base
  resolution, and worktree creation happen in the prepare worker; a bad target
  or missing base surfaces as `failed` with a specific error, visible through
  `status` and `wait`.
- It **returns after queue acceptance by default**. The response includes the
  delegation id, target project, requested name, brief path, and state. The
  branch and worktree appear once preparation completes and can be read with
  `status`/`wait --until started`. This keeps fan-out fast, avoids a 100–120 s
  blocking tool call, and means a slow startup command cannot time out the
  parent's shell command.
- `--wait-for-launch` blocks until the child PTY is running or has failed. It is
  intended for single spawns and for the legacy `split-out` command, which keeps
  `wait-for-launch` as its default so existing skills see the old response shape.
  A configured startup command runs before launch, so this flag can be slow.
- `dux agent wait --until started` is available when a parent wants to queue
  several children quickly and then wait for launch.
- A provider launch failure removes the newly created worktree (as today) and
  marks the delegation `failed`.
- A startup-command failure keeps the agent, marks the delegation `running`, and
  reports `launched_with_startup_warning`, as today.
- If the caller times out while `--wait-for-launch` is still running, the
  delegation continues. Retrying with the same request key returns the existing
  delegation rather than creating a duplicate.

### 6.6 Fan-out queue

Replace the global `split_out_in_flight` / `create_agent_in_flight` booleans with
a queue shared by all agent-creation paths:

```text
CreateQueue
  queued: VecDeque<CreateRequest>
  active: usize
  max_active: usize                 # [orchestration] max_concurrent_launches
  repo_locks: HashMap<PathBuf, Lock>
```

- Worktree preparation is serialized per repository to avoid `git worktree add`
  races, but different repositories may prepare concurrently.
- PTY launches run concurrently after creation; they do not hold a slot forever.
- User-created agents and delegated agents share the same guards so dux never
  performs conflicting operations on one worktree.
- The status line summarizes the queue: "Creating delegated agents (2 running,
  3 queued)."
- Control responders stay attached to each request, so `--wait-for-launch` still
  receives the final launch result while asynchronous requests are answered as
  soon as they are queued.
- `MAX_CONTROL_CONNECTIONS` rises from 8 to 32. The default active-descendant
  limit is 8 and the total-descendant limit is 24, so a full fan-out fits without
  exhausting the socket.

### 6.7 Policy checks

Before queueing:

- source session exists (or a user invocation supplies `--source`);
- source and target are not Scratch as a target;
- same-repo and cross-repo delegation are allowed by config;
- `depth = source_delegation.depth + 1` (or 1 for a non-delegated source) is
  within `max_depth`; a missing or cyclic parent chain is treated as depth 0
  with a logged warning;
- active children of the source are within `max_children_per_agent`;
- active descendants of the mission are within
  `max_active_descendants_per_mission`;
- total descendants of the mission are within `max_total_descendants_per_mission`;
- provider is configured;
- brief size is within limits;
- request key is unique, or resolves to an existing delegation;
- target repository is inside `allowed_repo_roots` when that list is non-empty.

Reject with a specific `code` and an actionable message. The existing style of
explicit failure is preserved.

---

## 7. Completion, results, and turn-taking

### 7.1 The completion contract

Every generated brief ends with a mandatory completion section. The typed
initial prompt is short and points at the brief:

```text
You are a delegated dux agent. Read the task brief at
/home/you/.config/dux/delegations/<id>/brief.md and complete it. When you are
done, run "$DUX_BIN" agent finish --status success --summary-file - and pass
your summary on stdin. If you need a decision, use --status blocked.
```

The generated brief contains the full instructions, including a here-doc example
for the summary. This keeps the typed prompt one line and avoids newline
flattening problems.

### 7.2 `dux agent finish`

```bash
dux agent finish \
  --status success|failed|blocked \
  --summary "Short summary" \
  --summary-file - \
  --blocker "Need the API contract for field X" \
  --suggest-followup "Add tests for the new endpoint" \
  --json
```

The path in `DUX_RESULT_FILE` is stable across iterations (`result.json`). The
environment cannot change when the parent sends a follow-up, and a child should
never have to know which iteration it is on. The TUI archives the accepted file
as `result-<iteration>.json` after it records the result.

Behavior:

1. Resolve the child session from `DUX_SESSION_ID` (or `--session` for a manual
   user invocation). If the caller is itself an agent and the target is not its
   own session, reject with `not_owner`; a parent cannot report a child's
   result. A human invocation with no agent environment may target any active
   delegation for repair.
2. Build a result object with a client-generated `result_id`.
3. Write `result.json` atomically to the server-owned `DUX_RESULT_FILE`.
4. Send `DelegationFinish` over the control socket, including the result file
   path and `result_id`. The server verifies that the path is inside the
   directory it allocated for this delegation.
5. The TUI updates the delegation state and summary and notifies the UI. File
   hashing, archiving, and import are queued to a worker; the UI thread does not
   read or copy the result payload. Diff and commit facts are also computed by a
   worker when `result` is requested.

If the socket is unavailable, the file remains on disk and the command exits 0
with a warning unless `--strict` is set. On the next TUI start, and periodically
afterward, dux imports `result.json` for delegations whose stored `result_id`
does not match. Import and callback are idempotent: the same `result_id` is a
no-op success, and a newer result for the same iteration supersedes the old one.
A result file never changes a delegation that is already `cancelled`; it is kept
as an artifact for inspection.

The result summary stored in SQLite is capped at `result_summary_max_bytes`
(default 8 KiB). `status` and `wait` return only a `summary_preview` capped at
`result_summary_preview_bytes` (default 512 bytes) plus a `result_available`
flag. `dux agent result` returns the summary, git facts, PRs, and the result
file path; `--full` reads the file directly from disk in the CLI rather than
sending the payload over the socket. Without this split, a `wait --all` over the
maximum 24 descendants, or a `result` near `result_max_bytes`, could exceed the
256 KiB control response limit.

Result shape:

```json
{
  "result_id": "0c8f1f8e-...",
  "status": "success",
  "summary": "Added gift-card ledger entries and tests.",
  "details": "Optional longer report.",
  "tests": [{"command": "cargo test ledger", "status": "passed"}],
  "blockers": [],
  "suggested_followups": ["Wire the storefront button once the API ships."],
  "artifacts": ["target/report.json"],
  "reported_at": "2026-11-19T17:04:00Z"
}
```

Dux augments the stored result with git-derived facts:

- commits between `base_ref` and the child HEAD;
- the working-tree changes the child has not committed yet;
- changed-file list and diff stat;
- the child's PRs from the existing PR tracking;
- worktree path and branch.

Those facts are computed in a background worker when `result` is requested and
cached in memory for the delegation. `status` never shells out to git and never
returns the full result payload, so polling during `wait` stays cheap and does
not block the UI thread. `result` caps the changed-file list at 200 entries and
sets a `truncated` flag; the full diff remains available in the TUI pane. That
keeps a result response bounded even for a very large change.

The full result file is capped at `result_max_bytes` (default 256 KiB). SQLite
stores status, summary, path, iteration, and `result_id`; the file stores the
full payload. When `result.json` is absent (for example after an interrupted
follow-up), `result` falls back to the newest archived `result-<iteration>.json`.

### 7.3 `dux agent status` and `dux agent tree`

```bash
dux agent status --session <id> --json
dux agent status --delegation <id> --json
dux agent tree --session <id>
```

`status` is a pure SQLite/cache read. It returns the delegation rows for the
source's subtree: id, parent, child session (if any), title, state, reason,
project, repo path, branch, worktree, base mode and label, depth, iteration,
provider, timestamps, result status, summary preview, result availability, and
PR summaries. It never runs git, never opens a result payload, and never returns
the full brief.

`tree` is the human-readable rendering of the same data, with indentation and
state glyphs. The JSON shape is stable and is what orchestrator prompts should
parse.

### 7.4 `dux agent wait`

```bash
dux agent wait --all --timeout 100 --json
dux agent wait --session <child-id> --timeout 100
dux agent wait --delegation <delegation-id> --timeout 100
dux agent wait --all --until started
```

**The default timeout is intentionally short.** Agent CLIs commonly kill a tool
call after 60–120 seconds. A `wait --timeout 3600` would be killed by the parent's
own agent runtime long before it returned, leaving the parent confused about
whether the children are still running. Instead:

- `[orchestration] wait_default_timeout_secs = 100` keeps a wait under common
  tool limits.
- A timeout exits `2` and prints the current progress/status. It is not a
  failure: the orchestrator simply runs `wait` again. The orchestrator skill
  should treat `2` as "still working; poll again", not as an error.
- If the parent's provider kills the wait command anyway, the children continue
  unaffected. The next `wait` reads persisted state and picks up where it left
  off.
- `--timeout 0` means wait indefinitely, for humans running the command in a
  terminal.
- The child set is snapshotted when `wait` starts. New delegations created while
  it waits are not included; the next wait call picks them up. This prevents a
  deep tree from making one wait hang forever on newly spawned work.

Semantics:

- `--session` or `--delegation` waits for one delegation; `--all` waits for all
  current descendants of the source session; `--any` returns when the first one
  reaches a terminal state.
- Terminal states for waiting: `completed`, `failed`, `blocked`, `cancelled`,
  `exited`, `interrupted`.
- `--until started` is for fan-out: it returns when every requested delegation is
  `running` or terminal.
- If no matching delegations exist, return `no_delegations` unless
  `--allow-empty` is set, instead of returning success for an accidental typo.
- Exit codes: `0` all requested delegations completed successfully; `1` at least
  one failed, was cancelled, exited, or was interrupted; `2` timeout; `3` dux
  unavailable; `4` at least one is `blocked` and no harder failure occurred.
- The CLI prints a compact progress line to stderr unless `--quiet`, and the
  final JSON/table to stdout.
- Polling backoff: 250 ms, 500 ms, 1 s, 2 s, then cap at 2 s, with ±20% jitter so
  several orchestrators polling at once do not synchronize.

`blocked` is a first-class outcome. The child asks a question by finishing with
`--status blocked --summary "Question..."`. The parent's wait returns; the
parent reads the result, then calls `followup` with the answer. A blocked child
is not a failure and is not retried automatically.

### 7.5 Follow-up iterations

```bash
dux agent followup --session <child-id> --text-file - --json
```

- Targets a session or a delegation id. The target must be a descendant of the
  caller's session; a sibling cannot drive another branch. A human invocation
  with no agent environment may target any delegation.
- Allowed when the child is `completed`, `failed`, or `blocked`. `cancelled` and
  `exited` require the user to reconnect or create a new delegation.
- Verifies the child PTY is alive. If it is not, returns `not_running` with
  instructions; dux does not silently relaunch a provider mid-task.
- Appends the text to the child's `inbox.md` and queues the payload durably.
  Delivery is not immediate: dux waits until the child PTY has produced output
  and then has been quiet for `followup_quiet_ms` (default 500 ms). This avoids
  typing into the middle of the child's final response or a still-running tool
  call. It is the one place where a short idle heuristic is useful; it affects
  delivery timing, not task completion.
- Newlines are flattened the same way the initial handoff prompt is, so a raw
  newline cannot submit half an instruction. If the provider supports bracketed
  paste, dux may use it to preserve formatting; the fallback is a single line.
- The queue is FIFO per child and processes one follow-up at a time. Additional
  follow-ups append to `inbox.md` and wait for the current one to be delivered;
  they are not typed concurrently.
- Returns as soon as the follow-up is queued, not after delivery, so the parent's
  tool call does not block. `--wait-for-delivery` is available when the caller
  needs confirmation. The delegation moves to `resuming` at queue time, so a
  subsequent `wait` cannot observe the stale terminal result.
- When delivered, sets the delegation to `running`, increments `iteration`,
  clears the previous terminal result summary, and leaves the old archived
  result intact. The child must call `finish` again.
- If delivery fails or times out after `followup_delivery_timeout_secs` (default
  15), the delegation returns to its previous terminal state with an error and
  the queued text remains in `inbox.md` for inspection. The iteration number is
  only advanced on successful delivery.

This gives orchestrators an approval/revision loop without a live chat protocol:

```text
spawn → wait(completed) → read result/diff → followup("fix the edge case")
      → wait(completed) → integrate
```

### 7.6 Mid-run messages

`dux agent message --session <id> | --delegation <id> --text-file -` is a
best-effort, non-terminal message for a running child. It uses the same durable
queue and quiet-period delivery as `followup`, appends to `inbox.md`, and does
not change delegation state. It returns after queuing unless
`--wait-for-delivery` is set. The ancestry check is the same as `followup`.

This exists for corrections, but the reliable coordination path is
finish/followup. Dux does not pretend it can know when an arbitrary TUI is idle,
so it does not auto-inject messages and does not make correctness depend on them.

### 7.7 Parent notifications

When a child reaches a terminal state, dux:

- updates the tree badge and status line;
- appends a line to the parent's `inbox.md`;
- optionally writes a one-line message into the parent PTY only if the user
  enables `[orchestration] notify_parent_in_pty = true` (default false).

The primary notification mechanism is `dux agent wait`. The UI badge and inbox
cover the case where the orchestrator is not currently waiting.

---

## 8. Cancellation and subtree lifecycle

`dux agent cancel --session <id> | --delegation <id> [--subtree]`:

- Accepting a delegation id is required for queued or preparing children,
  because they do not have a session id yet.
- The target must be a descendant of the caller's session, or the invocation is
  a human one with no agent environment.
- `--subtree` cancels all non-terminal descendants depth-first.
- Running PTYs receive a graceful interrupt, then are terminated after
  `cancel_grace_secs`. This needs a new public PTY termination method; today
  `PtyClient` only kills the child on drop.
- Delegations become `cancelled` with a reason. Cancelling an already terminal
  delegation returns `already_terminal` and does not change it.
- A `finish` callback that races with cancellation does not resurrect the
  delegation: the result is archived for inspection, but the state stays
  `cancelled`.
- Worktrees and branches are **kept**. Removing them is a separate, explicit
  user action through the existing delete/cleanup flow.
- Queued delegations are removed from the queue and marked cancelled. Queued
  follow-up messages for the cancelled child are dropped from the delivery queue
  but remain in `inbox.md`.
- The parent gets a response listing what was cancelled.

`cleanup-worktrees` must treat any worktree whose delegation is non-terminal as
active and never offer it for cleanup. Project deletion must warn when the
project has active delegations; dux does not remove their worktrees.

Deleting an orchestrator session:

- If it has active descendants, the UI warns and offers "Cancel subtree first" or
  "Delete only; children become roots of the mission."
- Deleting only reparents nothing automatically in SQLite; children keep their
  `parent_session_id`, and the UI treats a missing parent as a mission-root
  child with an `orphaned` badge. Their worktrees remain intact.
- Archiving preserves delegation rows so results remain inspectable.

`purge_empty_groups` is extended to consider `agent_delegations`. Delegation
history is kept while any session in the mission is active or archived, then
purged with the group.

---

## 9. Integration of same-repo work

Cross-repo integration is not a history merge. The child commits, pushes, and
opens a PR; the parent reports the PR link. Dux tracks those PRs with the
existing `gh` integration.

Same-repo integration is explicit:

```bash
dux agent integrate --session <child-id> --into parent --strategy merge --json
dux agent integrate --session <child-id> --into parent --strategy patch --json
```

Preconditions:

- child delegation is `completed` (`--allow-incomplete` is a deliberate escape
  hatch);
- child branch exists and the child worktree is clean for `merge`/`cherry-pick`
  (all work committed). `patch` may include uncommitted child changes with
  `--include-uncommitted`, but the default is to refuse rather than silently
  omit work;
- target is an ancestor of the child in the same mission and belongs to the same
  repository; merging into a sibling or unrelated agent is not allowed;
- target worktree exists;
- target worktree has no tracked or untracked changes unless `--allow-dirty`;
  the default is to refuse rather than risk mixing unrelated work;
- target agent is not running unless the user explicitly confirms that it is
  safe to modify the files underneath it;
- agent-initiated integration is disabled by default
  (`[orchestration] allow_agent_integration = false`). The parent cannot merge
  its own children without the user opting in. The server distinguishes this by
  the presence of `DUX_SESSION_ID`/`source_session_id`; a human running
  `dux agent integrate` from a terminal has no source session and is treated as
  a user action.

Strategies:

- `merge`: `git merge --no-ff <child-branch>` in the target worktree. Used for
  `parent-head`, `leading`, `ref`, and `sibling-head` children.
- `cherry-pick`: apply the child's commits one by one.
- `patch`: compute `git diff --binary <base_ref> <child-branch>` and apply it
  with `git apply --binary --3way`. Used for `parent-snapshot` children and as
  a portable fallback. This is also the only sensible way to move changes across
  repositories by hand. Omitting `--binary` silently loses binary changes.
- Strategy selection is validated against the base mode. `parent-snapshot`
  children default to `patch`, and `merge`/`cherry-pick` are rejected for them
  unless the user passes an explicit override, because the snapshot baseline
  contains the parent's uncommitted work and merging it back would duplicate
  that work.

On conflict, dux aborts the operation, leaves the target clean, and returns the
conflicting paths. `--keep-conflicts` is intentionally not part of v1: leaving a
running agent's worktree in a conflicted state is a foot-gun.

After success, record `integrated_at` and `integration_commit` on the
delegation and log the strategy. The child worktree is not deleted. If
the same child is integrated twice, return `already_integrated` unless
`--force`.

### Overlap warnings

Before integration, dux compares the child's changed-file set with:

- files already changed in the target worktree;
- files changed by siblings already integrated into the same target.

If they overlap, the response includes a warning. Dux does not block the merge,
because overlapping files are often intentional; it just makes the risk visible
before the user commits.

---

## 10. UI

### 10.1 Sidebar tree

Replace the flat child list in **Multi-repo work** with a depth-first tree:

```text
⇄ Gift-card launch
  ● checkout-orchestrator            ORCH
    ├ ● gift-card-ledger     billing-service   ✓ completed
    │ └ ◐ ledger-tests       billing-service   ● running
    └ ◐ storefront-button    storefront        ! blocked
```

- The origin remains the selectable mission heading.
- Children are indented by depth with existing connector styling.
- Cross-repo rows show the project name.
- State icons come from semantic theme colors, not raw colors.
- Search still matches title, branch, project, provider, state, and paths.
- A collapsed mission hides descendants but keeps the aggregate state. Aggregate
  precedence is: failed/interrupted/exited (needs attention), then blocked, then
  running/queued, then completed. The origin itself is always shown.
- `next-related-agent` becomes a depth-first traversal of the whole tree.
- `focus-parent-agent` jumps to `parent_session_id`.
- A missing parent degrades to a mission-root child with an `orphaned` badge.
  Tree building must also guard against duplicate or cyclic `parent_session_id`
  values from a corrupt database and cap the visual indentation.

### 10.2 Right pane: Delegations section

Add a focusable **Delegations** section above Changes, following the pattern
already used by **Pull Requests**. It appears when the selected session has
children, or when it is itself a delegated worker.

For an orchestrator:

```text
Delegations (3)
  ✓ gift-card-ledger    billing-service   completed   2m ago
  ● ledger-tests        billing-service   running     40s ago
  ! storefront-button   storefront        blocked     "Need API shape"
```

For a worker:

```text
Delegated by checkout-orchestrator
  base: parent: gift-cards @ 91be002
  result: completed · "Added ledger entries and tests."
  result file: ~/.config/dux/delegations/<id>/result.json
```

Queued and preparing delegations appear here too, before they have a session id.
They can be cancelled by delegation id; they do not get a sidebar row until the
child session exists.

Actions (palette or row keys):

- open child agent;
- open result;
- copy branch/worktree/result path;
- cancel delegation or subtree;
- integrate (same repo only);
- follow up (opens the text editor and sends an iteration);
- retry/recreate after a failed launch;
- mark complete manually (user override for `exited`/`interrupted`).

### 10.3 Command palette

New actions, all rebindable through the existing `[keys]` mechanism:

- `delegate-agent` — modal to choose target project/path, title, brief, base
  mode, provider, and create the child.
- `cancel-delegation`
- `integrate-delegation`
- `followup-delegation`
- `open-delegation-result`
- `focus-parent-agent`
- `focus-next-sibling`
- `toggle-delegation-subtree`
- `copy-delegation-brief`
- `edit-mission` — edit `agent_groups.mission` for the selected mission

The interactive delegate modal uses `PromptState` so `Esc` works uniformly, uses
the existing multiline editor for the brief, and includes the same
misclick-safe spacing rules as other dialogs.

### 10.4 Center pane and status line

- The agent pane title shows a compact role badge: `ORCH 3` for an orchestrator
  with three children, `DELEGATED` for a worker.
- The status line reports the full lifecycle:
  - "Creating delegated agent "ledger-tests" in billing-service from parent HEAD..."
  - "Delegated agent "gift-card-ledger" completed. Press Enter on its row to read
    the result."
  - "Cancelled 3 delegated agents. Their worktrees were kept."
- Child completion sets an info badge, not an error, unless the result is
  `failed` or `blocked`.

### 10.5 Theme

Prefer existing semantic tokens:

- running → `session_active`
- completed → existing success/PR-open color
- failed → existing error color
- blocked → existing warning color
- interrupted/exited → existing detached/exited color
- queued/preparing/starting/resuming → existing dim/hint color

Only add a new semantic field if no existing token fits (for example a distinct
`orchestrator_badge`). If a field is added, wire it through every supported
theme mapping in the same change.

### 10.6 Minimum UI for v1

The backend and CLI can ship before the full UI, but an orchestrator that is
only visible through `dux agent status` would be a poor day-to-day experience.
The minimum implementation for v1 is:

1. **Recursive sidebar tree.** Extend `LeftItem::Session` with a depth/role
   field or add a dedicated delegation row type, make `build_left_items`
   recursive, and render state glyphs, parent/child connectors, and project
   names for cross-repo children. This is the non-negotiable change.
2. **Delegations section in the right pane.** Add a `RightSection::Delegations`
   above the existing PR/Changes layout in `render_files`, with state, project,
   branch, age, and summary preview. Queued delegations appear here before they
   have a session row.
3. **Control actions.** Cancel, follow up, integrate, open result, and retry from
   the Delegations section or palette. Without these, a human cannot recover
   from a blocked or failed child without dropping to the CLI.
4. **Status-line lifecycle messages.** Reuse the existing status API for
   creation, completion, blocked, cancellation, and integration.
5. **Navigation.** Make `next-related-agent` depth-first and add a
   `focus-parent-agent` action.

The interactive `delegate-agent` modal, editable mission text, collapse/expand
state, role badges, result viewer, and search-by-delegation-state can follow in a
second UI pass. The diff pane, PR pane, macros, companion terminals, and existing
theme structure need no new UI beyond consuming `base_ref`.

The UI must remain a pure reader for delegation status. `dux agent wait` polling
and the right-pane refresh read cached SQLite state; they do not run git on the
UI thread.

---

## 11. Configuration

All policy lives in config, fully commented by the canonical config renderer.
Runtime state stays in SQLite.

```toml
[agent_control]
# Allow agents running inside dux to control dux through the local socket.
enabled = true

# Allow --repo <path> to register an unregistered checkout as a project.
allow_unregistered_repos = true

# Optional allowlist of repository roots. Empty means any path allowed by the
# other rules. Supports ~ and environment expansion. Roots and candidate paths
# are canonicalized before comparison.
allowed_repo_roots = []

[orchestration]
# Allow agents to delegate work to child agents.
enabled = true

# Allow a delegated child to target its own repository.
allow_same_repo = true

# Allow a child to create further children.
allow_nested = true

# Maximum depth below a mission root. Direct children are depth 1.
max_depth = 3

# Maximum non-terminal direct children of one agent.
max_children_per_agent = 6

# Maximum non-terminal descendants in one mission.
max_active_descendants_per_mission = 8

# Maximum non-archived descendants in one mission. Archived history does not
# consume this budget, so old completed trees do not permanently block new work.
max_total_descendants_per_mission = 24

# Simultaneous worktree preparations. PTY launches continue concurrently.
# Values below 1 are treated as 1.
max_concurrent_launches = 2

# Default base mode for same-repo delegations.
# leading | parent-head | parent-snapshot
same_repo_default_base = "parent-head"

# Default base mode for cross-repo delegations.
# leading | ref
cross_repo_default_base = "leading"

# Allow explicit parent-snapshot delegations. Snapshotting copies the parent's
# tracked and untracked, non-ignored files into a synthetic baseline commit.
allow_parent_snapshot = true

# Allow a parent agent to merge its children into its own worktree.
allow_agent_integration = false

# Allow dux to type child-completion notices into a running parent PTY.
notify_parent_in_pty = false

# Default `dux agent wait` timeout. Keep this below the tool-call timeout of the
# agents you use; a timeout exits 2 and the caller polls again.
wait_default_timeout_secs = 100

# Maximum delay between wait status polls.
wait_poll_max_interval_ms = 2000

# How long a follow-up must see no child output before dux types it into the PTY.
followup_quiet_ms = 500

# How long dux tries to deliver a queued follow-up before returning it to the
# previous terminal state with an error.
followup_delivery_timeout_secs = 15

# How long to wait for a cancelled child to exit before terminating it.
cancel_grace_secs = 5

# Maximum brief size accepted by the CLI and TUI.
brief_max_bytes = 8388608

# Maximum result payload stored on disk. `status` and `wait` only carry the
# summary, never this full payload.
result_max_bytes = 262144

# Maximum result summary copied into SQLite.
result_summary_max_bytes = 8192

# Maximum summary preview returned by status/wait. Full text comes from result.
result_summary_preview_bytes = 512
```

The `[orchestration]` table and every field are added to the `ConfigEntry`
registry, default config template, `dux config diff`, and config tests in the
same change.

---

## 12. Safety and governance

- **Policy first.** Every spawn, follow-up, cancel, and integration re-checks
  policy in the TUI. The CLI is not trusted to enforce limits.
- **Bounded fan-out.** Depth, direct children, active descendants, total
  descendants, and launch concurrency are all configurable and enforced.
- **No automatic deletion.** Cancelling keeps worktrees. Deleting a worktree
  still requires the existing explicit user confirmation.
- **No automatic merge.** Integration is opt-in and disabled for
  agent-initiated calls by default.
- **No automatic cross-repo mutation.** A child in repo B never writes repo A.
  The parent only reads its result and diff.
- **Single writer.** Only the TUI mutates git worktrees, sessions, and SQLite.
  Agents and the CLI request work over the socket.
- **Path validation.** Repo paths are canonicalized, rejected if they point
  inside dux's data directory, and optionally constrained by
  `allowed_repo_roots`. Allowlist checks compare path components after
  canonicalization, so a prefix or symlink cannot escape a root.
- **Brief/result privacy.** Delegation files are `0600` in a `0700` directory.
- **Bounded payloads.** Request, brief, result, and response sizes are capped,
  and oversized payloads fail explicitly rather than being truncated.
- **Explicit failure.** Launch failure, missing worktree, missing base, invalid
  policy, and integration conflict all return actionable errors.

---

## 13. Failure and recovery

| Failure | Behavior |
| --- | --- |
| Base ref missing | Reject before creating a worktree; return `invalid_base`. |
| Branch already exists | Deduplicate on request key, otherwise return `branch_exists`. |
| Worktree creation fails | Delegation `failed`; no worktree left behind. |
| Provider launch fails | Remove the worktree and mark `failed`, as split-out does today. |
| Startup command fails | Keep the agent; mark `running` with a startup warning. |
| Child PTY exits without `finish` | Delegation `exited`; parent wait returns; outcome is unknown. |
| Child PTY exits after `finish` | Leave the terminal delegation state alone; a clean exit after reporting is normal for some providers. |
| Dux restarts while child runs | On startup, live sessions become `interrupted`; delegation waits return `interrupted` until reattached. Auto-reopen can return it to `running`. |
| Dux restarts after result file write but before callback | `result.json` is imported on startup and by the periodic scanner. |
| Socket unavailable during `finish` | File remains; command warns; import happens later. |
| Duplicate or late `finish` | Same `result_id` is an idempotent success; a newer result for the same iteration supersedes the old one; a result for a cancelled delegation is archived but does not change state. |
| `finish` arrives while `resuming` | Archive the result but keep `resuming`; the queued follow-up still gets delivered and starts the next iteration. |
| `finish` races `cancel` | Cancel wins. The result is retained as an artifact; the delegation stays `cancelled`. |
| Parent cancelled while waiting | CLI receives `not_running` or status showing cancellation; exits nonzero. |
| `wait` hits its 100 s timeout | Exit 2 with current progress; not a failure. The caller polls again. |
| Follow-up cannot be delivered | Restore the previous terminal state, keep the queued text in `inbox.md`, and return an actionable error. |
| Startup command hangs | The delegation stays `starting`; `wait --until started` times out without failing the child. The UI can open the startup log. |
| Integration conflict | Abort, leave target clean, return conflicting paths. |
| Child worktree deleted manually | Session becomes `exited`/missing; delegation `failed` with `worktree_missing` on next reconciliation. |

`SessionStatus` and delegation state are separate state machines and must be
reconciled in one place:

| Event | Session status | Delegation rule |
| --- | --- | --- |
| PTY exits while `running`/`starting` with no result | `Detached`/`Exited` | `exited` (reason includes exit code if known). |
| PTY exits while `resuming` | `Detached`/`Exited` | Restore the previous terminal state and report the follow-up delivery failure. |
| PTY exits after `completed`/`failed`/`blocked` | `Detached`/`Exited` | Leave terminal state unchanged. |
| User stops a running agent | `Detached` | `interrupted` unless already terminal. |
| User reconnects a session | `Active` | `interrupted` → `running`; terminal states unchanged. |
| User deletes an agent | row archived/removed | Delegation keeps its result; children are orphaned, not deleted. |
| TUI restart | restored as `Detached` | Non-terminal delegations become `interrupted` after auto-reopen settles. |

On startup, reconcile every non-terminal delegation:

- `queued`/`preparing`/`starting` from a previous process → `interrupted` with a
  reason that no PTY survived.
- `running` with a live restored session → stays `running` if auto-reopen
  succeeded, otherwise `interrupted`. Reconciliation waits for the bounded
  auto-reopen attempt to settle, so a parent wait does not observe a transient
  `interrupted` while dux is still restoring the child.
- `resuming` after a restart → restore the previous terminal state with
  `followup_interrupted`, keep the text in `inbox.md`, and let the parent retry.
  The in-memory delivery job does not survive a restart.
- `running` with a missing worktree → `failed`.
- `result.json` newer than the stored result and carrying a new `result_id` →
  import it, archive it, and update the state unless the delegation is
  `cancelled` or `resuming`. In those two states the result is retained as an
  artifact without changing the state machine.
- Result files for delegations that no longer exist are left in place for manual
  inspection and ignored by the scanner.

### 13.1 Known limitations

- **Completion depends on the child.** If a provider ignores the brief's
  completion contract, the delegation ends as `exited`, not `completed`. That is
  an honest unknown, not a silent success. A provider that cannot run shell
  commands (for example a pure chat CLI) cannot call `finish` and therefore
  cannot participate in turn-taking; it can still run as a normal dux agent.
- **Follow-ups need a live PTY.** If a provider exits after `finish`, follow-up
  returns `not_running`. The user can reconnect/resume the agent, then retry.
  Dux does not silently relaunch a provider in the middle of a task.
- **Follow-up delivery is best-effort.** The quiet-period gate prevents most
  interleaving, but an arbitrary TUI can still be mid-render or mid-tool-call.
  The inbox keeps the text if delivery fails.
- **Snapshot excludes ignored files.** Local `.env`, dependency caches, and
  generated artifacts are not copied. Startup commands should recreate what a
  checkout needs.
- **Integration is not atomic across children.** Merging several same-repo
  children is a sequence of normal git merges; later merges can conflict with
  earlier ones. Dux warns about overlapping files but cannot make the set
  conflict-free.
- **Cross-repo work cannot be merged by dux.** It lands through commits and PRs.
- **A parent agent's tool timeout is outside dux's control.** This is why `wait`
  defaults to a short timeout and must be repeated on exit code 2.

---

## 14. Backward compatibility and migration

- Existing `split-out` sessions already have `group_id` and
  `parent_session_id`, so tree rendering works immediately.
- Existing split-out keeps working: `split-out` becomes an alias of `delegate`
  with the same flag names, response fields, default cross-repo behavior, and
  `wait-for-launch` default.
- New `ControlResponse` fields are additive and skipped when empty. New
  `ControlCommand` variants are additive. Add `#[serde(default)]` to every new
  `SplitOutParams` field so an older CLI still parses against a new TUI. Keep
  `CONTROL_PROTOCOL = 1`; old clients talking to a new TUI keep working, and a
  new client talking to an old TUI receives `invalid_request` for unknown
  commands. `DUX_BIN` normally keeps both sides in lockstep.
- Legacy sessions have `base_ref = NULL`; `diff_base_ref_for_session` keeps the
  current project-leading behavior until a new delegation sets a base.
- Legacy split-out rows have no `agent_delegations` row. `dux agent status` and
  the tree synthesize a read-only delegation from the session on demand, with
  state derived from `SessionStatus` and an empty result. New work always gets a
  real row.
- `agent_groups.mission` is nullable.
- All schema changes use the existing `ensure_column` migration pattern and are
  covered by a migration test that opens the oldest supported schema.
- The UI section is renamed to **Delegated work**; `README.md`,
  `docs/user-guide.md`, and the generated config comments are updated in the same
  change. The search and help overlay copy are updated too.

---

## 15. Implementation plan

Each phase is shippable and testable on its own.

### Phase 1 — Foundation

- Add `agent_delegations`, `agent_groups.mission`, and session `base_ref` /
  `delegation_id`, including archive-table migration. Reuse `source_branch` for
  the human-readable base label.
- Add `DelegationState`, `BaseMode`, `Delegation`, and result types in
  `src/model.rs`.
- Add storage CRUD and reconciliation.
- Make `split-out` create a delegation row.
- Freeze the base SHA and make `diff_base_ref_for_session` and base-relative UI
  prefer it.
- Add `--base-mode parent-head` and same-repo targeting.
- Add `dux agent status`, `result`, and `finish`. Keep `status` a pure SQLite
  read; compute result git facts on a worker.
- Add durable `brief.md` / `result.json` files, idempotent result import, and
  `result_id`-based deduplication.
- Add `dux agent wait` with the 100 s default and chunked polling semantics.
- Add one reconciliation function that maps `SessionStatus` events to
  delegation state.

### Phase 2 — Fan-out and control

- Replace global in-flight booleans with the create queue and per-repo locks.
- Make `delegate` return after queue acceptance; keep `split-out` waiting for
  launch for compatibility.
- Add policy limits, idempotency keys, deduplicated retries, `dux agent retry`,
  and `--delegation` targets for queued children.
- Add `--until started`, `--any`, `--all`, `--allow-empty`, and exit code `4`
  for `blocked`.
- Add `followup`, `blocked` iterations, `message`, and `cancel --subtree`.
- Add the durable follow-up queue with quiet-period delivery.
- Add parent inbox notifications.
- Raise the control connection limit to 32.

### Phase 3 — UI

**3a, minimum shippable UI**

- Render the recursive sidebar tree, depth indentation, state glyphs, and
  cross-repo project names.
- Add the Delegations right-pane section, including queued rows and summary
  previews.
- Add cancel, follow-up, integrate, open-result, and retry actions.
- Add tree-aware `next-related-agent` and `focus-parent-agent`.
- Add status-line lifecycle messages and help-overlay entries.

**3b, polish**

- Add role badges, aggregate mission state, collapse/expand, mission editing,
  search by delegation state, and the result viewer.
- Add the interactive `delegate-agent` modal.
- Add semantic theme tokens only where existing tokens do not fit.

### Phase 4 — Integration

- Add same-repo `integrate` with merge, cherry-pick, and patch strategies.
- Add clean-worktree checks, running-parent confirmation, conflict abort, and
  overlap warnings.
- Record integration metadata.
- Add cross-repo result summaries that surface PRs through the existing PR pane.

### Phase 5 — Product polish

- Add `skills/dux-orchestrate/SKILL.md` and update
  `skills/dux-split-out/SKILL.md` for the completion contract.
- Update `README.md`, `docs/user-guide.md`, and config documentation.
- Extend `demo/multi-repo` with an orchestrator tree that fans out to two
  repositories and one same-repo child.
- Refresh the README screenshot.
- Add recovery, migration, and concurrency tests.

### Likely file touchpoints

| Area | Files |
| --- | --- |
| Model/types | `src/model.rs` |
| Storage/migration | `src/storage.rs` |
| IPC | `src/ipc.rs` |
| CLI | `src/cli.rs` |
| Control handling/queue | `src/app/control.rs` |
| Workers/jobs | `src/app/workers.rs` |
| App state/tree building | `src/app/mod.rs` |
| Session/agent CRUD | `src/app/sessions.rs` |
| Input/palette/modals | `src/app/input.rs` |
| Rendering/theme | `src/app/render.rs`, `src/theme.rs`, `assets/themes/` |
| Config | `src/config.rs` |
| Docs/skills/demo | `README.md`, `docs/`, `skills/`, `demo/multi-repo/` |

---

## 16. Testing strategy

### Unit tests

- Base-mode resolution: leading, ref, parent-head, parent-snapshot, sibling-head,
  cross-repo rejection of parent modes.
- Snapshot baseline: synthetic commit contains tracked and untracked non-ignored
  files, excludes ignored files, and leaves the parent worktree untouched.
- Frozen diff base: child right-pane diff excludes parent changes.
- Policy: depth, direct-children, active-descendant, total-descendant, same-repo,
  cross-repo, repo allowlist.
- Idempotency: same request key returns the same delegation; branch collision
  without a key fails.
- State machine: every allowed transition, every rejected transition, follow-up
  iteration increments, cancellation is final, and `finish` after `cancel` does
  not resurrect the delegation.
- Session/delegation reconciliation: PTY exits before and after `finish`, user
  stop, reconnect, TUI restart, and missing worktree.
- Result parsing, size caps, atomic writes, `result_id` deduplication, duplicate
  callback, late callback, and import after a missed callback.
- Stable `result.json` path across iterations and archive naming.
- `wait` terminal-state selection, blocked exit code, empty-set handling,
  snapshot-of-descendants behavior, and timeout exit code (mock status source).
- Follow-up delivery: quiet-period gate, queued response, delivery timeout
  restoration, and inbox retention.
- Startup command that outlives the CLI request does not change the delegation
  response or duplicate it on retry.
- Snapshot identity: neutral git identity, empty-tree falls back to HEAD, and
  parent index/HEAD are untouched.
- Sidebar tree building: nested ordering, orphans, queued delegations in the
  task pane, collapsed subtrees, depth-first navigation.
- Migration: oldest schema gains columns, legacy sessions keep their diff base,
  and archived split-out sessions still render.
- Group/delegation purge with active and archived sessions.
- Config parsing and rendering for every new setting.

### Integration tests

- Fake provider that reads `DUX_BRIEF_FILE`, calls `dux agent finish`, and can
  optionally delegate a grandchild.
- Two-repo end-to-end: orchestrator fans out with `--no-wait`, polls with a short
  `wait`, reads results, sends a blocked follow-up, and reports PRs.
- Fan-out queue: five simultaneous spawns across two repos; verify no worktree
  races, correct per-request responses, and queue limits.
- Cancel subtree: running and queued descendants become cancelled, including
  cancel-by-delegation-id before a session exists; worktrees remain.
- Restart recovery: kill the TUI while children run; restart; verify
  `interrupted`, result import, auto-reopen reconciliation, and reattach.
- Integration: successful merge, conflict abort, snapshot patch with binary
  files, dirty/running target refusal, and double integration rejection.
- Follow-up delivery to a child that is still printing its final response.
- Control protocol compatibility: old split-out request shape still parses with
  defaulted new fields.

### Manual smoke test

Extend the reproducible multi-repo demo so `make screenshots` shows an
orchestrator, a same-repo child, and a cross-repo child, with one completed and
one blocked. Ask the user to run `cargo run` for an interactive smoke test rather
than running the TUI automatically.

---

## 17. Alternatives considered

| Alternative | Why rejected |
| --- | --- |
| Provider-native subagents only | Not provider-neutral, no cross-repo worktree isolation, no shared dux state. |
| One shared worktree for all children | Parallel writes conflict, diffs mix together, no isolation. |
| Children write directly to SQLite/config | Breaks the single-writer model and races with the TUI. |
| Full agent-to-agent RPC/MCP | Violates the no-protocol-layer tenet and adds adapter maintenance. |
| Infer completion from terminal idle output | Unreliable across TUIs; false completes are worse than explicit waits. |
| `wait` as a server-side long-poll | Consumes control connections and complicates the event loop; CLI polling is enough. |
| Automatic merge on child completion | Dangerous for running agents and conflicted worktrees. |
| Automatic worktree cleanup after completion | Worktrees are user data; deletion needs explicit confirmation. |
| Encode the tree in branch names (`parent/child`) | Nested worktree directories complicate cleanup; SQLite already knows the tree. |

---

## 18. Open questions

1. **Section rename.** Is "Delegated work" the right replacement for "Multi-repo
   work", or should the section stay named for cross-repo work and show
   same-repo trees under their project instead? Recommendation: one section.
2. **Same-repo default.** `parent-head` is the safest useful default. Should dux
   instead require an explicit base mode for same-repo spawns until users opt in?
3. **Snapshot scope.** Are tracked plus untracked non-ignored files enough, or do
   users need configured ignored files (`.env`) copied too? If so, add
   `snapshot_include_ignored` with diff-noise handling.
4. **Mission brief ownership.** Should the orchestrator's first prompt be copied
   into `agent_groups.mission` automatically, or only when explicitly set?
5. **Manual mark-complete.** Should users be able to mark an `exited` child as
   completed without a child result, or should `exited` always require a new
   child?
6. **Agent-initiated integration.** Default off is safe. Is an explicit
   `--yes-i-know` flag enough for power users, or should all integration require
   the UI?
7. **Large result payloads.** Is 256 KiB enough, or should results be split into
   summary plus separate artifact files referenced by path?
8. **Integration target.** Should v1 merge into the parent worktree, or should
   dux create a temporary integration branch/worktree per mission so a dirty or
   running parent is never touched? The latter is safer but adds lifecycle and
   branch-cleanup complexity.
9. **Wait defaults per provider.** Provider tool-call limits differ. Should
   `wait_default_timeout_secs` be global, project-level, or provider-level, and
   should the generated skill tell each provider how to loop?

---

## Appendix A — Example orchestration

Mission: ship gift-card support across billing and storefront.

```bash
# Orchestrator in checkout-api, branch gift-cards.
# All of this runs inside the orchestrator's agent PTY.

# 1. Same-repo child from the orchestrator's committed state.
# `delegate` returns after queueing, so this is fast even with startup commands.
dux agent delegate \
  --same-repo \
  --title "Add gift-card ledger entries" \
  --name gift-card-ledger \
  --base-mode parent-head \
  --brief-file - --json <<'BRIEF'
Implement gift-card ledger entries in the billing package.

Context: the orchestrator is adding gift-card checkout in checkout-api.
Your scope is billing-service only. Add the ledger model, migration, and tests.
Do not touch checkout-api. Report the exact ledger API shape when done.
BRIEF

# 2. Cross-repo child in storefront from its leading branch.
dux agent delegate \
  --project storefront \
  --title "Add gift-card checkout button" \
  --name gift-card-button \
  --base-mode leading \
  --brief-file storefront-brief.md --json

# 3. Fan in. Keep the call under your provider's tool timeout. Exit code 2 means
# "still running"; run the same wait again. Dux is not blocked while it waits.
dux agent wait --all --timeout 100 --json

# 4. Read each result and inspect diffs in the UI. Use the delegation id until
# a child has a session id.
dux agent result --delegation <ledger-delegation-id> --json
dux agent result --delegation <button-delegation-id> --json

# 5. If storefront is blocked on the ledger API shape, answer it.
dux agent followup --session <button-id> --text-file - <<'ANSWER'
The ledger API is POST /ledger/entries with {gift_card_id, amount}.
Continue and report the PR.
ANSWER

# 6. Integrate the same-repo child after review (user or allowlisted agent).
dux agent integrate --session <ledger-id> --into parent --strategy merge --json

# 7. Cross-repo child opens a PR; dux tracks it and the parent reports the link.
```

## Appendix B — Generated brief preamble

````markdown
# Delegated task

You are a delegated worker in a dux mission. You have your own git worktree and
branch. Work only in your worktree.

- Mission: Gift-card launch
- Parent agent: checkout-orchestrator (branch: gift-cards)
- Your branch: gift-card-ledger
- Your worktree: /home/you/.config/dux/worktrees/billing-service/gift-card-ledger
- Base: parent HEAD @ 91be002
- Depth: 1 of 3

**Rules**

- Do not edit the parent worktree or another delegated worktree.
- Commit your work on your branch.
- If you need a decision, report `blocked` instead of guessing.
- If the task is impossible, report `failed` with the exact error.

**Task**

<user brief>

**Completion contract**

When the task is done, run:

```bash
"$DUX_BIN" agent finish --status success --summary-file - <<'DUX'
One or two paragraphs: what changed, how it was verified, and anything the
parent must know before integrating.
DUX
```

If you need input, use `--status blocked` and put the question in the summary.
If you failed, use `--status failed` and include the error and what you tried.
````

The typed initial prompt points at this file and repeats the finish command in a
single line:

```text
You are a delegated dux agent. Read the task brief at $DUX_BRIEF_FILE and complete it. When done, run "$DUX_BIN" agent finish --status success --summary-file - and pass your summary on stdin. If you need a decision, use --status blocked.
```
