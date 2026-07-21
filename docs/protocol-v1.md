# Worktree Merge Consensus Protocol v1

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

Before each protocol turn, the daemon waits for the target task to become idle,
resumes it by its frozen task ID, and only then sends `turn/start`. Reading task
history is not a substitute for resuming a task that App Server reports as
`notLoaded`.

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
roots, disables network and temporary-directory writes, and uses approval
policy `untrusted`; the daemon accepts only a narrow set of branch creation,
exact-SHA merge, staging, commit, and read-only Git commands. The primary
verification turn supplies only the isolated clone as a writable root, remains
offline, uses approval policy `untrusted`, and accepts only exact frozen tests.
App Server may attach a `proposedExecpolicyAmendment` to a one-time command
approval. The coordinator ignores that proposal and returns plain `accept`; it
never applies or persists the amendment. Additional filesystem, network, and
network-policy requests are still cancelled. A unified-exec approval reports
one shell-joined wrapper such as `/bin/bash -lc '<script>'`; the coordinator
removes exactly one known-shell `-c` or `-lc` wrapper and applies the same
allowlist to the inner script. Nested dynamic launchers, non-null subcommand
`approvalId` callbacks, and non-`local` execution environments are cancelled.
Publication, destructive Git operations, shell chaining, wrong-directory
execution, and added permission requests are cancelled at the App Server
request boundary.

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
shells, and unknown items remain terminal. Version 0.1.21 recognizes the App
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
Run or repeat branch creation and merge. Version 0.1.13 renders concrete direct-field payload templates for
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
