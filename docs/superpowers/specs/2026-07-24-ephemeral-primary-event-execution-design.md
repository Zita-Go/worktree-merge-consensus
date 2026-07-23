# Ephemeral Primary Event-Backed Execution Design

## Problem

`codex-consensus 0.2.7` creates an ephemeral Effective Primary when a loaded
Source Primary does not expose the participant patch MCP server. The fork is
created correctly with `excludeTurns=false`, and the fork response is checked
against the Source Primary history. The coordinator then treats that fork like
a stored task:

- `thread/read(includeTurns=true)` is used to check binding health and recover
  request markers;
- `thread/resume` is used before each action;
- completed turns are polled from stored history.

Codex App Server 0.145.0 rejects all three assumptions for ephemeral tasks.
An observed production Run therefore pauses at `REQUEST_PRIMARY_CONTRACT`
before `turn/start` with:

```text
ephemeral threads do not support includeTurns
```

The process-level test server currently accepts those unsupported operations,
which allowed the incompatible implementation to pass acceptance tests.

## Verified App Server Contract

A no-turn probe against the basestream-cpu App Server 0.145.0 established:

- `thread/fork(ephemeral=true, excludeTurns=false)` returns the complete copied
  history;
- `thread/read(includeTurns=false)` succeeds and returns runtime status with no
  turns;
- `mcpServerStatus/list` succeeds for the loaded ephemeral task;
- `thread/read(includeTurns=true)` is rejected;
- `thread/turns/list` is rejected;
- `thread/resume` is rejected because an ephemeral task has no stored rollout.

The coordinator must adapt to this contract. The App Server must not be
patched or weakened.

## Safety Invariants

1. Source Primary and Reviewer task IDs, worktrees, refs, and SHAs remain
   frozen.
2. A new integration branch remains the only Git mutation target.
3. The initial Source Primary history is read from the stored source task
   before the fork and compared to the complete history returned by the fork.
4. Every later ephemeral generation must derive from the same frozen source
   history fingerprint.
5. The coordinator never calls stored-history or resume APIs for an ephemeral
   binding.
6. Every new ephemeral turn is accepted only from request-bound live event
   evidence persisted in SQLite.
7. A turn whose delivery or completion is uncertain is never automatically
   repeated.
8. A missing ephemeral task may be recreated only at a completed action
   boundary with no pending send.
9. Contracts, plans, feedback, approvals, integration evidence, and test
   evidence remain durable coordinator state and are included in deterministic
   later prompts.

## App Server Client Split

The client exposes two distinct reads:

- `read_thread(thread_id)` requests `includeTurns=true` and is used only for
  stored Source Primary and Reviewer tasks;
- `read_thread_summary(thread_id)` requests `includeTurns=false` and returns
  only `ThreadSummary`.

The reconnecting client applies the same reconnect policy to both methods.
No fallback from a full read to a summary read is allowed because callers must
make the persistence decision explicitly.

## Primary Binding Lifecycle

### Direct binding

A stored Primary configured through `thread/resume`, or one already exposing
the exact participant MCP capability, keeps the existing behavior. Full
history reads, request-marker recovery, and stored turn polling remain valid.

### Ephemeral binding creation

The coordinator:

1. reads the idle Source Primary with complete stored history;
2. confirms the Source Primary has no active goal;
3. computes a canonical ordered-turn-ID fingerprint;
4. creates the ephemeral participant fork;
5. compares the fork response's ordered turn IDs with the source;
6. verifies idle status and exact participant MCP inventory;
7. persists the binding generation and source-history fingerprint.

An older binding without a fingerprint may be replaced only when there is no
pending send. This safely migrates the affected 0.2.7 Run, which failed before
its first turn.

### Ephemeral action preparation

The coordinator checks the Effective Primary with
`read_thread_summary(includeTurns=false)`, waits for idle status using summary
reads, and rechecks the participant MCP inventory. It does not call
`thread/resume`.

### Ephemeral turn delivery

Before `turn/start`, the coordinator persists a start-intent timestamp. After
the response, it persists the returned thread and turn IDs. For ephemeral
bindings:

- a pending record with no start intent is unsent and may be sent;
- a pending record with a start intent but no turn ID is delivery-uncertain and
  must pause without retry;
- a pending record with a turn ID continues waiting for request-bound events.

Stored tasks retain canonical request-marker recovery from full history.

### Ephemeral completion

The coordinator consumes matching `item/started`, `item/completed`, and
`turn/completed` events. Items and the terminal turn are persisted before
normalization. A completed turn is reconstructed from this evidence and passed
through the existing response, execution-policy, patch provenance, and state
transition checks.

Summary reads provide liveness and runtime status only. If the task disappears
before terminal evidence is durable, the action pauses as uncertain.

## Recovery

| Durable state | Recovery |
|---|---|
| No pending send; ephemeral task available | Reuse the active binding |
| No pending send; ephemeral task missing | Recreate a new generation from the frozen source history |
| Pending, no start intent | Continue the unsent deterministic action |
| Start intent, no turn ID | Pause fail-closed; do not resend |
| Turn ID plus durable completion event | Reconstruct and accept the exact turn |
| Turn ID without durable completion event and task missing | Pause fail-closed |

The same Run ID is retained. Rebinding increments the binding generation and
does not create an implicit substitute Run.

## Persistence

The `turns` table gains `turn_start_intent_at INTEGER`. `PendingSend` exposes
that timestamp. The existing event item and completion tables remain the
canonical ephemeral turn journal.

The Primary binding record gains a source-history fingerprint for ephemeral
generations. Direct bindings store no fingerprint.

Migrations are additive and preserve existing Runs. A legacy ephemeral binding
without a fingerprint is never trusted across an uncertain pending action.

## Test Strategy

1. App Server client unit tests assert the exact
   `thread/read(includeTurns=false)` request.
2. Coordinator tests use a fake that rejects full reads and resumes for
   ephemeral tasks.
3. A red-path test proves the old implementation fails immediately after a
   valid fork.
4. Direct, not-loaded direct, and preloaded-without-tool paths complete
   independently.
5. Ephemeral turns complete from event evidence without history polling.
6. A missing mirror is recreated only after a completed boundary.
7. Lost `turn/start` delivery pauses without duplicate execution.
8. The process-level fake enforces the same ephemeral restrictions as App
   Server 0.145.0.
9. Full workspace and end-to-end suites prove source refs remain unchanged and
   no push, pull request, or existing-branch merge occurs.

## Current Run Migration

Run `433797ff-11b2-49b9-9873-ff1179740da8` is paused before its first
`turn/start`: it has no Primary contract, integration branch, integration SHA,
or pending turn. After installation of the fixed version, resuming the same
Run may replace the legacy generation-1 ephemeral binding and retry
`REQUEST_PRIMARY_CONTRACT`. The frozen source refs and worktrees must be
verified unchanged before and after resume.
