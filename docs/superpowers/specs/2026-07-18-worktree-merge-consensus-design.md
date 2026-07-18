# Worktree Merge Consensus — Design Specification

Date: 2026-07-18  
Status: Approved in conversation; awaiting written-spec review

## 1. Summary

`worktree-merge-consensus` is an open-source coordinator for safely integrating
changes produced by two existing Codex tasks. The tasks retain their original
conversation histories and work in separate worktrees of the same Git
repository on the same host. One task is the primary implementer; the other is
the reviewer that protects its implementation intent.

The project uses ordinary Rust code—not a third coordinating agent—to drive a
structured negotiation through Codex App Server. The primary task proposes and
executes the integration. The reviewer task approves the plan and the exact
resulting commit. The run stops at a newly created, locally accepted integration
branch. It never pushes, opens a pull request, or merges into an existing branch.

## 2. Goals

- Preserve the independent context and implementation rationale of both tasks.
- Automate contract exchange, plan review, integration, verification, and final
  result review.
- Keep all Git writes under the primary task's control.
- Make every approval apply to exact source SHAs, plan revisions, and result
  SHAs.
- Recover safely after coordinator interruption or server restart.
- Work with standalone Codex installations that provide `codex app-server`.
- Be distributable as both a Linux CLI and a Codex plugin.
- Fail closed whenever identity, repository state, protocol state, or tool
  compatibility is uncertain.

## 3. Supported Environment

Version 1 supports this topology only:

- Two existing tasks managed by Codex App/App Server.
- Both tasks execute on the same Linux host and under the same Unix account.
- Each task has a distinct worktree.
- Both worktrees resolve to the same canonical Git common directory.
- Both implementations are committed before coordination starts.
- The primary task has permission to create a new local branch, edit files,
  commit conflict resolutions, and run repository tests.
- `codex-cli 0.144.5` is the first verified Codex version.
- Release artifacts target Linux x86_64 and Linux ARM64.

The coordinator runs on the same server as both tasks. It does not require a
local desktop relay, Node.js, Python, or a third model session.

## 4. Non-goals

Version 1 does not:

- Coordinate tasks located on different hosts.
- Transfer commits between repositories or hosts.
- Support arbitrary standalone interactive CLI sessions that are not visible
  through the local App Server.
- Create, fork, or delegate to an additional Codex task or subagent.
- Push branches, open pull requests, or update an existing branch.
- Delete branches, reset worktrees, rebase source commits, or clean user files.
- Replace project-specific tests or repository instructions.
- Promise compatibility with unverified App Server protocol versions.

## 5. Architecture

The repository has five runtime components.

### 5.1 Rust core

The core owns the deterministic state machine, protocol envelopes, JSON Schema
validation, verdict rules, progress detection, branch safety rules, and recovery
logic. It never invokes a language model itself; it asks App Server to start
turns in the two selected tasks.

### 5.2 App Server adapter

This layer isolates the experimental Codex protocol. It implements only the
methods required by the project, initially:

- `thread/list`
- `thread/read`
- `thread/resume`
- `turn/start`
- turn lifecycle notifications and approval/input requests

It performs an initialization and capability handshake before every run.
Unknown required fields, missing methods, or an unsupported Codex version cause
`INCOMPATIBLE_CODEX`; the coordinator does not guess at compatibility.

### 5.3 Consensus daemon

A single per-user daemon runs on the server and owns active consensus runs. It
continues after the initiating CLI or MCP request exits. It exposes a Unix
socket with mode `0600`, persists state transactionally, serializes operations
for a repository, and reconnects to App Server after transient failures.

The daemon connects to the managed local App Server through its control socket
and `codex app-server proxy`. `doctor` checks the daemon and App Server. When a
managed App Server is absent, the tool may start it with the installed Codex
CLI before accepting a run.

### 5.4 CLI

The CLI provides human terminal selection and non-interactive automation. It
communicates with the consensus daemon; it does not duplicate the state machine.

### 5.5 Codex plugin

The plugin contains a Skill and an MCP server entry point backed by the same
Rust binary. The MCP layer forwards list/start/status/resume/cancel operations
to the consensus daemon. When started from the primary task, it returns a
`run_id` immediately; the daemon waits for that initiating turn to finish before
starting the next primary turn. No third agent is introduced.

## 6. Roles and Authority

### Primary task

- Supplies its implementation contract from its own history and committed code.
- Produces and revises the complete integration plan.
- Is the only actor allowed to create the integration branch or modify Git
  state.
- Merges the reviewer SHA, resolves conflicts, runs tests, and commits fixes.
- Reports the exact integration branch and HEAD SHA after every result change.

### Reviewer task

- Supplies its implementation contract from its own history and committed code.
- Reviews whether the plan preserves every reviewer-owned behavior and decision.
- Inspects the exact shared Git object named by `integration_sha`.
- Returns concrete changes or approval for the exact current result.
- Does not modify the integration branch during review.

### Coordinator

- Selects and addresses tasks, starts turns, validates replies, persists state,
  and advances the deterministic state machine.
- Performs read-only Git inspection independently of task claims.
- Does not resolve conflicts, choose product behavior, modify files, or write Git
  refs.

## 7. Task Selection

### Interactive mode

Running `codex-consensus run` in a TTY performs these steps:

1. Call `thread/list` on the local App Server.
2. Display searchable rows containing task title, state, working directory,
   update time, and abbreviated thread ID.
3. Let the user select the primary task.
4. Canonicalize the primary task's Git common directory from its working
   directory.
5. Filter reviewer candidates to a different task and different worktree that
   resolves to the same Git common directory.
6. Let the user select the reviewer task.
7. Display both thread IDs, worktrees, frozen HEAD candidates, and the common
   repository before confirmation.

The selector is a terminal UI implemented by Rust. It consumes no model turn.
Users may run it through SSH with a TTY, for example `ssh -t host
codex-consensus run`.

### Non-interactive mode

Automation supplies both IDs explicitly:

```bash
codex-consensus run \
  --primary-thread 019f-example-primary \
  --reviewer-thread 019f-example-reviewer \
  --json
```

Non-interactive mode executes every identity and Git safety check used by the
interactive path. Flags cannot bypass preflight gates.

## 8. Git Preflight and Frozen State

For each selected task, the coordinator obtains its working directory from App
Server and runs read-only Git commands equivalent to:

```bash
git rev-parse --show-toplevel
git rev-parse --git-common-dir
git rev-parse HEAD
git status --porcelain
git symbolic-ref --quiet HEAD
```

Paths are canonicalized before comparison. Preflight requires:

- Different thread IDs.
- Different canonical worktree paths.
- The same canonical Git common directory.
- Clean source worktrees.
- Readable 40-character source commit SHAs.
- Stable attached source refs when a worktree is not detached.

The run records `primary_sha`, `reviewer_sha`, source-ref names when present,
and source-ref target SHAs. A detached source worktree is identified only by its
frozen commit. Any later source drift invalidates approvals and blocks the run.

## 9. State Machine

The coordinator advances through:

```text
DISCOVER
  -> FREEZE
  -> CONTRACT
  -> PLAN_REVIEW
  -> INTEGRATE
  -> VERIFY
  -> RESULT_REVIEW
  -> ACCEPTED | BLOCKED | PAUSED_USER_ACTION | CANCELLED
```

### DISCOVER and FREEZE

Resolve both tasks and establish the immutable repository and source-SHA facts.
No task turn that could cause integration is started until preflight succeeds.

### CONTRACT

Start one self-contained contract request in each task. Each contract includes
goals, user-observable behavior, design choices and rationale, invariants,
interfaces, edge cases, rejected alternatives, relevant files, tests, and its
frozen SHA.

### PLAN_REVIEW

Send both complete contracts to the primary task. Require a complete plan and a
coverage row for every contract item. Send that full plan to the reviewer. A
reviewer may return `CHANGES_REQUIRED` with evidence or `APPROVED_PLAN` for one
exact plan revision and pair of source SHAs.

The phase allows at most six review rounds. Two consecutive rounds with no
material change in either the plan or issue set produce `NO_PROGRESS`.

### INTEGRATE

Only after valid plan approval, tell the primary task to:

1. Revalidate its worktree and frozen inputs.
2. Create a unique new branch at `primary_sha`.
3. Merge `reviewer_sha` only into that branch.
4. Resolve conflicts according to the approved plan.
5. Commit all compatibility fixes on the integration branch.

The coordinator does not execute these Git writes.

### VERIFY

The primary task runs repository instructions, both contracts' tests, and any
user-supplied tests. It reports commands, exit statuses, and coverage evidence.
The coordinator independently verifies read-only Git facts: current branch,
HEAD, clean status, source-ref stability, merge-base ancestry, unresolved index
entries, and conflict markers where safely inspectable.

Required tests must pass before result review starts.

### RESULT_REVIEW

Send the reviewer the exact integration branch, `integration_sha`, coverage
matrix, conflict decisions, changed-file summary, and test evidence. Approval
applies only to that exact SHA. Any new integration commit invalidates the prior
verdict and starts another result-review round, up to six rounds with the same
no-progress rule.

### ACCEPTED

Accept only when the reviewer approves the current HEAD and the coordinator
revalidates all Git invariants. Stop without pushing or merging elsewhere.

## 10. Message Protocol

Every machine-significant response is exactly one JSON object validated against
the repository's versioned JSON Schema. Human explanation belongs inside schema
fields. Text outside the object is ignored for approval purposes.

Every envelope contains:

```text
protocol
run_id
message_type
phase
round
primary_sha
reviewer_sha
plan_revision
integration_branch
integration_sha
reason_code
payload
```

Initial protocol messages use `worktree-merge-consensus/v1`. Core verdicts are:

- `CONTRACT_READY`
- `CHANGES_REQUIRED`
- `APPROVED_PLAN`
- `INTEGRATION_READY`
- `APPROVED_RESULT`
- `BLOCKED`

Immutable envelope mismatches, unknown verdicts, stale rounds, stale plan
revisions, and stale result SHAs are invalid. Natural-language agreement is not
approval.

Each turn request is self-contained. It includes the full current envelope and
payload rather than relying on the other task to remember incremental deltas.
This preserves correctness when long task histories are compacted.

## 11. Persistence and Idempotency

State lives under `$XDG_STATE_HOME/codex-consensus/` when `XDG_STATE_HOME` is
set, otherwise under `$HOME/.local/state/codex-consensus/`:

```text
$XDG_STATE_HOME/codex-consensus/
$HOME/.local/state/codex-consensus/
```

A local SQLite database, built into the standalone binary, stores:

- Run and repository identifiers.
- Thread IDs and canonical worktree paths.
- Frozen source facts.
- Current phase, round, and plan revision.
- App Server thread/turn identifiers.
- Request and response hashes.
- Integration branch and SHA.
- Test summaries and terminal status.

Full prompts, chat history, source code, and diffs are not persisted by default.
The daemon parses them in memory and discards them after extracting validated
protocol data. On recovery, it uses saved turn IDs and `thread/read` to locate
the canonical messages. If canonical history cannot be recovered, the run fails
closed rather than reconstructing an uncertain payload.

An explicit detailed-audit option may retain complete protocol messages in
files readable only by the owning user. The interface warns that those files
may contain proprietary code context.

Before sending a turn, the daemon commits a pending-send record. After reconnect
or restart, it checks the thread history for the matching `run_id`, phase, round,
and message hash before deciding whether to send. This makes turn delivery
idempotent across crashes.

## 12. Runtime Status and Recovery

User-visible statuses are:

- `RUNNING`
- `WAITING_THREAD`
- `PAUSED_USER_ACTION`
- `ACCEPTED`
- `BLOCKED`
- `CANCELLED`
- `INCOMPATIBLE_CODEX`

The daemon never starts a new turn on a task with an active turn. It waits for a
completion notification or a bounded status poll. Permission approvals and
requests for user input transition to `PAUSED_USER_ACTION`; after the user acts,
`resume` revalidates all frozen facts before continuing.

Transient App Server disconnections use bounded retry with backoff. Repeated
failure pauses the run without changing Git. Cancellation prevents new turns
but does not forcibly interrupt an already running task turn, delete branches,
or revert code.

## 13. Blocked Reasons

Stable reasons include:

- `AMBIGUOUS_THREAD`
- `INCOMPATIBLE_CODEX`
- `DIFFERENT_REPOSITORY`
- `DIRTY_WORKTREE`
- `SOURCE_DRIFT`
- `INVALID_RESPONSE`
- `COMMUNICATION_FAILURE`
- `NO_PROGRESS`
- `ROUND_LIMIT`
- `TEST_FAILURE`
- `PERMISSION_REQUIRED`
- `HISTORY_UNAVAILABLE`

A blocked report includes observed evidence, preserved state, and the minimum
user action that could make a new or resumed run safe.

## 14. Security Model

- All services run as the same unprivileged Unix user as the two tasks.
- The consensus daemon listens only on a user-owned Unix socket with mode
  `0600`; v1 exposes no TCP listener.
- Repository paths are canonicalized and command arguments are passed without
  shell interpolation.
- Thread payloads are treated as untrusted data and cannot alter immutable
  envelope fields or coordinator policy.
- Only one active run may write through a given primary worktree or target Git
  common directory.
- No credentials, environment values, complete prompts, or source contents are
  emitted in normal logs.
- The daemon has no Git write path. Git mutation instructions are sent only to
  the selected primary task after plan approval.

## 15. User Interfaces

### CLI commands

```text
codex-consensus doctor
codex-consensus threads list
codex-consensus run
codex-consensus status [RUN_ID]
codex-consensus resume [RUN_ID]
codex-consensus cancel [RUN_ID]
```

Machine consumers use `--json`. The run command accepts
`--primary-thread`, `--reviewer-thread`, an optional unique
`--integration-branch`, extra test commands, and policy limits that may only be
stricter than project defaults.

### Plugin MCP tools

```text
consensus_doctor
consensus_list_threads
consensus_start
consensus_status
consensus_resume
consensus_cancel
```

The bundled Skill guides the initiating primary task to list candidates, obtain
the user's choice, call `consensus_start`, report the run ID, and end its launch
turn. It does not perform the review loop itself.

## 16. Successful Output

An accepted run reports at least:

```text
status: ACCEPTED
run_id
integration_branch
integration_sha
primary_sha
reviewer_sha
tests
source_refs_unchanged: true
```

The output states explicitly that the branch is local and was not pushed or
merged into an existing branch.

## 17. Repository Layout

```text
worktree-merge-consensus/
├── crates/
│   ├── core/
│   ├── app-server-client/
│   ├── daemon/
│   ├── cli/
│   └── mcp-server/
├── plugin/
│   ├── .codex-plugin/
│   ├── skills/
│   └── marketplace/
├── schemas/
├── tests/
│   ├── fixtures/
│   ├── fake-app-server/
│   └── e2e/
├── docs/
├── .github/workflows/
├── Cargo.toml
├── README.md
├── README.zh-CN.md
├── LICENSE
└── SECURITY.md
```

The repository name, Skill name, and protocol family are
`worktree-merge-consensus`. The executable is `codex-consensus` to keep command
invocations concise.

## 18. Testing Strategy

### Unit tests

- Every legal and illegal state transition.
- Envelope and payload schema validation.
- Exact-SHA and exact-plan approval rules.
- Six-round limits and two-round no-progress detection.
- Idempotent send recovery and message hashing.
- Version and capability negotiation.
- Path canonicalization and repository identity rules.

### Fake App Server integration tests

- Thread listing, reading, resuming, and turn start.
- Busy tasks and out-of-order notifications.
- Duplicate notifications and reconnects.
- Invalid JSON, stale verdicts, timeouts, and approval requests.
- Crash after pending-send and crash after received-response.

### Git end-to-end tests

Tests create temporary repositories with two real worktrees and simulated task
transports. Cases include:

- Conflict-free integration.
- Text conflicts resolved only on the new branch.
- Multiple plan revisions.
- Result rejection followed by a new integration SHA.
- Source-ref drift.
- Dirty worktrees.
- Existing integration branch names.
- Detached source worktrees.
- Cancellation and recovery.

Every success fixture verifies that source refs are unchanged, no push occurred,
and exactly one new accepted integration branch remains.

### Real Codex smoke tests

Opt-in tests exercise two disposable App Server tasks with an isolated fixture
repository. They are not run on ordinary pull requests because they require
credentials and consume model usage. Release qualification records the Codex
version and smoke-test result.

## 19. CI, Release, and Distribution

- GitHub Actions runs `cargo fmt`, Clippy, unit/integration/E2E tests, dependency
  audit, and license checks.
- Releases provide Linux x86_64 and ARM64 standalone binaries.
- Every release includes SHA-256 checksums and an SBOM.
- CLI, daemon, MCP entry point, Skill, and plugin share one semantic version.
- The repository includes English and Simplified Chinese README files.
- The project is licensed under Apache-2.0.
- The plugin bundle includes the manifest and marketplace metadata required by
  current Codex plugin installation flows.
- App Server adapters are tested against the declared supported Codex version
  set. A newly observed version is unsupported until its generated protocol and
  smoke tests pass.

## 20. Acceptance Criteria for Version 1

Version 1 is ready only when all of the following are demonstrated:

1. A user can select two existing same-repository tasks interactively.
2. The same run can be started non-interactively with two thread IDs.
3. A Codex primary task can launch the background run through the plugin and end
   its initiating turn without becoming a coordinator agent.
4. Both tasks generate contracts from their own existing contexts.
5. The reviewer can reject a plan repeatedly and the primary receives each
   complete revision request.
6. No integration branch exists before exact plan approval.
7. Only the primary task performs Git writes.
8. Final approval applies to the exact current integration SHA.
9. Source refs remain unchanged before and after acceptance.
10. Coordinator termination and restart do not duplicate a task turn.
11. Permission/input pauses can be resumed safely.
12. Unknown Codex/App Server versions fail closed.
13. The terminal CLI and Codex plugin use the same core and produce equivalent
    run results.
14. Linux x86_64 and ARM64 release artifacts pass checksum and smoke checks.

## 21. Confirmed Product Decisions

- Codex App-managed tasks only for v1.
- Same host and same Git common directory only.
- Rust standalone coordinator running on the task host.
- CLI and Codex plugin entry points.
- No third coordinator agent.
- Primary task owns all Git writes.
- Persistent, resumable runs with minimal default logging.
- Fail-closed compatibility beginning with Codex CLI 0.144.5.
- Linux x86_64 and ARM64 releases.
- Apache-2.0 license.
- Repository name: `worktree-merge-consensus`.
