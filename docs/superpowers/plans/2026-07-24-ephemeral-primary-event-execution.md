# Ephemeral Primary Event-Backed Execution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make ephemeral Effective Primary tasks operate entirely through summary reads and durable live-event evidence while preserving exact-once safety and same-Run recovery.

**Architecture:** Split stored-history reads from runtime-summary reads in the App Server client. Keep stored Source/Reviewer and direct Primary behavior unchanged; route ephemeral binding health, action preparation, turn completion, and recovery through summary reads plus the existing SQLite event journal. Persist start intent and source-history identity so uncertain delivery or changed source context always fails closed.

**Tech Stack:** Rust 2024, Tokio, serde/serde_json, rusqlite, Codex App Server JSON-RPC v2, Cargo unit and process-level end-to-end tests.

## Global Constraints

- Codex CLI support remains `>=0.144.1` with no upper bound.
- Never modify either frozen source branch, worktree, ref, or SHA.
- Never push, create a pull request, or merge into an existing branch.
- Never call `thread/read(includeTurns=true)`, `thread/turns/list`, or `thread/resume` for an ephemeral binding.
- Never automatically repeat a delivery-uncertain turn.
- Preserve the participant MCP preflight and request-bound single-patch policy.
- Preserve the user-owned untracked `downloads/` directory.

---

### Task 1: Encode the App Server ephemeral read boundary

**Files:**
- Modify: `crates/app-server-client/src/client.rs`
- Modify: `crates/app-server-client/tests/client.rs`
- Modify: `crates/daemon/tests/coordinator.rs`

**Interfaces:**
- Produces: `AppServer::read_thread_summary(&self, thread_id: &str) -> Result<ThreadSummary, AppServerError>`
- Preserves: `AppServer::read_thread` as the stored-history read

- [ ] **Step 1: Write the failing request-shape test**

Extend `typed_methods_emit_the_pinned_v2_request_shapes` so the fake transport
expects:

```rust
let summary_read = read_request(&mut lines).await;
assert_eq!(summary_read["method"], "thread/read");
assert_eq!(
    summary_read["params"],
    json!({"threadId": "fork-1", "includeTurns": false})
);
respond(
    &mut server_write,
    &summary_read,
    json!({"thread": thread_with_id("fork-1")}),
)
.await;
```

Call `client.read_thread_summary("fork-1")` and assert identity and idle status.

- [ ] **Step 2: Run the targeted client test and verify RED**

Run:

```bash
cargo test -p app-server-client --test client typed_methods_emit_the_pinned_v2_request_shapes -- --exact
```

Expected: compile failure because `read_thread_summary` is absent.

- [ ] **Step 3: Implement the summary-read API**

Add the trait method and implementations. The concrete request is:

```rust
let raw = self
    .rpc_request(
        "thread/read",
        json!({"threadId": thread_id, "includeTurns": false}),
    )
    .await?;
Ok(parse_thread_response(raw)?.summary)
```

The reconnecting implementation retries once after reconnect using operation
name `thread/read summary`.

- [ ] **Step 4: Update the coordinator fake implementation**

Implement `read_thread_summary` by returning `self.detail(thread_id).summary`.
Keep `read_thread` available for stored tasks so later tests can make
ephemeral full reads fail explicitly.

- [ ] **Step 5: Run the targeted client and coordinator compile tests**

Run:

```bash
cargo test -p app-server-client --test client typed_methods_emit_the_pinned_v2_request_shapes -- --exact
cargo test -p consensus-daemon --test coordinator --no-run
```

Expected: PASS.

### Task 2: Persist delivery intent and frozen source-history identity

**Files:**
- Modify: `crates/daemon/src/store.rs`
- Modify: `crates/daemon/src/participant_binding.rs`
- Modify: `crates/daemon/tests/store.rs`
- Modify: `crates/daemon/tests/coordinator.rs`

**Interfaces:**
- Adds: `PendingSend::turn_start_intent_at: Option<i64>`
- Adds: `PrimaryParticipantBinding::source_history_hash: Option<String>`
- Changes: `record_turn_start_intent(run_id, message_hash)` to durably set `turn_start_intent_at`
- Changes: ephemeral binding activation to require and preserve a source-history hash

- [ ] **Step 1: Add failing migration and state tests**

Assert that a newly recorded pending send has no intent, calling
`record_turn_start_intent` sets a non-null intent, and reopening the database
preserves it. Assert that an ephemeral binding stores a nonempty history hash
and a direct binding stores `None`.

- [ ] **Step 2: Run the store tests and verify RED**

Run:

```bash
cargo test -p consensus-daemon --test store turn_start_intent -- --nocapture
cargo test -p consensus-daemon --test store primary_binding_history -- --nocapture
```

Expected: compile or assertion failures for missing fields/columns.

- [ ] **Step 3: Add additive schema migrations**

Add:

```sql
ALTER TABLE turns ADD COLUMN turn_start_intent_at INTEGER;
ALTER TABLE primary_participant_bindings ADD COLUMN source_history_hash TEXT;
```

Include both columns in fresh schema creation, decoding, and all binding/pending
queries. Reject empty hashes and reject a new ephemeral generation whose hash
differs from an earlier non-null ephemeral hash for the same Run.

- [ ] **Step 4: Implement durable intent recording**

Update only the matching active pending row:

```sql
UPDATE turns
SET turn_start_intent_at = COALESCE(turn_start_intent_at, ?1)
WHERE run_id = ?2 AND message_hash = ?3
  AND delivery_state = 'PENDING'
  AND thread_id IS NULL AND turn_id IS NULL
```

Repeated calls remain idempotent.

- [ ] **Step 5: Run store tests**

Run:

```bash
cargo test -p consensus-daemon --test store
```

Expected: PASS.

### Task 3: Route ephemeral preparation and delivery safely

**Files:**
- Modify: `crates/daemon/src/coordinator.rs`
- Modify: `crates/daemon/tests/coordinator.rs`

**Interfaces:**
- Adds: `read_thread_summary_with_retry`
- Adds: `wait_until_idle_summary`
- Uses: `PreparedActionThread.primary_binding.mode`

- [ ] **Step 1: Make the fake reject unsupported ephemeral operations**

Configure `FakeAppServer::read_thread` to return the observed
`ephemeral threads do not support includeTurns` error for mirror IDs, and
`resume_thread` to return `no rollout found` for mirror IDs. Allow
`start_turn` on a mirror without a resume ticket.

- [ ] **Step 2: Add a failing successful-flow test**

Start with `without_primary_participant()`, drive the Run, and assert:

```rust
assert_eq!(result.status, RunStatus::Accepted);
assert!(app.full_history_reads_for_mirrors().is_empty());
assert!(app.resumes().iter().all(|id| !id.contains("-consensus-mirror-")));
```

- [ ] **Step 3: Run the test and verify RED**

Run:

```bash
cargo test -p consensus-daemon --test coordinator preloaded_primary_uses_ephemeral_summary_reads -- --exact --nocapture
```

Expected: Run pauses on the first forbidden mirror full-history read.

- [ ] **Step 4: Implement summary-only binding health and preparation**

For `EPHEMERAL_FORK`:

- verify the active binding with `read_thread_summary_with_retry`;
- wait for idle using summary reads;
- skip `thread/resume`;
- recheck the exact participant MCP inventory.

For `DIRECT` and Reviewer tasks retain the existing full-read/resume path.

- [ ] **Step 5: Make delivery recovery mode-aware**

For ephemeral bindings, never search history for a request marker. Use:

```rust
match (&pending.turn_id, pending.turn_start_intent_at) {
    (Some(turn_id), _) => continue_waiting(turn_id),
    (None, Some(_)) => fail_delivery_uncertain(),
    (None, None) => start_new_turn(),
}
```

Stored tasks retain canonical request-marker recovery.

- [ ] **Step 6: Run the targeted tests**

Run:

```bash
cargo test -p consensus-daemon --test coordinator preloaded_primary_uses_ephemeral_summary_reads -- --exact --nocapture
cargo test -p consensus-daemon --test coordinator missing_mirror_with_uncertain_turn_is_never_reforked -- --exact --nocapture
```

Expected: PASS.

### Task 4: Complete ephemeral turns from durable events

**Files:**
- Modify: `crates/daemon/src/coordinator.rs`
- Modify: `crates/daemon/tests/coordinator.rs`

**Interfaces:**
- Adds: event-backed branch of `wait_for_turn_response`
- Adds: `completed_turn_from_event_evidence`
- Preserves: existing stored-history polling for direct and Reviewer turns

- [ ] **Step 1: Add a failing event-only ephemeral completion test**

Have the fake emit `item/completed` for all canonical items followed by
`turn/completed`, while full reads remain forbidden. Assert the response is
accepted and `turn_event_evidence` contains the terminal turn and items.

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p consensus-daemon --test coordinator ephemeral_turns_complete_from_event_evidence -- --exact --nocapture
```

Expected: timeout or forbidden history-read failure.

- [ ] **Step 3: Implement event-backed waiting**

When the binding mode is ephemeral:

1. check already persisted completion evidence;
2. consume and persist matching live events;
3. use summary reads only for liveness;
4. merge the terminal event with completed item events;
5. return the canonical completed turn;
6. fail closed if the task disappears before completion evidence is durable.

Pass binding mode from `drive_model_action` into the waiter.

- [ ] **Step 4: Route completed-turn recovery through event evidence**

When a recorded Primary generation is ephemeral, load terminal turn evidence
from SQLite rather than task history. If no terminal evidence exists, return
`HISTORY_UNAVAILABLE` without resending or reforking.

- [ ] **Step 5: Run coordinator tests**

Run:

```bash
cargo test -p consensus-daemon --test coordinator
```

Expected: PASS.

### Task 5: Make the process-level fake enforce the production contract

**Files:**
- Modify: `tests/fake-app-server/src/main.rs`
- Modify: `tests/e2e/tests/acceptance.rs`

**Interfaces:**
- Process fake rejects ephemeral full-history reads and resumes
- Process fake emits complete event evidence for ephemeral turns

- [ ] **Step 1: Add a failing acceptance assertion**

In `preloaded_primary_uses_ephemeral_full_history_binding`, assert the event log
contains no mirror `thread/resume` and no mirror full-history read.

- [ ] **Step 2: Enforce exact fake-server behavior**

For an active mirror:

```rust
if params.get("includeTurns") == Some(&json!(true)) {
    return Err("ephemeral threads do not support includeTurns".to_owned());
}
```

Return an empty-turn summary for `includeTurns=false`, reject
`thread/resume`, let the already-loaded mirror start turns directly, and append
item/turn completion notifications.

- [ ] **Step 3: Run the three binding-path acceptance tests**

Run:

```bash
cargo test -p consensus-e2e --test acceptance not_loaded_primary_uses_direct_participant_binding -- --exact --nocapture
cargo test -p consensus-e2e --test acceptance preloaded_primary_uses_ephemeral_full_history_binding -- --exact --nocapture
cargo test -p consensus-e2e --test acceptance invalid_mirror_postconditions_fail_before_any_turn_or_git_write -- --exact --nocapture
```

Expected: PASS.

### Task 6: Verify, release, deploy, and recover the original Run

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `.agents/plugins/marketplace.json`
- Modify: `plugin/.codex-plugin/plugin.json`
- Modify: `README.md`
- Modify: `CHANGELOG.md`

**Interfaces:**
- Produces: `codex-consensus 0.2.8`
- Preserves: plugin/binary version lockstep

- [ ] **Step 1: Run formatting and all local tests**

Run:

```bash
cargo fmt --all -- --check
cargo test --workspace
```

Expected: PASS with no warnings or failures.

- [ ] **Step 2: Run the real no-turn capability probe**

Against basestream-cpu 0.145.0, reconfirm summary read succeeds while full
history, turns/list, and resume remain rejected. The probe must not call
`turn/start`.

- [ ] **Step 3: Bump all release metadata to 0.2.8**

Update package, lockfile, plugin marketplace/manifest, changelog, and README
compatibility notes. Build static x86_64 and aarch64 Linux release archives
through the existing GitHub Release workflow.

- [ ] **Step 4: Commit, push, tag, and publish**

Use intentional commits, push the tested source SHA, tag `v0.2.8`, and wait for
CI and Release workflows to succeed.

- [ ] **Step 5: Install matching binary and plugin on both servers**

Verify artifact SHA256, install atomically, refresh plugin `0.2.8`, restart the
App Server, and run:

```bash
codex-consensus --version
codex-consensus doctor
```

Expected: version `0.2.8` and `Ready`.

- [ ] **Step 6: Resume and inspect the existing basestream Run**

Before and after:

```bash
codex-consensus status 433797ff-11b2-49b9-9873-ff1179740da8 --json
```

Resume the same Run. Confirm it advances beyond
`REQUEST_PRIMARY_CONTRACT`, creates no substitute Run, and leaves both frozen
source refs unchanged. Continue monitoring to a terminal accepted or explicit
safe paused state.
