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
- all required test commands.

The task IDs, worktrees, common directory, source SHAs, and source refs are
immutable for the life of the run. Source drift, a dirty worktree, a detached
primary, a mismatched repository, or a pre-existing target branch fails closed.

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
| `VERIFY` | Required commands and Git source-ref/ancestry checks validate the reported integration. |
| `RESULT_REVIEW` | Reviewer audits the exact branch, SHA, contracts, plan, and evidence; it returns `CHANGES_REQUIRED` or exact `APPROVED_RESULT`. |
| `ACCEPTED` | Daemon revalidates the approved SHA and unchanged sources, records acceptance, and stops dispatching. |
| `PAUSED_USER_ACTION` | An external permission or task action must be resolved before explicit resume. |
| `BLOCKED` | A terminal protocol or safety condition stopped the run. |
| `CANCELLED` | User cancellation stopped the run without reverting Git state. |

## Message types

### `CONTRACT_READY`

Valid only in `CONTRACT`. Integration identity must be `null`. The payload
captures observable behavior, constraints, files/interfaces, tests, and details
that the later plan and result must preserve. Contract payloads are hashed and
the canonical versions are reused in subsequent prompts.

### `PLAN_READY`

Valid only in `PLAN_REVIEW`. It has a positive `plan_revision` and no
integration identity. The payload must contain object-valued
`primary_contract`, `reviewer_contract`, and `plan`, plus an array-valued
`coverage_matrix`. The matrix maps every contract item to a concrete integration
decision and verification method.

### `CHANGES_REQUIRED`

Valid in `PLAN_REVIEW` or `RESULT_REVIEW`. It requires a nonempty
`reason_code`, the current plan revision, stable issue IDs, and concrete
evidence. During result review it must also identify the exact current branch
and SHA. A changed response fingerprint counts as progress; repeating an
unchanged verdict reaches `NO_PROGRESS` after two unchanged rounds.

### `APPROVED_PLAN`

Valid only in `PLAN_REVIEW`. Its payload must repeat the approved plan revision
and both frozen SHAs, and `uncovered_items` must be present and empty. Approval
authorizes only that exact plan; it does not authorize integration into an
existing branch or any remote operation.

### `INTEGRATION_READY`

Valid in `INTEGRATE` or `VERIFY`. It identifies the authorized integration
branch and exact HEAD SHA. Its payload includes changed files, conflict
decisions, coverage, and test evidence. Every configured test command needs a
matching passing result. Git verification also requires both frozen commits to
be ancestors of the result and both source refs to remain unchanged.

### `APPROVED_RESULT`

Valid only in `RESULT_REVIEW`. Its payload repeats the approved plan revision,
both frozen SHAs, exact integration branch, and exact integration SHA;
`uncovered_items` must be present and empty. Acceptance is bound to this SHA.
Any primary amendment produces a new SHA and requires another reviewer verdict.

### `BLOCKED`

Requires a nonempty reason code. It reports that the task cannot safely produce
the expected message. The state machine converts terminal blocks to `BLOCKED`
instead of improvising a next step.

## Bounded review

The v1 defaults are six review rounds and two unchanged-review fingerprints.
Exceeding them yields `ROUND_LIMIT` or `NO_PROGRESS`. Malformed responses yield
`INVALID_RESPONSE`; test evidence failures yield `TEST_FAILURE`. Safety reasons
such as `SOURCE_DRIFT`, `DIRTY_WORKTREE`, `MISSING_SOURCE_ANCESTRY`, and
`UNEXPECTED_INTEGRATION_BRANCH` also fail closed.

`PERMISSION_REQUIRED` pauses for user action. Communication and history errors
are surfaced with stable reason codes and are never treated as approval.

## Durability and delivery

SQLite stores run facts, state transitions, messages, and an outbox-style
pending send before the App Server turn is started. Accepted response hashes
make duplicate notifications idempotent. After daemon restart, an incomplete
send is reconciled with canonical task history before the state machine
continues. A response absent from task history is not accepted merely because a
notification was observed.

## Git postconditions

An accepted run guarantees only a new local branch and exact commit SHA:

- the branch did not exist when the run froze;
- both frozen commits are ancestors of the accepted SHA;
- both frozen source refs still point to their original SHAs;
- the primary worktree is on the authorized branch at the accepted SHA;
- configured tests have passing evidence;
- the reviewer approved that exact SHA.

The protocol never pushes, opens a PR, updates an existing target branch, or
deletes source or integration state.

## Versioning

Any incompatible envelope, message, approval, or safety change requires a new
protocol identifier and schema. Additive documentation clarifications do not.
Codex App Server compatibility is versioned separately in
[the adapter policy](compatibility.md).
