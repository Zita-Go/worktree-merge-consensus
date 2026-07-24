# Worktree Merge Consensus Protocol v1

> **Legacy participant protocol:** release 0.2.0 and later prompt for the
> [v2 marker protocol](protocol-v2.md). This document remains authoritative for
> valid v1 envelopes accepted during in-flight migration and for the
> coordinator's canonical internal representation.

This document is the human-readable contract for
[`worktree-merge-consensus/v1`](../schemas/protocol-v1.json). The checked-in JSON
Schema and Rust invariants are authoritative when this document and executable
behavior differ.

## Participants and authority

A run has exactly two existing Codex tasks:

- **Primary:** owns the integration plan and is the only task permitted to
  create or modify the integration branch.
- **Reviewer:** protects the intent and implementation details represented by
  its frozen commit. It may inspect but must not modify Git state.

The daemon is a deterministic protocol coordinator, not a third reviewing
agent. It freezes facts, constructs role-specific prompts, validates structured
responses, persists transitions, executes read-only Git checks, and dispatches
the next allowed action.

## Frozen run identity

Before the first task turn, the coordinator records:

- run UUID;
- both task IDs and worktree paths;
- the shared Git common directory;
- both source commit SHAs and source refs;
- the unique target integration branch;
- user-supplied test commands. Contract and approved-plan test commands are
  added to this set and frozen before integration starts.

Task identity and source identity are selected independently. A task's App
Server cwd is non-authoritative display metadata and may be shared by both
tasks or outside Git. New runs explicitly bind each task ID to one absolute
path returned by registered-worktree discovery. Preflight requires different
task IDs, different clean worktree roots, and one canonical Git common
directory before recording the mapping.

Every frozen test command must be a direct command. Git executables, shell
control operators, and dynamic shell/interpreter command launchers are invalid.
Contract and plan prompts state this same restriction before a task declares
tests; a model-generated violation is not silently removed or executed.

The task IDs, worktrees, common directory, source SHAs, and source refs are
immutable for the life of the run. Source drift, a dirty worktree, a mismatched
repository, or a pre-existing target branch fails closed. A source worktree may
start detached because it is frozen by SHA; an accepted integration result must
be attached to its authorized new local branch.

Before the first Primary action, the daemon establishes a participant binding.
The frozen selected task remains the Source Primary. A `notLoaded` Source is
loaded with coordinator-owned task-scoped MCP configuration and used directly;
a preloaded Source without the exact `consensus_apply_patch` inventory is
represented by an ephemeral full-history `thread/fork` whose goal must be null.
That Effective Primary is not a new source identity. Before every Primary turn,
the daemon resumes the Effective Primary, fully paginates
`mcpServerStatus/list`, requires exactly `consensus_apply_patch`, and only then
sends `turn/start`. Reviewer routing and both frozen source task IDs, worktrees,
refs, and SHAs remain unchanged. A pending request may be rebound to a
replacement ephemeral Effective Primary only when it is proven unsent by the
absence of an effective task ID, turn ID, and turn-start intent. An uncertain
turn is never reforked or resent. This preflight occurs before every
`turn/start` for a Primary action.

Reading task history is not a substitute for resuming a task that App Server
reports as `notLoaded`. Version 0.1.26 treats active-task waiting as a bounded
inactivity wait: the default idle window is 30 minutes and renews only when
canonical task status or turn history changes. A task with no canonical
progress pauses with `COMMUNICATION_FAILURE`; cancellation remains available
while waiting.

For legacy v1 Runs, version 0.2.7 permits only the exact post-0.2.6
`CONTROLLED_PATCH_TOOL_UNAVAILABLE` correction recovery. Explicit resume after
a matching 0.2.8 deployment preserves the same Run, round, integration branch,
old SHA, and failed frozen verification evidence; archives only the empty
side-effect-free correction turn; reacquires the lock; repeats participant
preflight; and permits one request-bound corrective patch and commit. The
integration SHA must advance and all frozen verification reruns. Installing or
enabling the operator plugin alone never mutates a blocked Run.

Version 0.2.9 also permits explicit same-Run recovery of one exact completed
integration turn whose request-bound patch and commit succeeded before the
legacy command audit blocked it. Approved writes must still be completed with
exit code zero; only retry-safe read-only inspections may have a canonical
numeric nonzero result. Recovery revalidates the frozen refs, patch record,
clean target, ancestry, and SHA, archives only that response attempt, and
requests one read-only confirmation without repeating any write. Every
near-match remains terminal.

Version 0.2.10 performs that recovery's initial repository check with the
integration-in-progress policy. It permits the Primary worktree to be attached
to the exact authorized target after a successful commit, but still requires
the Reviewer worktree and frozen source refs to remain unchanged and then
revalidates the authoritative target result.

Version 0.2.11 also recognizes the App Server's canonical
`unifiedExecStartup` value as agent-initiated command provenance when auditing
that completed turn. User-shell, unified-exec interaction, null, malformed, and
unknown sources remain rejected.

Version 0.2.12 permits the pending read-only confirmation from that recovery to
move to a replacement ephemeral generation only when the request is durably
unsent. Binding replacement and pending-request rebinding are one transaction.
The patch record remains attached to the archived completed old generation and
is accepted only across the exact same frozen Source-history lineage. Any
uncertain delivery or identity mismatch remains rejected.

Version 0.2.13 resumes a persisted Source Primary reported as `notLoaded` with
the task-scoped participant configuration before creating that replacement.
It verifies the frozen Source identity and idle state and still never resumes
an ephemeral Effective Primary. The same release can explicitly resume only
the exact 0.2.12 `BLOCKED / HISTORY_UNAVAILABLE` state created at this
proven-unsent boundary. Lock reacquisition requires the unchanged approved
plan, target, one unsent pending request, active binding generation, frozen
history hash, request hash, and archived completed patch attempt. Any sent,
uncertain, or near-match state remains terminal.

Version 0.2.14 recognizes exactly `git symbolic-ref --short HEAD` in the
frozen Primary worktree as a read-only current-branch query, with at most one
canonical `/bin/bash -lc` wrapper. Every other `symbolic-ref` form remains
forbidden. It can explicitly resume only the exact 0.2.13
`BLOCKED / FORBIDDEN_OPERATION` diagnostic that names that wrapped command.
The same Run is reactivated only after the completed request, binding,
successful controlled patch, canonical read-only history, unchanged frozen
sources, clean target, ancestry, and authoritative result all revalidate.
Only the confirmation turn is archived and retried; patch, merge, staging, and
commit are never repeated. Every near-match or side-effectful history remains
terminal.

Preflight reason codes include `UNREGISTERED_WORKTREE`,
`DUPLICATE_WORKTREE`, `REPOSITORY_MISMATCH`, `DIRTY_WORKTREE`, and
`WORKTREE_UNAVAILABLE`. Once frozen, a task may reject an incorrect
user-confirmed association with `SOURCE_BINDING_MISMATCH`; the mapping cannot
be replaced by `resume`.

## Envelope

Every task response is exactly one JSON object. Text outside the object is an
invalid response. The envelope fields are:

| Field | Meaning |
| --- | --- |
| `protocol` | Constant `worktree-merge-consensus/v1`. |
| `run_id` | Frozen run UUID. |
| `message_type` | One of the seven v1 message types. |
| `phase` | Phase in which the message is valid. |
| `round` | Positive review round, checked against current state. |
| `primary_sha` | Frozen 40-hex primary commit. |
| `reviewer_sha` | Frozen 40-hex reviewer commit. |
| `plan_revision` | Exact positive revision where required, otherwise `null`. |
| `integration_branch` | Exact authorized branch after integration, otherwise `null`. |
| `integration_sha` | Exact 40-hex result after integration, otherwise `null`. |
| `reason_code` | Stable reason for changes or blocking, otherwise `null`. |
| `payload` | Message-specific JSON object containing all explanation and evidence. |

The daemon rejects stale rounds, changed frozen SHAs, changed branch identity,
schema violations, missing approval identities, or an approval with a nonempty
`uncovered_items` array.

## Lifecycle

| Phase | Actor and required outcome |
| --- | --- |
| `CONTRACT` | Primary and reviewer independently return `CONTRACT_READY` with behavior, constraints, tests, and protected details. |
| `PLAN_REVIEW` | Primary returns `PLAN_READY`; reviewer returns `CHANGES_REQUIRED` or exact `APPROVED_PLAN`. |
| `INTEGRATE` | After plan approval only, primary creates the new branch at the frozen primary SHA and integrates the reviewer SHA. |
| `VERIFY` | The daemon creates an isolated detached clone of the exact result SHA; a separate primary turn runs every frozen command there, and Git source-ref/ancestry checks validate the result. |
| `RESULT_REVIEW` | Reviewer audits the exact branch, SHA, contracts, plan, and evidence; it returns `CHANGES_REQUIRED` or exact `APPROVED_RESULT`. |
| `ACCEPTED` | Daemon revalidates the approved SHA and unchanged sources, records acceptance, and stops dispatching. |
| `PAUSED_USER_ACTION` | Explicit task input or another external action must be resolved before resume. |
| `BLOCKED` | A terminal protocol or safety condition stopped the run. |
| `CANCELLED` | User cancellation stopped the run without reverting Git state. |

## Message types

### `CONTRACT_READY`

Valid only in `CONTRACT`. Integration identity must be `null`. The payload
captures observable behavior, constraints, files/interfaces, tests, and details
that the later plan and result must preserve. Contract payloads are hashed and
the canonical versions are reused in subsequent prompts. `contract.tests` is a
nonempty array of exact commands. The coordinator binds the responding task's
role before starting the turn, so `payload.role` is only a redundant audit echo:
if present it must be `PRIMARY` or `REVIEWER` as bound, while omission does not
invalidate an otherwise complete contract.

### `PLAN_READY`

Valid only in `PLAN_REVIEW`. It has a positive `plan_revision` and no
integration identity. The payload must contain object-valued
`primary_contract`, `reviewer_contract`, and `plan`, plus an array-valued
`coverage_matrix`, plus a nonempty `test_commands` array. The matrix maps every
contract item to a concrete integration decision and verification method. The
coordinator unions contract, plan, and user commands before integration.

### `CHANGES_REQUIRED`

Valid in `PLAN_REVIEW` or `RESULT_REVIEW`. It requires a nonempty
`reason_code`, the current plan revision, stable issue IDs, and concrete
evidence. During result review it must also identify the exact current branch
and SHA. A changed response fingerprint counts as progress; repeating an
unchanged verdict reaches `NO_PROGRESS` after two unchanged rounds.

### `APPROVED_PLAN`

Valid only in `PLAN_REVIEW`. Its payload must repeat the approved plan revision
and both frozen SHAs, copy the 64-hex `approved_plan_hash` calculated over the
canonical complete `PLAN_READY` payload, and include an empty
`uncovered_items`. Approval authorizes only that exact payload; it does not
authorize integration into an existing branch or any remote operation.

### `INTEGRATION_READY`

Valid in `INTEGRATE` or `VERIFY`, with different evidence rules. Both forms
identify the authorized integration branch and exact HEAD SHA.

During `INTEGRATE`, the primary reports the complete changed-file set and
integration decisions but must not report passing tests. The reported file set
must exactly equal the daemon's read-only
`git diff --name-only primary_sha integration_sha` result. Conflict markers are
stream-scanned across that authoritative set, including large text files. Git
verification also requires both frozen commits to be ancestors and both source
refs unchanged.

Before `VERIFY`, the daemon creates
`verification/<run-id>-<integration-sha>` under the private state directory by
cloning without local object sharing, removing all remotes, and checking out the
exact SHA detached with hooks disabled. Its Git common directory must differ
from the source repository. A separate primary turn may run only every frozen
test command exactly once in that clone and no other command. The response must
report those tests, but the daemon replaces the report with authoritative App
Server command-item evidence. Every accepted entry records `command`, exact
integer `exit_code: 0`, `turn_id`, `item_id`, and absolute `cwd`. Missing,
duplicated, extra, failed, wrong-directory, or merely self-reported commands
yield `TEST_FAILURE` or `FORBIDDEN_OPERATION`. The canonical integration
payload from `INTEGRATE` cannot be replaced during verification.

### `APPROVED_RESULT`

Valid only in `RESULT_REVIEW`. Its payload repeats the approved plan revision,
both frozen SHAs, exact integration branch, and exact integration SHA;
`uncovered_items` must be present and empty. Acceptance is bound to this SHA.
Any primary amendment produces a new SHA and requires another reviewer verdict.

### `BLOCKED`

Requires a nonempty reason code and the exact current phase, round, plan
revision, and integration identity. It reports that the task cannot safely
produce the expected message. Stale blocks are rejected instead of terminating
the current run. If the supplied role worktree does not represent the
implementation in that task's history, it must use
`SOURCE_BINDING_MISMATCH` and may not search for or switch to another source.

## Bounded review

The v1 defaults are six review rounds and two unchanged-review fingerprints.
Exceeding them yields `ROUND_LIMIT` or `NO_PROGRESS`. Malformed responses yield
`INVALID_RESPONSE`; test evidence failures yield `TEST_FAILURE`. Safety reasons
such as `SOURCE_DRIFT`, `DIRTY_WORKTREE`, `MISSING_SOURCE_ANCESTRY`, and
`UNEXPECTED_INTEGRATION_BRANCH` also fail closed.

Unexpected command, file-write, network, or permission escalation is denied and
terminates with `FORBIDDEN_OPERATION`. Explicit task input may pause as
`PERMISSION_REQUIRED`. Communication and history errors are never approvals.

## App Server execution policy

The App Server connection declares `capabilities.experimentalApi: true` during
`initialize`. Every turn explicitly selects the same-host `local` execution
environment with the authorized absolute cwd, so command and file tools remain
available without inheriting an arbitrary sticky environment. Irrespective of the task's initial or subsequently
reported cwd, contract, plan, and review turns supply the explicitly bound
absolute role worktree, one runtime workspace root, a read-only sandbox, network
disabled, and approval policy `never`. The authorized primary integration turn
supplies only the primary worktree and source Git common directory as writable
roots and disables network and temporary-directory writes. The primary
verification turn supplies only the isolated clone as a writable root and
remains offline. Both write-capable turns also use approval policy `never`, so
the workflow never waits for interactive command or file approval. The daemon
admits a result only when persisted command evidence contains the narrow set of
branch creation, exact-SHA merge, staging, commit, read-only Git, and exact
frozen-test commands allowed for that phase. Additional filesystem, network,
and network-policy requests are still rejected. A unified-exec item reports
one shell-joined wrapper such as `/bin/bash -lc '<script>'`; the coordinator
removes exactly one known-shell `-c` or `-lc` wrapper and applies the same
allowlist to the inner script. Nested dynamic launchers, non-null subcommand
`approvalId` callbacks, and non-`local` execution environments fail the Run.
Publication, destructive Git operations, shell chaining, wrong-directory
execution, and added permission requests cannot be accepted. The pinned offline
sandbox is the preventive boundary; `never` intentionally removes interactive
pre-execution review inside those writable roots.

## Durability and delivery

SQLite stores versioned run facts, canonical protocol payloads, state
transitions, and an outbox-style pending send before the App Server turn is
started. Accepted response hashes make duplicate notifications idempotent.
After daemon restart, an incomplete send is reconciled with its delivery
identity in task history before the state machine continues. Extra historical
protocol-looking messages never replace the persisted contracts, plan,
approval, or integration payload. Unknown persisted-state schema versions fail
closed instead of receiving permissive defaults. If a daemon crashes after a
verification turn starts, recovery may reuse its test-dirty clone only while a
matching send remains pending; exact detached HEAD, independent Git common
directory, and no-remote invariants are still mandatory.

An explicit resume after `COMMUNICATION_FAILURE` reads the exact persisted turn
from canonical task history. A `failed` or `interrupted` attempt is archived and
the same deterministic request may receive one new turn only when every
canonical item is side-effect-free. Command execution, file changes, missing
history, and unknown item types fail closed instead of being duplicated.

`INVALID_TEST_COMMAND` from a contract or plan declaration is a correctable
pre-integration model-output error. New runs pause while preserving their exact
action. On explicit resume, the coordinator revalidates the frozen sources and
reads the exact completed turn from canonical history. It may archive and
replace that turn only when the original action used the read-only execution
policy and every item is a message, reasoning, completed command execution, or
a completed query to this plugin's exact `consensus_list_threads`,
`consensus_list_worktrees`, or `consensus_status` tool. Mutating, external, and
unknown MCP calls, file changes, incomplete commands, missing history, and
unknown items fail closed. The replacement prompt explicitly forbids Git test
commands and recursive `consensus_*` calls. For the legacy terminal state
emitted by version 0.1.9, version 0.1.10 and later restore the same run and
reacquire its repository lock in the same SQLite transaction that archives the
old attempt. Version 0.1.12 applies the same exact-turn, canonical-history, and
atomic-lock safeguards to a pre-integration `BLOCKED / INVALID_RESPONSE` caused
by malformed model output. Only contract, primary-plan, and reviewer-plan
verdict actions are eligible; post-integration, side-effectful, incomplete,
external, and unknown histories remain terminal. Other `BLOCKED` states remain
terminal except the exact side-effect-free pre-integration
`EXECUTION_TOOL_UNAVAILABLE` case added in 0.1.14. That recovery requires the
accepted primary turn and response hash to match canonical history, a blocker
payload that explicitly reports no writes, no command/file-change items, both
frozen worktrees clean, both source refs unchanged, and the target integration
branch absent. The same version explicitly selects environment `local` with the
authorized cwd on every turn; it never sends an empty environment selection,
which would disable execution tools. Version 0.1.15 adds one more narrow
recovery for a first-integration `BLOCKED / FORBIDDEN_OPERATION`: the exact
pending turn must be canonically `failed` or `interrupted`, contain no
side-effect-capable item, have no integration identity or test evidence, and
pass strict frozen-source and absent-target-branch checks before the run lock
is reacquired and the attempt is archived. All other forbidden-operation
states remain terminal. Version 0.1.16 removes exactly one App Server-generated
known-shell `-c` or `-lc` wrapper before applying the inner-command allowlist.
Version 0.1.17 adds only the exact target-ref
`git show-ref --verify refs/heads/<target-integration-branch>` preflight.
Version 0.1.19 additionally permits only the equivalent exact
`git branch --list <target-integration-branch>` query; every other `git branch`
form remains forbidden. During
the same-run forbidden-operation recovery, terminal command items are allowed
only when they are frozen-primary-cwd, policy-valid read-only Git queries;
Version 0.1.20 also identifies coordinator-authored Primary and Reviewer turns
as internal participants for which the launcher skill is inapplicable. A
same-run retry may discard the exact denied legacy `sed -n 1,240p` read of this
plugin's semver-versioned `SKILL.md`, but that read is never admitted to the
live integration execution allowlist. `inProgress`, writes, wrong cwd, nested
shells, and unknown items remain terminal except for the exact v0.1.24
controlled-patch approval recovery below. Version 0.1.21 recognizes the App
Server's internal `contextCompaction` lifecycle marker during retry auditing
only when its object has exactly a nonempty `id` and the fixed `type`; any
additional field remains terminal. Version 0.1.22 allows exactly
`rg --files -g AGENTS.md` in the frozen primary cwd for repository-instruction
discovery, keeps every other `rg` form denied, and directs all other tracked-file
inspection through the existing read-only Git command policy. Version 0.1.23
adds `consensus_apply_patch`, a participant-only capability bound to the exact
active Primary Run and request hash. It accepts one successful text patch of at
most 512 KiB only on the clean authorized branch after both frozen commits are
ancestors, uses Git preflight without unsafe paths, revalidates source refs, and
records single use in SQLite. An exact completed
`FILE_CHANGE_TOOL_UNAVAILABLE` turn after a communication pause may be archived
and replaced only when its approved identity, bwrap permission evidence,
reported merge SHA, clean target branch, both source ancestors, and frozen refs
all match. The replacement reuses the existing merge and cannot create a new
Run or repeat branch creation and merge. Version 0.1.24 requires the effective
per-tool setting
`plugins.worktree-merge-consensus.mcp_servers.worktreeMergeConsensus.tools.consensus_apply_patch.approval_mode = "approve"`
before Run start or controlled-patch resume. After explicit same-Run resume, a
canonically `waitingOnApproval` Primary integration turn may be interrupted and
retried only when it contains exactly one request-bound `inProgress`
`consensus_apply_patch` item, every command item completed successfully and
still passes the integration allowlist, no successful patch record exists, the
authorized target is clean with both frozen source ancestors, and both source
refs remain frozen. Unknown or multiple tool calls, other incomplete items,
mismatched arguments, drift, or possible writes fail closed. If the turn
completes during the interrupt race, its completed result is reused rather than
duplicated. Version 0.1.25 additionally recognizes the exact completed
rejection race that can occur when App Server continues the old approval after
configuration hot reload while the Run remains paused. Explicit same-Run
resume may archive and retry only a Primary turn with exactly one request-bound
`consensus_apply_patch` item in `failed` status and an exact
`BLOCKED / PATCH_NOT_AUTHORIZED` response. No successful patch record may
exist, the authorized target must be clean at the reported merge SHA with both
frozen ancestors, and source refs must remain unchanged. The existing merge is
reused; unknown or additional tools, possible writes, mismatched evidence, or
drift fail closed. Version 0.1.26 also recognizes the same exact failed patch
call and blocker when App Server has persisted a final assistant message but
left the Primary turn `inProgress` with `waitingOnApproval`. It first applies
all 0.1.25 checks, then interrupts and atomically archives only that stale turn
before retrying the same request. Every other `inProgress` failed-tool shape,
missing final response, mismatch, possible write, or drift remains terminal.
Version 0.1.27 adds one narrower missing-final exception: the exact
request-bound patch call must already be canonically `failed`, no agent message
may be present, every command must be completed successfully and pass the
integration allowlist, and SQLite must contain no successful patch record. The
daemon snapshots the clean authorized merge SHA, interrupts only that stale
turn, and requires the same clean SHA afterward before atomically archiving and
retrying it. Any unknown item, ambiguous execution, target movement, or source
drift fails closed.
Version 0.1.28 defines the blocker's authority boundary precisely. Its protocol
envelope plus direct request hash, approved plan revision and hash, frozen source
SHAs, target branch, and resulting merge SHA are mandatory. `payload.role` is
redundant with the persisted pending-send task binding, and
`blocking_condition` is free-form prose; either may be absent. This does not
relax canonical item checks, SQLite no-write proof, or repository revalidation.
Version 0.1.13 renders concrete direct-field payload templates for
`APPROVED_PLAN` and `APPROVED_RESULT`; the checked-in JSON Schema requires those
approval identity fields at payload top level rather than accepting a nested
identity object.

## Git postconditions

An accepted run guarantees only a new local branch and exact commit SHA:

- the branch did not exist when the run froze;
- both frozen commits are ancestors of the accepted SHA;
- both frozen source refs still point to their original SHAs;
- the primary worktree is on the authorized branch at the accepted SHA;
- every frozen test has authoritative passing command-item evidence from the
  isolated exact-SHA clone;
- the reviewer approved that exact SHA.

The persisted `accepted_result` repeats the branch and SHA, both source SHAs,
structured test results, `source_refs_unchanged: true`, and an explicit
publication boundary: local-only, not pushed, no PR, and not merged into an
existing branch.

The protocol never pushes, opens a PR, updates an existing target branch, or
deletes source or integration state.

## Versioning

Any incompatible envelope, message, approval, or safety change requires a new
protocol identifier and schema. Additive documentation clarifications do not.
Codex App Server compatibility is versioned separately in
[the adapter policy](compatibility.md).
