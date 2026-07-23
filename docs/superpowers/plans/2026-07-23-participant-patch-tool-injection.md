# Loaded Primary Participant Binding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every Primary consensus action automatically use a verified controlled-patch tool, falling back to an ephemeral full-history Primary mirror when an already-loaded source task cannot refresh its MCP runtime.

**Architecture:** Extend the Codex App Server adapter with non-idempotent ephemeral fork support, goal inspection, runtime-state parsing, and complete MCP-status pagination. Persist a Run-scoped Source-to-Effective Primary binding, route every Primary turn and patch proof through that binding, and retain the selected Source Primary as the immutable repository and contract identity. A missing mirror is recreated only between completed actions; a pending or uncertain turn is never reforked or resent.

**Tech Stack:** Rust 2024, Tokio, JSON-RPC, Codex App Server experimental v2, MCP stdio, SQLite, Git, process-level fake App Server tests.

## Global Constraints

- Support Codex versions greater than or equal to `0.144.1` with no upper version ceiling.
- Preserve the two exact user-selected Source Primary and Reviewer task IDs as immutable Run identities.
- Preserve their completed conversation histories. A mirror must fork the complete Source Primary history without truncation.
- Preserve both frozen source refs and source worktrees.
- Keep the integration result on a new local-only branch; never push, open a pull request, or merge into an existing branch.
- Keep `dangerFullAccess` and `approvalPolicy: "never"` for unattended coordinator turns.
- Expose no operator or launcher capability from the participant-only MCP server.
- Continue to bind every successful patch to one exact Run, request hash, source and effective Primary identities, integration round, clean target branch, and frozen source pair.
- Never carry or automatically continue a Source Primary goal into a mirror.
- Fail closed before a model turn when the participant route or patch capability cannot be initialized or verified.
- Do not require an operator to close a task, restart Codex, approve a command, register global MCP configuration, or manually relay messages.
- Preserve the exact release-bounded recovery for Run `f83cd777-9ed1-4369-8270-0fedd282f912`; do not recreate its branch or repeat its original merge.
- Leave the user-owned untracked `downloads/` directory untouched.

## Baseline

The implementation starts at commit `e820b9d` on
`diagnostic/app-server-error`. This baseline already contains:

- the hidden `participant-mcp-server` CLI mode;
- the participant-only `consensus_apply_patch` MCP surface;
- task-scoped MCP configuration on Primary integration resume;
- single-page `mcpServerStatus/list` preflight;
- request-bound controlled patch application;
- exact post-verification `CONTROLLED_PATCH_TOOL_UNAVAILABLE` recovery;
- package and plugin version `0.2.7`.

This plan replaces the integration-only resume assumption. It does not rewrite
the participant MCP backend or the existing corrective-Run safety proof.

## File Structure

- `crates/app-server-client/src/types.rs`: typed runtime state, resume/fork
  policies, thread goals, and MCP status pages.
- `crates/app-server-client/src/client.rs`: exact App Server requests,
  reconnect behavior, full pagination, and strict response parsing.
- `crates/app-server-client/src/lib.rs`: public adapter exports.
- `crates/daemon/src/participant_binding.rs`: binding types and pure
  capability, fork-history, and prompt-identity checks.
- `crates/daemon/src/store.rs`: SQLite binding generations, turn provenance,
  and patch provenance.
- `crates/daemon/src/coordinator.rs`: binding establishment, Primary routing,
  safe mirror recreation, and historical identity validation.
- `tests/fake-app-server/src/main.rs`: process-level loaded/not-loaded/forked
  App Server behavior.
- `tests/e2e/tests/acceptance.rs`: complete direct and mirror workflows.
- `schemas/app-server/supported-methods.json`: minimum App Server contract.
- Public documentation and plugin Skill files: operator-visible behavior and
  recovery instructions.

---

### Task 1: Typed fork, goal, runtime-state, and paginated MCP adapter

**Files:**

- Modify: `crates/app-server-client/src/types.rs`
- Modify: `crates/app-server-client/src/client.rs`
- Modify: `crates/app-server-client/src/lib.rs`
- Modify: `crates/app-server-client/tests/client.rs`
- Modify: `crates/app-server-client/tests/process.rs`

**Interfaces:**

- Produces:
  `ParticipantMcpConfig { participant_executable: PathBuf }`
- Produces:
  `ThreadResumePolicy::{Default, Participant(ParticipantMcpConfig)}`
- Produces:
  `ThreadForkPolicy::EphemeralParticipant(ParticipantMcpConfig)`
- Produces:
  `ThreadRuntimeStatus::{NotLoaded, Idle, Active, SystemError}`
- Produces:
  `ThreadSummary::runtime_status() -> Result<ThreadRuntimeStatus, String>`
- Produces:
  `AppServer::fork_thread(&self, source_thread_id: &str, policy: &ThreadForkPolicy) -> Result<ThreadDetail, AppServerError>`
- Produces:
  `AppServer::get_thread_goal(&self, thread_id: &str) -> Result<Option<Value>, AppServerError>`
- Changes:
  `AppServer::list_mcp_server_status` to consume every bounded page.

- [ ] **Step 1: Write failing request-shape and runtime-state tests**

Add these test cases to `crates/app-server-client/tests/client.rs`:

```rust
#[test]
fn thread_runtime_status_is_strict() {
    assert_eq!(
        summary_with_status(json!({"type": "notLoaded"}))
            .runtime_status()
            .unwrap(),
        ThreadRuntimeStatus::NotLoaded
    );
    assert_eq!(
        summary_with_status(json!({"type": "idle"}))
            .runtime_status()
            .unwrap(),
        ThreadRuntimeStatus::Idle
    );
    assert_eq!(
        summary_with_status(json!({"type": "active", "activeFlags": []}))
            .runtime_status()
            .unwrap(),
        ThreadRuntimeStatus::Active
    );
    assert_eq!(
        summary_with_status(json!({"type": "systemError"}))
            .runtime_status()
            .unwrap(),
        ThreadRuntimeStatus::SystemError
    );
    assert!(summary_with_status(json!({"type": "future"}))
        .runtime_status()
        .unwrap_err()
        .contains("unsupported thread status"));
}
```

Extend `typed_methods_emit_the_pinned_v2_request_shapes` so the fork request is
exactly:

```json
{
  "threadId": "t-1",
  "config": {
    "mcp_servers": {
      "worktreeMergeConsensusParticipant": {
        "command": "/opt/codex-consensus",
        "args": ["participant-mcp-server"],
        "required": true,
        "enabled_tools": ["consensus_apply_patch"],
        "startup_timeout_sec": 10,
        "tool_timeout_sec": 300,
        "tools": {
          "consensus_apply_patch": {"approval_mode": "approve"}
        }
      }
    }
  },
  "ephemeral": true,
  "excludeTurns": false
}
```

Assert that `lastTurnId`, `path`, `threadSource`, and
`deferGoalContinuation` are absent. Add a goal response test accepting only
`{"goal": null}` or this complete object and rejecting a missing or scalar
`goal`:

```json
{
  "goal": {
    "threadId": "t-1",
    "objective": "preserve both implementations",
    "status": "active",
    "tokenBudget": null,
    "tokensUsed": 0,
    "timeUsedSeconds": 0,
    "createdAt": 1,
    "updatedAt": 1
  }
}
```

- [ ] **Step 2: Run the request-shape tests and verify RED**

Run:

```bash
cargo test --locked -p app-server-client --test client thread_runtime_status_is_strict
cargo test --locked -p app-server-client --test client typed_methods_emit_the_pinned_v2_request_shapes
cargo test --locked -p app-server-client --test client thread_goal_response_is_strict
```

Expected: compilation failures because the new types and methods do not exist.

- [ ] **Step 3: Implement the typed policies and exact request builders**

Add to `types.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantMcpConfig {
    pub participant_executable: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadResumePolicy {
    Default,
    Participant(ParticipantMcpConfig),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadForkPolicy {
    EphemeralParticipant(ParticipantMcpConfig),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadRuntimeStatus {
    NotLoaded,
    Idle,
    Active,
    SystemError,
}
```

Make `resume_params` and the new `fork_params` share one
`participant_mcp_config(&ParticipantMcpConfig)` builder. Reject a
non-absolute executable before JSON-RPC. Implement:

```rust
fn fork_params(
    source_thread_id: &str,
    policy: &ThreadForkPolicy,
) -> Result<Value, AppServerError> {
    let ThreadForkPolicy::EphemeralParticipant(participant) = policy;
    Ok(json!({
        "threadId": source_thread_id,
        "config": participant_mcp_config(participant)?,
        "ephemeral": true,
        "excludeTurns": false,
    }))
}
```

Parse `thread/fork` through the existing strict `parse_thread_response`.
Parse `thread/goal/get` only when the top-level `goal` key exists and is null
or an object.

- [ ] **Step 4: Write failing complete-pagination tests**

Add one test that returns two pages:

```json
{"data": [{"name": "unrelated", "tools": {}}], "nextCursor": "page-2"}
{"data": [{"name": "worktreeMergeConsensusParticipant", "tools": {
  "consensus_apply_patch": {"inputSchema": {"type": "object"}}
}}], "nextCursor": null}
```

Assert requests use:

```json
{"threadId":"t-1","detail":"toolsAndAuthOnly","limit":100,"cursor":null}
{"threadId":"t-1","detail":"toolsAndAuthOnly","limit":100,"cursor":"page-2"}
```

Add separate assertions rejecting:

- a missing `nextCursor` field;
- a non-string non-null cursor;
- a repeated cursor;
- a duplicate server name across pages;
- more than 16 pages;
- more than 1,000 total servers;
- a tool definition whose `inputSchema` is absent or not an object.

- [ ] **Step 5: Run the pagination test and verify RED**

Run:

```bash
cargo test --locked -p app-server-client --test client mcp_status_consumes_all_pages
cargo test --locked -p app-server-client --test client mcp_status_rejects_incomplete_or_unbounded_pagination
```

Expected: assertion failures because the current method sends one unpaginated
request and ignores `nextCursor`.

- [ ] **Step 6: Implement bounded pagination and strict tool parsing**

Use these bounds in `client.rs`:

```rust
const MCP_STATUS_PAGE_LIMIT: u32 = 100;
const MCP_STATUS_MAX_PAGES: usize = 16;
const MCP_STATUS_MAX_SERVERS: usize = 1_000;
```

Change the single-page parser to return:

```rust
struct McpServerStatusPage {
    data: Vec<McpServerStatus>,
    next_cursor: Option<String>,
}
```

Follow each opaque cursor, track every seen cursor and server name in
`BTreeSet`, and reject a cycle, duplicate, or bound exhaustion. Require every
tool definition and its `inputSchema` to be objects.

- [ ] **Step 7: Preserve correct reconnect semantics**

In `ReconnectingCodexAppServer`:

- retry `thread/goal/get` and `mcpServerStatus/list` after a reconnect because
  they are idempotent;
- retry `thread/resume` with the same policy because it is idempotent;
- for `thread/fork`, reconnect only when the transport is already known closed
  before sending, then issue one request;
- never repeat `thread/fork` after an I/O, protocol, or closed error returned
  from the request itself.

Add `reconnecting_client_does_not_repeat_an_uncertain_thread_fork` alongside
the existing uncertain `turn/start` test. Its fake proxy must log exactly one
`thread/fork` request and one proxy process.

- [ ] **Step 8: Verify Task 1 GREEN**

Run:

```bash
cargo test --locked -p app-server-client
```

Expected: all app-server-client unit, process, compatibility, and JSON-RPC
tests pass.

- [ ] **Step 9: Commit Task 1**

```bash
git add crates/app-server-client
git commit -m "feat: support ephemeral participant task forks"
```

### Task 2: Persist Primary binding generations and patch provenance

**Files:**

- Create: `crates/daemon/src/participant_binding.rs`
- Modify: `crates/daemon/src/lib.rs`
- Modify: `crates/daemon/src/store.rs`
- Modify: `crates/daemon/tests/store.rs`

**Interfaces:**

- Produces:
  `PrimaryBindingMode::{Direct, EphemeralFork}`
- Produces:
  `PrimaryParticipantBinding`
- Produces:
  `SqliteRunStore::activate_primary_binding(&self, run_id: &str, source_primary_thread_id: &str, effective_primary_thread_id: &str, mode: PrimaryBindingMode, participant_server: &str) -> Result<PrimaryParticipantBinding, StoreError>`
- Produces:
  `SqliteRunStore::active_primary_binding(run_id)`
- Produces:
  `SqliteRunStore::primary_binding(run_id, generation)`
- Adds:
  `PendingSend.participant_binding_generation`
- Adds:
  `AcceptedTurn.participant_binding_generation`
- Produces:
  `SuccessfulPatchRecord` with source/effective task and generation fields.

- [ ] **Step 1: Write failing binding persistence tests**

Add `primary_binding_generations_are_atomic_and_auditable` to
`crates/daemon/tests/store.rs`. It must:

1. insert a normal Run;
2. activate a direct binding for `primary-thread`;
3. assert generation `1`;
4. activate the same binding again and assert idempotent generation `1`;
5. activate an ephemeral binding for `primary-mirror-1`;
6. assert generation `2` is active and generation `1` remains queryable;
7. assert source identity remains `primary-thread`;
8. assert activation is rejected while a pending turn exists.

Use these exact public types:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrimaryBindingMode {
    Direct,
    EphemeralFork,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimaryParticipantBinding {
    pub run_id: String,
    pub source_primary_thread_id: String,
    pub effective_primary_thread_id: String,
    pub mode: PrimaryBindingMode,
    pub generation: u32,
    pub participant_server: String,
    pub created_at: i64,
    pub verified_at: i64,
}
```

- [ ] **Step 2: Run the binding store test and verify RED**

Run:

```bash
cargo test --locked -p consensus-daemon --test store primary_binding_generations_are_atomic_and_auditable
```

Expected: compilation failure because the binding model and store API do not
exist.

- [ ] **Step 3: Add the binding schema and transactional API**

Create this table and index in `migrate`:

```sql
CREATE TABLE IF NOT EXISTS primary_participant_bindings (
    run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    source_primary_thread_id TEXT NOT NULL,
    effective_primary_thread_id TEXT NOT NULL,
    mode TEXT NOT NULL CHECK(mode IN ('DIRECT', 'EPHEMERAL_FORK')),
    participant_server TEXT NOT NULL,
    active INTEGER NOT NULL CHECK(active IN (0, 1)),
    created_at INTEGER NOT NULL,
    verified_at INTEGER NOT NULL,
    PRIMARY KEY(run_id, generation)
);
CREATE UNIQUE INDEX IF NOT EXISTS one_active_primary_binding
    ON primary_participant_bindings(run_id) WHERE active = 1;
```

`activate_primary_binding` must run one transaction that:

- loads `source_facts.primary_thread_id` and reviewer ID;
- requires the supplied source ID to match frozen facts;
- allows Source = Effective only for `DIRECT`;
- requires a mirror to differ from both selected source IDs;
- requires no `PENDING` or `SENT` turn for the Run;
- returns the existing active row unchanged when every identity field matches;
- otherwise deactivates the current row and inserts `MAX(generation) + 1`;
- commits before returning the new binding.

- [ ] **Step 4: Write failing turn- and patch-provenance tests**

Extend the existing pending/accepted turn test to call:

```rust
store.record_pending_send(
    RUN_ID,
    "PRIMARY",
    "INTEGRATE",
    2,
    REQUEST_HASH,
    Some(binding.generation),
)?;
```

Assert the same generation survives `record_turn_start_intent`,
`record_turn_started`, `accept_response_and_advance`, archive/reset, and
`latest_accepted_turn`.

Add a patch test expecting:

```rust
SuccessfulPatchRecord {
    patch_hash: PATCH_HASH.to_owned(),
    source_primary_thread_id: Some("primary-thread".to_owned()),
    effective_primary_thread_id: Some("primary-mirror-1".to_owned()),
    participant_binding_generation: Some(2),
}
```

- [ ] **Step 5: Implement backward-compatible schema migration**

Add nullable columns with `PRAGMA table_info` guards:

```sql
ALTER TABLE turns ADD COLUMN participant_binding_generation INTEGER;
ALTER TABLE patch_applications ADD COLUMN source_primary_thread_id TEXT;
ALTER TABLE patch_applications ADD COLUMN effective_primary_thread_id TEXT;
ALTER TABLE patch_applications ADD COLUMN participant_binding_generation INTEGER;
```

Keep legacy rows readable with null provenance. Define:

```rust
pub const LEGACY_PARTICIPANT_CAPABILITY_GENERATION: &str = "participant-mcp-v1";
pub const PARTICIPANT_CAPABILITY_GENERATION: &str = "participant-mcp-v2";
```

The exact pre-`0.2.7` corrective blocker continues to accept only
`participant-mcp-v1`; all newly recorded Primary turns use
`participant-mcp-v2` plus a non-null binding generation. Reviewer turns keep a
null binding generation.

- [ ] **Step 6: Verify Task 2 GREEN**

Run:

```bash
cargo test --locked -p consensus-daemon --test store
```

Expected: all store tests pass, including opening and migrating a database
created without any binding or provenance columns.

- [ ] **Step 7: Commit Task 2**

```bash
git add crates/daemon/src/participant_binding.rs crates/daemon/src/lib.rs crates/daemon/src/store.rs crates/daemon/tests/store.rs
git commit -m "feat: persist primary participant bindings"
```

### Task 3: Establish the binding and route every Primary action

**Files:**

- Modify: `crates/daemon/src/participant_binding.rs`
- Modify: `crates/daemon/src/coordinator.rs`
- Modify: `crates/daemon/tests/coordinator.rs`

**Interfaces:**

- Consumes all Task 1 and Task 2 interfaces.
- Produces:
  `Coordinator::ensure_primary_participant_binding`
- Produces:
  `Coordinator::create_ephemeral_primary_binding`
- Produces:
  `Coordinator::prepare_action_thread`
- Produces:
  `verify_participant_patch_capability`
- Produces:
  `verify_full_history_fork`
- Produces:
  `append_primary_execution_identity`.

- [ ] **Step 1: Extend the in-process FakeAppServer and write direct-binding RED tests**

Extend `FakeAppServer` with:

```rust
primary_runtime_status: Mutex<ThreadRuntimeStatus>,
participant_threads: Mutex<BTreeSet<String>>,
forks: Mutex<Vec<(String, String, ThreadForkPolicy)>>,
goals: Mutex<BTreeMap<String, Option<Value>>>,
```

Implement the new AppServer trait methods. A `notLoaded` Source Primary must
apply `ThreadResumePolicy::Participant`; an idle preloaded Source must keep its
existing inventory when resumed, matching the real App Server behavior.

Add `not_loaded_primary_binds_directly_before_the_first_primary_turn`. Assert:

- `thread/resume:primary` carries `Participant`;
- complete MCP preflight occurs before the first Primary contract
  `turn/start`;
- active binding is `DIRECT`, generation `1`, effective ID `primary`;
- every later Primary turn uses `primary`;
- every Reviewer turn uses `reviewer`.

Add
`loaded_primary_with_existing_participant_binds_directly_without_fork`.
Initialize the Source Primary as `idle` and include it in
`participant_threads`. Assert generation `1` is `DIRECT`, no `thread/fork`
occurs, the exact inventory is preflighted before every Primary turn, and all
Primary turns retain the Source Primary ID.

- [ ] **Step 2: Run the direct-binding test and verify RED**

Run:

```bash
cargo test --locked -p consensus-daemon --test coordinator not_loaded_primary_binds_directly_before_the_first_primary_turn
```

Expected: the current coordinator uses default resume for the Primary contract
and does not create a binding.

- [ ] **Step 3: Implement direct binding and per-Primary preflight**

Implement `ensure_primary_participant_binding` with this state table:

| Source runtime | Exact participant MCP | Result |
| --- | --- | --- |
| `notLoaded` | established by configured resume | activate `DIRECT` |
| `notLoaded` | missing after configured resume | `PATCH_TOOL_UNAVAILABLE` |
| `idle` | present | activate or retain `DIRECT` |
| `idle` | absent | call mirror creation |
| `active` | any | use existing bounded busy handling; never fork |
| `systemError` or unknown | any | fail closed before `turn/start` |

Before every Primary action:

1. resolve the active Effective Primary;
2. require it idle;
3. resume it with `Default` when already loaded, or `Participant` when it is
   the not-loaded direct Source Primary;
4. list the complete MCP inventory;
5. call `verify_participant_patch_capability`;
6. only then create or recover the pending send.

Reviewer routing remains exact-ID default resume without participant preflight.

- [ ] **Step 4: Write ephemeral-mirror RED tests**

Add tests for an idle loaded Source Primary whose inventory lacks the
participant server. Require one exact fork:

```rust
ThreadForkPolicy::EphemeralParticipant(ParticipantMcpConfig {
    participant_executable: participant_mcp_executable(),
})
```

The fake fork ID is `primary-consensus-mirror-1`. Assert:

- fork source is `primary`;
- the fork's complete pre-existing turn ID sequence equals the Source Primary
  sequence;
- `thread/goal/get` returns null;
- the mirror passes MCP preflight;
- binding mode is `EPHEMERAL_FORK`, generation `1`;
- all four Primary actions use `primary-consensus-mirror-1`;
- all three Reviewer actions use `reviewer`;
- no coordinator turn is appended to the Source Primary.

Add negative cases for:

- fork ID equal to Source Primary;
- fork ID equal to Reviewer;
- missing or reordered source history;
- a non-null mirror goal;
- active mirror status;
- missing or expanded participant inventory.

Every negative case must assert no `turn/start` and no Git result event.

- [ ] **Step 5: Run mirror tests and verify RED**

Run:

```bash
cargo test --locked -p consensus-daemon --test coordinator loaded_primary_uses_one_full_history_ephemeral_mirror
cargo test --locked -p consensus-daemon --test coordinator invalid_primary_mirror_fails_before_any_model_turn
```

Expected: compilation or assertion failure because `thread/fork`, the Source
goal precondition, and Effective Primary routing are not implemented.

- [ ] **Step 6: Implement mirror creation and Source goal precondition**

`create_ephemeral_primary_binding` must:

1. re-read and verify the exact Source Primary;
2. require Source runtime `idle`;
3. call `thread/goal/get` on the Source Primary and require `None`;
4. call non-retrying `thread/fork` once;
5. reject an empty, Source, or Reviewer ID;
6. compare the source and fork turn ID sequences exactly;
7. require mirror runtime `idle`;
8. require the exact participant server and patch tool;
9. atomically activate the new binding;
10. return only after the store can reload the same binding.

Do not send `deferGoalContinuation`. Do not call `thread/goal/get` or
`thread/goal/clear` on an ephemeral mirror; supported Codex runtimes may reject
goal operations for ephemeral tasks. A Source goal is a failed precondition,
not state the coordinator may mutate.

- [ ] **Step 7: Add Source/Effective identity to every Primary prompt**

Append this block after the normal action payload and before delivery identity:

````text
Primary participant execution identity:
```json
{
  "source_primary_thread_id": "primary",
  "effective_primary_thread_id": "primary-consensus-mirror-1",
  "binding_mode": "EPHEMERAL_FORK",
  "binding_generation": 1
}
```
````

The surrounding instruction must say that the Effective Primary represents the
Source Primary, preserves its full implementation contract, and may write only
to the coordinator-authorized integration worktree. Direct mode includes the
same block with equal source/effective IDs. Reviewer prompts contain no Primary
execution identity block.

- [ ] **Step 8: Verify Task 3 GREEN**

Run:

```bash
cargo test --locked -p consensus-daemon --test coordinator
```

Expected: every coordinator test passes. Update old expectations so participant
preflight precedes every Primary turn rather than only integration.

- [ ] **Step 9: Commit Task 3**

```bash
git add crates/daemon/src/participant_binding.rs crates/daemon/src/coordinator.rs crates/daemon/tests/coordinator.rs
git commit -m "feat: route primary actions through verified bindings"
```

### Task 4: Bind recovery, controlled patches, and mirror recreation to provenance

**Files:**

- Modify: `crates/core/src/state.rs`
- Modify: `crates/core/tests/state_machine.rs`
- Modify: `crates/daemon/src/coordinator.rs`
- Modify: `crates/daemon/src/store.rs`
- Modify: `crates/daemon/tests/coordinator.rs`
- Modify: `crates/daemon/tests/store.rs`

**Interfaces:**

- Adds optional binding fields to `RunDiagnostic`.
- Produces:
  `Coordinator::validate_recorded_role_thread`
- Changes controlled patch authorization to require the active binding.
- Preserves legacy null-generation Source Primary turns only in explicit
  release-bounded recovery validators.

- [ ] **Step 1: Write failing diagnostic and historical-identity tests**

Extend `RunDiagnostic` with backward-compatible defaults:

```rust
#[serde(default)]
pub source_thread_id: Option<String>,
#[serde(default)]
pub effective_thread_id: Option<String>,
#[serde(default)]
pub participant_binding_generation: Option<u32>,
#[serde(default)]
pub participant_binding_mode: Option<String>,
#[serde(default)]
pub participant_server: Option<String>,
```

Add state serialization tests proving old diagnostics without these fields
still deserialize, while a mirror capability failure records:

```json
{
  "source_thread_id": "primary",
  "effective_thread_id": "primary-consensus-mirror-1",
  "participant_binding_generation": 1,
  "participant_binding_mode": "EPHEMERAL_FORK",
  "participant_server": "worktreeMergeConsensusParticipant"
}
```

Add coordinator fixtures containing:

- a new Primary accepted turn on a mirror with binding generation `1`;
- a legacy accepted turn on Source Primary with null binding generation;
- a forged mirror ID with no matching stored generation;
- a valid older mirror generation after a later generation became active.

- [ ] **Step 2: Run identity tests and verify RED**

Run:

```bash
cargo test --locked -p consensus-core --test state_machine diagnostic_binding_identity_is_backward_compatible
cargo test --locked -p consensus-daemon --test coordinator recorded_primary_turns_require_their_exact_binding_generation
```

Expected: compilation failures for the new fields and assertions where current
code hard-codes `state.facts.primary_thread_id`.

- [ ] **Step 3: Implement one historical identity validator**

Add:

```rust
fn validate_recorded_role_thread(
    &self,
    state: &RunState,
    role: Role,
    thread_id: &str,
    participant_binding_generation: Option<u32>,
) -> Result<(), CoordinatorError>
```

Rules:

- Reviewer requires the frozen Reviewer ID and a null binding generation.
- Primary with `Some(generation)` loads that exact historical binding, requires
  its source ID to equal frozen Source Primary, and requires `thread_id` to
  equal that binding's Effective Primary.
- Primary with `None` is accepted only when `thread_id` equals frozen Source
  Primary and the enclosing validator is an existing release-bounded legacy
  path.
- Current non-legacy Primary sends always require the active binding and
  non-null generation.

Replace direct Source-ID comparisons for `PendingSend` and `AcceptedTurn` in
terminal retry, approval retry, invalid-response retry, verification recovery,
corrective patch-tool recovery, and integration evidence inspection.

- [ ] **Step 4: Write failing controlled-patch provenance tests**

Start an integration request on `primary-consensus-mirror-1`. Assert
`apply_patch` rejects each mutation independently:

- pending thread changed to Source Primary;
- pending binding generation changed;
- active binding switched after the pending send;
- binding source ID changed;
- missing binding generation;
- second successful patch.

Assert the successful case writes `SuccessfulPatchRecord` with Source,
Effective, and generation values and still leaves both frozen source refs
unchanged.

- [ ] **Step 5: Implement controlled-patch binding checks**

In `Coordinator::apply_patch`, require:

```rust
pending.thread_id.as_deref() == Some(binding.effective_primary_thread_id.as_str())
    && pending.participant_binding_generation == Some(binding.generation)
    && binding.source_primary_thread_id == state.facts.primary_thread_id
```

Pass the binding into `record_successful_patch`. Keep all existing request
hash, round, phase, branch, ancestry, clean-worktree, and single-success checks.

- [ ] **Step 6: Write safe mirror-recreation RED tests**

Complete one mirror Primary contract and its following Reviewer contract so no
pending send remains. Remove the ephemeral mirror from the fake App Server,
then request the next Primary plan. Assert:

- Source Primary is revalidated and still idle;
- one new mirror is created;
- generation advances from `1` to `2`;
- the deterministic action payload and request hash are unchanged by
  generation;
- the plan turn is sent only to generation `2`;
- no accepted generation `1` history is rewritten.

Create a second fixture with a `PENDING` or `SENT` turn on the missing mirror.
Assert the Run pauses/blocks with `COMMUNICATION_FAILURE`, creates no generation
`2`, sends no replacement turn, and performs no Git write.

- [ ] **Step 7: Implement safe-boundary recreation**

When an active ephemeral binding cannot be read after bounded retries:

1. query `pending_send`;
2. if any pending or sent record exists, return the original communication
   failure;
3. otherwise verify Source Primary and create a new mirror generation;
4. rebuild the next prompt entirely from canonical Run state.

Do not recreate after `record_pending_send`, after a non-idempotent
`turn/start`, or while turn outcome is uncertain.

- [ ] **Step 8: Prove the existing blocked Run remains recoverable**

Update the corrective patch-tool fixtures so the old blocked turn remains:

```text
thread_id = Source Primary
capability_generation = participant-mcp-v1
participant_binding_generation = null
```

Explicit resume must archive only that legacy empty blocker, establish the
current binding, and send the correction through its Effective Primary. The
same Run, round, branch, old integration SHA, verification clone, failed
evidence, frozen refs, and source worktrees remain unchanged until the one
authorized corrective patch advances the integration SHA.

- [ ] **Step 9: Run a hard-coded identity scan and focused tests**

Run:

```bash
rg -n "pending\\.thread_id.*primary_thread_id|accepted\\.thread_id.*primary_thread_id|thread_id != state\\.facts\\.primary_thread_id" crates/daemon/src/coordinator.rs
cargo test --locked -p consensus-core --test state_machine
cargo test --locked -p consensus-daemon --test store
cargo test --locked -p consensus-daemon --test coordinator
```

Expected: `rg` returns no unreviewed direct comparison; every remaining
`primary_thread_id` comparison is source-fact validation or an explicitly
named legacy validator. All focused tests pass.

- [ ] **Step 10: Commit Task 4**

```bash
git add crates/core crates/daemon
git commit -m "fix: bind primary recovery and patches to execution provenance"
```

### Task 5: Process fake and end-to-end loaded-task coverage

**Files:**

- Modify: `tests/fake-app-server/src/main.rs`
- Modify: `tests/e2e/tests/acceptance.rs`

**Interfaces:**

- Consumes the final AppServer and coordinator interfaces.
- Adds process scenarios:
  `primary_not_loaded`, `primary_loaded_without_participant`,
  `primary_loaded_with_participant`, `source_goal_present`,
  `mirror_history_mismatch`, and `participant_status_paginated`.

- [ ] **Step 1: Write failing process-level acceptance tests**

Add these complete test bodies:

```rust
#[test]
fn not_loaded_primary_uses_direct_participant_binding() {
    let fixture = AcceptanceFixture::new("primary_not_loaded", false);
    let (run_id, _daemon) = fixture.start();
    let accepted = fixture.wait_for_terminal(&run_id);
    let events = fixture.events();

    assert_eq!(accepted["status"], "ACCEPTED", "{events}");
    assert_eq!(accepted["accepted_result"]["tests"][0]["exit_code"], 0);
    assert_eq!(accepted["accepted_result"]["source_refs_unchanged"], true);
    assert_eq!(accepted["accepted_result"]["publication"]["local_only"], true);
    assert_eq!(accepted["accepted_result"]["publication"]["pushed"], false);
    assert_eq!(
        accepted["accepted_result"]["publication"]["pull_request_created"],
        false
    );
    assert_eq!(
        accepted["accepted_result"]["publication"]["merged_into_existing_branch"],
        false
    );
    assert!(events.lines().any(|line| {
        line == "primary-binding primary-thread primary-thread DIRECT 1"
    }));
    assert_eq!(
        events
            .lines()
            .filter(|line| line.starts_with("method thread/fork "))
            .count(),
        0
    );
    assert!(events.lines().filter(|line| line.starts_with("turn primary-thread ")).count() >= 4);
    fixture.assert_source_refs_unchanged();
}

#[test]
fn preloaded_primary_uses_ephemeral_full_history_binding() {
    let fixture = AcceptanceFixture::new("primary_loaded_without_participant", false);
    let (run_id, _daemon) = fixture.start();
    let accepted = fixture.wait_for_terminal(&run_id);
    let events = fixture.events();
    let mirror = "primary-thread-consensus-mirror-1";
    let binding_event =
        format!("primary-binding primary-thread {mirror} EPHEMERAL_FORK 1");
    let goal_event = "method thread/goal/get primary-thread null";

    assert_eq!(accepted["status"], "ACCEPTED", "{events}");
    assert_eq!(accepted["accepted_result"]["tests"][0]["exit_code"], 0);
    assert_eq!(accepted["accepted_result"]["source_refs_unchanged"], true);
    assert_eq!(accepted["accepted_result"]["publication"]["local_only"], true);
    assert_eq!(accepted["accepted_result"]["publication"]["pushed"], false);
    assert_eq!(
        accepted["accepted_result"]["publication"]["pull_request_created"],
        false
    );
    assert_eq!(
        accepted["accepted_result"]["publication"]["merged_into_existing_branch"],
        false
    );
    assert!(events
        .lines()
        .any(|line| line == binding_event.as_str()));
    assert_eq!(
        events
            .lines()
            .filter(|line| *line == "method thread/fork primary-thread")
            .count(),
        1
    );
    assert_eq!(
        events
            .lines()
            .filter(|line| *line == goal_event.as_str())
            .count(),
        1
    );
    assert!(events.lines().filter(|line| line.starts_with(&format!("turn {mirror} "))).count() >= 4);
    assert!(!events.lines().any(|line| line.starts_with("turn primary-thread ")));
    assert!(events.lines().filter(|line| line.starts_with("turn reviewer-thread ")).count() >= 3);
    fixture.assert_source_refs_unchanged();
}
```

For both tests, require final status `ACCEPTED`, all frozen tests successful,
both source refs unchanged, no remote publication, and the expected binding
mode in the fake event log.

The loaded case must additionally require:

- a configured `thread/resume` on the loaded Source does not alter its MCP
  inventory;
- exactly one `thread/fork`;
- exactly one null `thread/goal/get` on the Source Primary before the fork;
- no goal operation on the ephemeral mirror;
- all Primary `turn/start` events use the mirror;
- all Reviewer events use the selected Reviewer;
- the Source Primary receives no coordinator turn.

- [ ] **Step 2: Run process tests and verify RED**

Run:

```bash
cargo test --locked -p consensus-e2e --test acceptance not_loaded_primary_uses_direct_participant_binding -- --test-threads=1
cargo test --locked -p consensus-e2e --test acceptance preloaded_primary_uses_ephemeral_full_history_binding -- --test-threads=1
```

Expected: fake App Server rejects unsupported `thread/fork` or
`thread/goal/get`, and the loaded case blocks before integration.

- [ ] **Step 3: Implement realistic process fake semantics**

Persist task runtime metadata under the fake state directory. Implement:

- Source status `notLoaded` or `idle`;
- participant config taking effect only when loading a `notLoaded` task;
- loaded `thread/resume.config` returning success without changing inventory;
- `thread/fork` copying every Source turn, assigning
  `primary-thread-consensus-mirror-1`, applying participant config, and marking
  it ephemeral;
- `thread/goal/get` on the Source Primary returning the scenario's null or
  object goal before any fork;
- paginated `mcpServerStatus/list` with required `nextCursor`;
- turn policy accepting the Effective Primary ID while retaining the frozen
  Source Primary worktree as cwd.

Every fake request appends one deterministic method line so ordering assertions
do not depend on timing.

- [ ] **Step 4: Add fail-closed end-to-end scenarios**

For `source_goal_present` and `mirror_history_mismatch`, assert:

- reason is `HISTORY_UNAVAILABLE`;
- no `turn/start` occurs;
- the integration branch remains absent;
- both source refs and worktrees remain unchanged.

For `participant_status_paginated`, place an unrelated server on page one and
the participant server on page two; require the full workflow to reach
`ACCEPTED`.

- [ ] **Step 5: Verify Task 5 GREEN**

Run:

```bash
cargo test --locked -p consensus-e2e --test acceptance -- --test-threads=1
```

Expected: all process-level acceptance tests pass.

- [ ] **Step 6: Commit Task 5**

```bash
git add tests/fake-app-server/src/main.rs tests/e2e/tests/acceptance.rs
git commit -m "test: cover loaded primary mirror fallback"
```

### Task 6: Compatibility fixture, Skill, and release documentation

**Files:**

- Modify: `schemas/app-server/supported-methods.json`
- Modify: `crates/app-server-client/src/compat.rs`
- Modify: `crates/app-server-client/tests/compat.rs`
- Modify: `README.md`
- Modify: `README.zh-CN.md`
- Modify: `docs/compatibility.md`
- Modify: `docs/protocol-v1.md`
- Modify: `docs/protocol-v2.md`
- Modify: `docs/real-codex-smoke-test.md`
- Modify: `plugin/skills/worktree-merge-consensus/SKILL.md`
- Modify: `plugin/skills/worktree-merge-consensus/references/protocol.md`
- Modify: `tests/docs.sh`

**Interfaces:**

- Adds required App Server methods `thread/fork` and `thread/goal/get`.
- Extends the request-shape fixture for fork and MCP pagination.
- Keeps package and plugin version exactly `0.2.7`.

- [ ] **Step 1: Write failing compatibility assertions**

Require `REQUIRED_METHODS` to contain, in exact order:

```rust
[
    "initialize",
    "thread/list",
    "thread/read",
    "thread/resume",
    "thread/fork",
    "thread/goal/get",
    "turn/start",
    "turn/interrupt",
    "command/exec",
    "config/read",
    "config/batchWrite",
    "mcpServerStatus/list",
]
```

Require these fixture shapes:

```json
"thread/resume": {
  "default": ["threadId"],
  "participant": ["threadId", "config"]
},
"thread/fork": [
  "threadId", "config", "ephemeral", "excludeTurns"
],
"thread/goal/get": ["threadId"],
"mcpServerStatus/list": [
  "threadId", "detail", "limit", "cursor"
]
```

Assert `deferGoalContinuation` is absent, minimum version is `0.144.1`, and no
maximum version field exists.

- [ ] **Step 2: Run compatibility tests and verify RED**

Run:

```bash
cargo test --locked -p app-server-client --test compat
```

Expected: fixture and compiled required-method assertions fail.

- [ ] **Step 3: Update compatibility code and fixture**

Add the two required methods and exact request shapes. Keep method-not-found
mapping to `INCOMPATIBLE_CODEX`. Document that `thread/fork` is non-idempotent
and is never automatically repeated after an uncertain response.

- [ ] **Step 4: Replace the obsolete integration-only documentation**

Every public description of `0.2.7` must state:

- participant binding occurs before the first Primary action;
- a not-loaded Source Primary binds directly;
- a preloaded Source without the tool uses an ephemeral full-history mirror;
- the mirror is not a third reviewer or source identity;
- no Source goal is carried;
- every Primary turn is preflighted;
- Reviewer routing is unchanged;
- source refs, source worktrees, and selected source task IDs stay frozen;
- mirror loss is recreated only between completed actions;
- pending or uncertain turns are not reforked or resent;
- existing exact corrective blocker recovery still requires explicit resume.

The plugin Skill must call only operator `consensus_*` tools. It must not ask
the invoking task to find, call, or install a participant-side
`consensus_apply_patch`; that capability is coordinator-owned.

- [ ] **Step 5: Add documentation contract checks**

In `tests/docs.sh`, require both READMEs, `docs/compatibility.md`,
`docs/protocol-v2.md`, and the plugin protocol reference to contain:

```text
thread/fork
ephemeral
Source Primary
Effective Primary
>=0.144.1
```

Reject the obsolete phrases:

```text
only when resuming a Primary integration task
default, ordinary, and non-integration variant remains
```

- [ ] **Step 6: Verify Task 6 GREEN**

Run:

```bash
cargo test --locked -p app-server-client --test compat
bash tests/docs.sh
bash tests/release-gate.sh
bash tests/release.sh v0.2.7
```

Expected: all commands exit zero and package/plugin versions remain `0.2.7`.

- [ ] **Step 7: Commit Task 6**

```bash
git add schemas crates/app-server-client/src/compat.rs crates/app-server-client/tests/compat.rs README.md README.zh-CN.md docs plugin tests/docs.sh
git commit -m "docs: explain automatic primary mirror binding"
```

### Task 7: Full verification, real App Server qualification, and deployment

**Files:**

- Modify: `docs/real-codex-smoke-test.md` only after collecting reproducible
  redacted evidence.
- Modify implementation files only when a failing check identifies a concrete
  defect; add the reproducing test in the same commit as its fix.

**Interfaces:**

- Consumes every prior task deliverable.
- Produces a verified `0.2.7` candidate and a deployment record for
  Basestream and Huoshan.

- [ ] **Step 1: Run formatting, lint, MSRV, and complete tests**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo +1.85.0 check --locked --workspace --all-targets
cargo test --locked --workspace --all-targets -- --test-threads=1
cargo test --locked --workspace --all-targets --all-features -- --test-threads=1
cargo test --locked --workspace --doc --all-features
bash tests/docs.sh
bash tests/release-gate.sh
bash tests/release.sh v0.2.7
```

Expected: every command exits zero.

- [ ] **Step 2: Review final source scope**

Run:

```bash
git diff --check v0.2.6..HEAD
git status --short
git log --oneline --decorate v0.2.6..HEAD
```

Expected: only intentional source, test, specification, and plan changes are
tracked. `downloads/` remains untracked and unchanged.

- [ ] **Step 3: Build and transfer the exact candidate to Basestream**

Create a source archive from the committed candidate, excluding `.git`,
`target`, and `downloads`, transfer it to
`basestream-cpu:/tmp/worktree-merge-consensus-v0.2.7-candidate`, and run:

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets -- --test-threads=1
cargo build --locked --release -p codex-consensus
./target/release/codex-consensus --version
```

Expected on Basestream:

```text
codex-consensus 0.2.7
```

Record the exact local commit SHA and require the transferred source archive
hash to match before installation.

- [ ] **Step 4: Run non-mutating real App Server qualification on Basestream**

Using Codex `0.145.0` or the installed supported version, run three isolated
adapter probes without `turn/start`:

1. Resume a disposable `notLoaded` task with participant config and require the
   same task ID plus exact `consensus_apply_patch`.
2. Inspect the preloaded Source Primary
   `019f7ec2-0ed8-7d90-80b4-5b87ad54bee0`, confirm configured resume does not
   refresh its inventory, then create an ephemeral full-history fork.
3. Require the fork ID to differ from
   `019f7ec2-0ed8-7d90-80b4-5b87ad54bee0` and
   `019f78f6-fdd5-7c43-b3a2-a3be7da9a398`, require null goal, identical source
   history IDs, idle status, and exact participant capability.

Expected: all preflights succeed, no model turn starts, and
`/gpfs/users/i-zhangguoqiang/workspace/gh_testtest` has no Git change.

- [ ] **Step 5: Install the candidate on Basestream and Huoshan**

On each host:

1. back up the current `codex-consensus` binary with its version and SHA in the
   filename;
2. install the exact release candidate at the path returned by
   `command -v codex-consensus`;
3. install the matching `0.2.7` plugin source;
4. restart only the consensus daemon;
5. run:

```bash
codex-consensus --version
codex-consensus doctor
codex-consensus threads list
```

Expected on both hosts: version `0.2.7`, `doctor` reports Ready, and existing
tasks are listed. Do not restart unrelated Codex tasks.

- [ ] **Step 6: Run a full disposable repository workflow**

On Basestream, use the two committed worktrees under:

```text
/gpfs/users/i-zhangguoqiang/workspace/gh_testtest
```

Start a fresh Run with the preloaded Primary and selected Reviewer, a new
`consensus/` branch name, and the repository's frozen test command. Require:

- automatic mirror binding;
- at least one complete Primary proposal and Reviewer verdict cycle;
- one controlled integration patch;
- coordinator-owned verification;
- final Reviewer approval;
- accepted local-only integration branch;
- both original source refs and worktrees unchanged;
- no push, pull request, or merge into an existing branch.

- [ ] **Step 7: Resume the exact blocked production Run once**

Before resume, record status, branch, SHA, source refs, source worktree
cleanliness, and failed verification evidence for:

```text
f83cd777-9ed1-4369-8270-0fedd282f912
```

Call explicit resume once. Require:

- the same Run ID and correction round;
- the same existing integration branch as the starting point;
- automatic Effective Primary binding;
- no repetition of the original merge;
- at most one request-bound corrective patch;
- a new integration SHA;
- complete frozen verification rerun;
- final Reviewer result approval;
- both frozen source refs and source worktrees unchanged.

If any precondition differs from the recorded exact recovery shape, stop
without mutation and retain the blocked Run.

- [ ] **Step 8: Record real smoke evidence and commit**

Update `docs/real-codex-smoke-test.md` with redacted outputs for:

- date, OS, architecture, Codex version, project commit;
- direct and mirror probe task IDs;
- mirror no-goal and full-history proof;
- disposable and recovered Run IDs;
- frozen and accepted SHAs;
- source-ref/worktree immutability;
- no publication;
- Basestream and Huoshan installed binary/plugin versions.

Run:

```bash
bash tests/docs.sh
git diff --check
git add docs/real-codex-smoke-test.md
git commit -m "test: record real primary mirror qualification"
```

Expected: documentation checks pass and the evidence contains no account
credentials, prompt contents, patches, or private repository data.

- [ ] **Step 9: Final verification before release handoff**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features -- --test-threads=1
bash tests/docs.sh
bash tests/release.sh v0.2.7
git status --short
```

Expected: all gates pass; only the user-owned `downloads/` directory remains
untracked.
