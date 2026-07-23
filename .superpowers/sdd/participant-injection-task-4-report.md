# Task 4 Report: Strict Same-Run Recovery for the Corrective Patch-Tool Blocker

Date: 2026-07-23

## Status

Implemented and verified on `diagnostic/app-server-error`, based on
`4474737f3e3cdef7b639f5ae2797ef0fc0e7ec5e`.

The implementation is generic and test-only with respect to recovery data. It
did not connect to, resume, or mutate the live production Run. It did not
create, move, merge, or modify any source branch. The pre-existing untracked
`downloads/` directory was left untouched.

## Implementation

### Exact state transition

Added:

```text
RunState::retry_blocked_corrective_patch_tool_unavailable()
```

The transition accepts only an exact terminal
`CONTROLLED_PATCH_TOOL_UNAVAILABLE` corrective shape:

- `BLOCKED` status, `BLOCKED` phase, and `STOP` next action;
- no unrelated persisted diagnostic;
- an approved plan and frozen target integration branch;
- a current integration branch, SHA, and payload;
- the current branch equal to the authorized target branch;
- a correction round after the initial result round;
- complete frozen test evidence with at least one nonzero exit;
- retained nonempty `machine_verification.failed_tests` feedback;
- no accepted result and no approved result SHA.

The transition restores only:

```text
status = RUNNING
phase = INTEGRATE
next_action = REQUEST_PRIMARY_INTEGRATION
reason_code = null
```

It preserves the exact Run id, round, source facts, target branch, current
integration branch and SHA, integration payload, failed test evidence, and
machine-derived failure feedback.

`RunState::apply_integration` now rejects a corrective
`INTEGRATION_READY` result that reports the already-recorded integration SHA
with `STALE_INTEGRATION_SHA`. This keeps the correction request bound to an
actual advancing commit.

### Canonical coordinator inspection

Added a dedicated corrective-patch recovery path. It does not widen or reuse
the pre-integration `EXECUTION_TOOL_UNAVAILABLE` validator.

Before reactivation, the coordinator:

- recomputes the exact retryable state shape through the new state method;
- rechecks the controlled-patch approval configuration;
- calls the full repository integration verifier on the recorded branch, SHA,
  and changed-file payload;
- thereby requires the exact clean target at the recorded SHA, both frozen
  source commits as ancestors, unchanged source refs, the frozen reviewer
  worktree, and the frozen repository/worktree identities;
- loads the latest accepted Primary `INTEGRATE` turn for the same correction
  round and thread;
- requires the canonical turn to be `completed`;
- requires the deterministic request marker for the persisted message hash;
- merges persisted item/completion event evidence before classifying the turn;
- rejects every command, file-change, MCP, dynamic-tool, or unknown item;
- requires exactly the
  `BLOCKED:CONTROLLED_PATCH_TOOL_UNAVAILABLE` participant signal;
- reconstructs the blocker response from the pre-response active state and
  verifies its canonical response hash;
- rejects any successful request-bound patch residue.

The resume route calls `verify_integration`, not branch-absence or integration
creation, so it neither remerges nor creates or moves the integration branch.

The controlled patch endpoint now recognizes either:

- the existing exact initial-integration shape; or
- an exact active corrective shape that round-trips through
  `retry_blocked_corrective_patch_tool_unavailable`.

All existing request identity checks remain in force. The reused pending
message hash binds the patch to the retried correction request, and the
successful-patch journal still permits only one successful controlled patch
for that request. The subsequent `INTEGRATION_READY` result must report a SHA
different from the preserved pre-correction SHA.

### Atomic store recovery

Added:

```text
SqliteRunStore::reactivate_blocked_run_with_corrective_patch_tool_retry(...)
```

The store independently recomputes and compares the exact resumed state, then
performs one SQLite transaction that:

- verifies the persisted Run still equals the supplied blocked state;
- verifies the exact accepted Primary integration turn, including round,
  request hash, response hash, thread, turn, and accepted delivery state;
- rejects any prior archived retry for this request;
- rechecks that no successful patch record exists for the request;
- atomically reacquires the repository lock, failing closed on a conflicting
  active Run;
- calls the existing `archive_and_reset_turn` helper for only the accepted
  empty blocker attempt;
- removes stale turn item/completion event rows;
- clears thread, turn, response, and acceptance identity on the reusable
  pending turn;
- stamps the current crash-safe participant capability generation;
- persists the resumed Run and transition.

Any validation, patch-residue, or lock failure rolls the transaction back
without changing the blocked Run or pending-turn state. A repeated resume is
rejected.

## Files

- `crates/core/src/state.rs`
- `crates/core/tests/state_machine.rs`
- `crates/daemon/src/store.rs`
- `crates/daemon/src/coordinator.rs`
- `crates/daemon/tests/store.rs`
- `crates/daemon/tests/coordinator.rs`
- `.superpowers/sdd/participant-injection-task-4-report.md`

## Test-Driven Development Evidence

### Baseline

Before Task 4 changes:

- state-machine suite: 29 passed;
- store suite: 26 passed;
- coordinator suite: 77 passed.

### State RED

Command:

```bash
cargo test --locked -p consensus-core --test state_machine corrective_patch_tool
```

Result: exit 101 with six `E0599` compilation errors because
`retry_blocked_corrective_patch_tool_unavailable` did not exist.

The RED tests covered exact preservation plus rejection of missing failed
evidence, an accepted result, absent integration identity, the wrong reason,
and a pre-integration shape.

### Store RED

Command:

```bash
cargo test --locked -p consensus-daemon --test store corrective_patch_tool
```

Result: exit 101 with four `E0599` compilation errors because
`reactivate_blocked_run_with_corrective_patch_tool_retry` did not exist.

### Coordinator RED

Command:

```bash
cargo test --locked -p consensus-daemon --test coordinator corrective_patch_tool
```

The initial RED run had five failures and one pass: the positive path remained
terminal (`NOT_PAUSED`) and the negative fixtures could not reach their new
dedicated recovery checks.

The repository-negative fixture was then strengthened to prove that resume
uses the corrective integration-verification route:

```bash
cargo test --locked -p consensus-daemon --test coordinator \
  corrective_patch_tool_retry_revalidates_target_sources_and_ancestry -- --exact
```

Result: exit 101 at the new assertion, with controlled-patch approval request
count `left: 1`, `right: 2`. This retained exact RED evidence that the
dedicated correction recovery route had not yet been wired.

### GREEN during implementation

- state corrective tests: 7 passed;
- full state-machine suite: 36 passed;
- store corrective tests: 3 passed;
- coordinator corrective tests: 6 passed.

The first full store/coordinator run exposed two fixture issues rather than
production defects:

- stale turn-event setup had been inserted in an unrelated store test;
- three pre-existing failed-verification correction fixtures still reported
  the pre-correction SHA.

The event setup was moved to the corrective store seed. The JSON correction
fixtures were changed to the advancing SHA. The remaining marker fixture was
reproduced exactly:

```bash
cargo test --locked -p consensus-daemon --test coordinator \
  coordinator_owned_verification_runs_after_nonzero_and_routes_bounded_diagnostics -- --exact
```

Before the fixture correction: exit 101, final status `Blocked` instead of
`Accepted`.

Root cause: its `RecordingSafety` double returned the original integration SHA
for both authoritative results, so the new stale-SHA guard correctly rejected
the correction. A dedicated `AdvancingIntegrationSafety` returns the original
SHA once and the corrected SHA on the later correction.

After the fixture correction: 1 passed, 0 failed.

### Self-review RED/GREEN: request-bound corrective patch

The first requirements self-review found that the recovered state correctly
preserved the current integration branch/SHA, but the controlled patch endpoint
still authorized only the initial integration shape where those fields were
absent. A resumed correction therefore could not perform the one patch allowed
by the brief.

A regression was added before changing production code:

```bash
cargo test --locked -p consensus-daemon --test coordinator \
  corrective_patch_tool_retry_allows_exactly_one_request_bound_patch -- --exact
```

RED result: exit 101. The exact request-bound patch failed with
`PATCH_NOT_AUTHORIZED` and detail `controlled patch is limited to the active
primary integration turn before a result is reported`.

The minimal implementation recognizes an exact active correction by cloning
the state, applying the production blocker shape, and requiring the same strict
state retry validator to accept it. It does not loosen the initial integration
shape or any request/thread/round checks.

GREEN result: 1 passed, 0 failed. The regression also proves a wrong request
hash is rejected, the exact request succeeds once from the recorded base SHA,
and a second patch is rejected as `PATCH_ALREADY_APPLIED`.

## Negative Fixtures

State-machine fixtures reject:

- failed verification evidence removed or changed to all-success;
- an already accepted result;
- absent current integration identity;
- a reason other than `CONTROLLED_PATCH_TOOL_UNAVAILABLE`;
- a pre-integration blocker shape;
- a retried correction that does not advance the integration SHA.

Canonical coordinator fixtures reject:

- `commandExecution`;
- `fileChange`;
- `mcpToolCall`;
- `dynamicToolCall`;
- an unknown future item type;
- a missing or changed deterministic request marker;
- a changed blocker response hash;
- successful patch residue for the request;
- missing failed verification evidence;
- a dirty integration target;
- an integration target moved from the recorded SHA;
- frozen source/ref/worktree drift;
- missing frozen source ancestry;
- a conflicting repository lock;
- a repeated resume.

Store fixtures independently reject without mutation:

- successful request-bound patch residue;
- a conflicting repository lock;
- a repeated archive/reset attempt.

The positive coordinator fixture additionally proves:

- exact Run id and correction round preservation;
- exact source facts, branch, SHA, payload, evidence, and failure-feedback
  preservation;
- no branch-absence/recreation call during resume;
- exact archival of the blocker turn and clearing of its pending identity;
- the retried request receives the retained machine diagnostic and old SHA;
- the retried request may apply exactly one patch bound to its reused request
  hash, while a wrong hash and second patch fail closed;
- the later correction advances to the corrected SHA and reaches acceptance.

## Final Verification

All commands used the brief's pinned Rust toolchain PATH.

```bash
cargo fmt --all -- --check
```

Passed.

```bash
git diff --check
```

Passed.

```bash
cargo test --locked -p consensus-daemon --test store
```

Passed: 29 passed, 0 failed.

```bash
cargo test --locked -p consensus-daemon --test coordinator
```

Passed: 84 passed, 0 failed.

```bash
cargo test --locked --workspace
```

The sandboxed attempt reached and passed the Task 4 state, store, and
coordinator suites, then the daemon lifecycle suite failed because local Unix
socket creation returned `Operation not permitted`, the environment
restriction anticipated in the task brief.

The complete workspace command was rerun with local Unix socket access and
passed: 287 passed, 0 failed, including 36 state-machine tests, 29 store tests,
84 coordinator tests, 18 end-to-end acceptance tests, and all doc tests.

```bash
cargo clippy --locked --workspace --all-targets -- -D warnings
```

Passed with no warnings.

## Self-Review

- Compared every branch of the new state method against the exact production
  shape and confirmed unrelated blocker reasons remain routed to their
  existing validators.
- Confirmed the pre-integration `EXECUTION_TOOL_UNAVAILABLE` helper and
  `validate_execution_tool_unavailable_blocker` were not widened or changed.
- Confirmed repository checks are read-only and happen before reactivation.
- Confirmed branch absence, branch creation, merge, and branch movement are
  not part of corrective resume.
- Confirmed response identity is bound both to canonical task history and the
  accepted SQLite turn.
- Confirmed patch residue is checked before canonical recovery and rechecked
  inside the mutation transaction.
- Confirmed lock acquisition, exact attempt archival, event cleanup,
  capability-generation reset, Run update, and transition insert are atomic.
- Confirmed a repeated resume fails closed and the correction must advance the
  integration SHA.
- Found and fixed the initial controlled-patch authorization gap for an active
  correction using an additional RED/GREEN regression; exact request binding
  and one-successful-patch enforcement remain unchanged.
- Confirmed Task 3 participant preflight behavior remains active and the
  existing archive/reset helper preserves its crash-safe
  `turns.capability_generation` invariant.
- Reviewed the complete diff for scope. Only the six assigned Rust files and
  this report are changed; `downloads/` remains the only unrelated untracked
  path.

## Concerns

- No known implementation concern remains.
- The workspace suite needs local Unix socket access for daemon lifecycle and
  server tests; with that expected permission, it passes completely.
- The live production Run was intentionally not resumed or inspected by this
  task.
