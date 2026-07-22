# Unattended Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove Bubblewrap and interactive approvals from coordinator-owned turns, collect frozen-test evidence through structured App Server `command/exec`, and safely resume the original blocked Run without repeating integration work.

**Architecture:** Keep the existing participant state machine and marker protocol. Primary verification becomes a marker-only turn; after that marker, the coordinator executes the frozen argv commands itself, journals each structured result in SQLite, and feeds those results into the existing verification state transition. A one-time bounded migration archives the final legacy 0.2.4 verification turn and reuses the existing Run, branch, merge, patch, commit, and SHA.

**Tech Stack:** Rust 2024 workspace, Tokio, serde/serde_json, rusqlite, shell-words, Codex App Server v2 JSON-RPC, SQLite WAL.

## Global Constraints

- Codex CLI support remains `>=0.144.1` with no upper bound.
- Every coordinator-started participant turn uses `approvalPolicy: "never"` and `sandboxPolicy.type: "dangerFullAccess"`.
- The deployment is trusted-task/trusted-repository only; `runtimeWorkspaceRoots` are metadata, not an OS boundary.
- Frozen tests run only in the detached, remote-free verification clone for the exact integration SHA.
- Never recreate the Run, integration branch, merge, successful controlled patch, or integration commit during migration.
- Never move either frozen source ref, push a branch, create a PR, or merge into an existing branch.
- Implement each behavior test-first and run the authoritative full suite on Basestream.

---

### Task 1: Unattended App Server Policies and Structured Command Execution

**Files:**
- Modify: `crates/app-server-client/src/types.rs`
- Modify: `crates/app-server-client/src/client.rs`
- Modify: `crates/app-server-client/tests/client.rs`
- Modify: `crates/app-server-client/tests/compat.rs`

**Interfaces:**
- Consumes: existing `JsonRpcTransport::request`, `TurnExecutionPolicy`, and supported Codex version check.
- Produces: `CommandExecRequest`, `CommandExecResult`, and `AppServer::execute_command` for the coordinator.

- [ ] **Step 1: Write failing request-shape tests**

Extend `typed_methods_emit_the_pinned_v2_request_shapes` so every turn expects:

```rust
assert_eq!(turn["params"]["approvalPolicy"], "never");
assert_eq!(
    turn["params"]["sandboxPolicy"],
    json!({"type": "dangerFullAccess"})
);
```

Add a buffered command request and response assertion:

```rust
let exec = read_request(&mut lines).await;
assert_eq!(exec["method"], "command/exec");
assert_eq!(exec["params"], json!({
    "command": ["cargo", "test", "--locked"],
    "cwd": "/state/verification/run",
    "timeoutMs": 1_800_000,
    "outputBytesCap": 65_536,
    "sandboxPolicy": {"type": "dangerFullAccess"}
}));
respond(&mut server_write, &exec, json!({
    "exitCode": 7,
    "stdout": "partial stdout",
    "stderr": "test failed"
})).await;
```

Assert that the typed result preserves all three fields.

- [ ] **Step 2: Run the focused tests and verify RED**

Run on Basestream after syncing the test-only diff:

```bash
cargo test --locked -p app-server-client --test client
```

Expected: FAIL because turn policies still emit `readOnly`/`workspaceWrite` and `AppServer::execute_command` does not exist.

- [ ] **Step 3: Add the typed command API**

Add to `types.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandExecRequest {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub timeout_ms: u64,
    pub output_bytes_cap: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}
```

Add to `AppServer` and both concrete implementations:

```rust
async fn execute_command(
    &self,
    request: &CommandExecRequest,
) -> Result<CommandExecResult, AppServerError>;
```

The concrete method must reject relative cwd, empty argv, zero timeout, and zero output cap before sending:

```rust
let raw = self.rpc_request("command/exec", json!({
    "command": request.command,
    "cwd": request.cwd,
    "timeoutMs": request.timeout_ms,
    "outputBytesCap": request.output_bytes_cap,
    "sandboxPolicy": {"type": "dangerFullAccess"}
})).await?;
serde_json::from_value(raw)
    .map_err(|error| invalid(format!("invalid command/exec result: {error}")))
```

- [ ] **Step 4: Replace all participant sandbox shapes**

Keep each variant's absolute-path validation and `runtimeWorkspaceRoots`, but return:

```rust
json!({"type": "dangerFullAccess"})
```

for `ReadOnly`, `PrimaryIntegration`, and `PrimaryVerification`. Continue returning approval policy `never`.

- [ ] **Step 5: Verify GREEN and compatibility fixture**

Run:

```bash
cargo test --locked -p app-server-client
```

Expected: all app-server-client tests pass, including minimum version `0.144.1` and no-version-ceiling assertions.

- [ ] **Step 6: Commit**

```bash
git add crates/app-server-client
git commit -m "feat: run consensus turns without sandbox"
```

---

### Task 2: Durable Coordinator Verification Journal

**Files:**
- Modify: `crates/daemon/src/store.rs`
- Modify: `crates/daemon/tests/store.rs`

**Interfaces:**
- Consumes: deterministic run ID, verification request hash, Primary verification turn ID, command index, exact command, and exact cwd.
- Produces: idempotent `VerificationCommandClaim` and persisted `VerificationCommandRecord` values.

- [ ] **Step 1: Write failing journal tests**

Add tests covering these exact cases:

```rust
let claim = store.begin_verification_command(
    RUN_ID, "request-hash", "turn-7", 0,
    "cargo test --locked", Path::new("/verify/run")
).unwrap();
assert!(matches!(claim, VerificationCommandClaim::Execute(_)));

store.complete_verification_command(
    RUN_ID, "request-hash", 0, 0, "ok", ""
).unwrap();

let claim = store.begin_verification_command(
    RUN_ID, "request-hash", "turn-7", 0,
    "cargo test --locked", Path::new("/verify/run")
).unwrap();
assert!(matches!(claim, VerificationCommandClaim::Reuse(record) if record.exit_code == Some(0)));
```

Also assert that an existing `STARTED` row returns `VERIFICATION_EXECUTION_UNCERTAIN`, and that changing command, cwd, turn ID, or request hash fails without mutation.

- [ ] **Step 2: Run the focused tests and verify RED**

```bash
cargo test --locked -p consensus-daemon --test store verification_command
```

Expected: FAIL because the table and methods do not exist.

- [ ] **Step 3: Add journal types and schema**

Add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationCommandRecord {
    pub run_id: String,
    pub message_hash: String,
    pub turn_id: String,
    pub item_id: String,
    pub command_index: u32,
    pub command: String,
    pub cwd: PathBuf,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationCommandClaim {
    Execute(VerificationCommandRecord),
    Reuse(VerificationCommandRecord),
}
```

Add the migration table:

```sql
CREATE TABLE IF NOT EXISTS verification_command_executions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
    message_hash TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    command_index INTEGER NOT NULL,
    command TEXT NOT NULL,
    cwd TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('STARTED', 'COMPLETED')),
    exit_code INTEGER,
    stdout TEXT,
    stderr TEXT,
    started_at INTEGER NOT NULL,
    completed_at INTEGER,
    UNIQUE(run_id, message_hash, command_index),
    UNIQUE(run_id, item_id)
);
```

Generate `item_id` deterministically as
`coordinator-command/{message_hash}/{command_index}`.

- [ ] **Step 4: Implement claim and completion transactions**

`begin_verification_command` must insert `STARTED`, reuse only an exact
`COMPLETED` row, and reject an exact `STARTED` row as uncertain. Completion
must update exactly one matching `STARTED` row and reject a changed second
completion.

- [ ] **Step 5: Verify GREEN and reopen durability**

```bash
cargo test --locked -p consensus-daemon --test store
```

Expected: all store tests pass, including reopening SQLite and reusing a completed record.

- [ ] **Step 6: Commit**

```bash
git add crates/daemon/src/store.rs crates/daemon/tests/store.rs
git commit -m "feat: journal coordinator verification commands"
```

---

### Task 3: Marker-Only Verification With Coordinator-Owned Evidence

**Files:**
- Modify: `crates/core/src/prompts.rs`
- Modify: `crates/core/tests/prompts.rs`
- Modify: `crates/daemon/src/coordinator.rs`
- Modify: `crates/daemon/tests/coordinator.rs`

**Interfaces:**
- Consumes: `AppServer::execute_command`, `VerificationCommandClaim`, frozen `required_test_commands`, verification clone, and existing `RunState::apply_message`.
- Produces: existing `AuthoritativeVerification` and `TestEvidence` values without participant command items.

- [ ] **Step 1: Write failing prompt and coordinator tests**

Change the prompt test to require this instruction:

```text
Do not run Shell, Git, file, MCP, or patch tools in this turn. Return the VERIFICATION_READY marker when you are ready for coordinator-owned verification.
```

Add a coordinator test where the Primary verification turn has only user and
agent messages, while the fake App Server returns two structured command
results. Assert:

```rust
assert_eq!(app.executed_commands(), vec![
    vec!["cargo", "fmt", "--all", "--", "--check"],
    vec!["cargo", "test", "--locked"]
]);
assert_eq!(accepted.test_evidence.len(), 2);
assert!(accepted.test_evidence.iter().all(|item| {
    item.item_id.starts_with("coordinator-command/")
}));
```

Add a nonzero-first test proving the second command still executes and the
same Run returns to `REQUEST_PRIMARY_INTEGRATION` with bounded diagnostics.

- [ ] **Step 2: Run focused tests and verify RED**

```bash
cargo test --locked -p consensus-core --test prompts verification
cargo test --locked -p consensus-daemon --test coordinator coordinator_owned_verification
```

Expected: prompt assertion and coordinator-owned execution tests fail.

- [ ] **Step 3: Make the verification prompt marker-only**

Replace the current command-execution instructions with:

```rust
"This is a marker-only handoff to coordinator-owned verification. Do not run Shell, Git, file, MCP, or patch tools in this turn. Return VERIFICATION_READY when ready; the coordinator will run every frozen command in the exact isolated clone and derive all evidence."
```

The response remains ordinary Markdown plus the existing single marker.

- [ ] **Step 4: Add structured execution to the coordinator**

Add constants:

```rust
const VERIFICATION_COMMAND_OUTPUT_CAP_BYTES: usize = 65_536;
```

Add:

```rust
async fn execute_frozen_verification(
    &self,
    state: &RunState,
    request_hash: &str,
    turn_id: &str,
) -> Result<AuthoritativeVerification, CoordinatorError>
```

For each frozen command:

```rust
let argv = shell_words::split(command).map_err(|_| {
    CoordinatorError::operational("INVALID_TEST_COMMAND", "frozen test command is not parseable")
})?;
let claim = self.store.begin_verification_command(
    &run_id, request_hash, turn_id, index as u32, command, verification_cwd
)?;
let record = match claim {
    VerificationCommandClaim::Reuse(record) => record,
    VerificationCommandClaim::Execute(record) => {
        let result = self.app.execute_command(&CommandExecRequest {
            command: argv,
            cwd: verification_cwd.to_owned(),
            timeout_ms: u64::try_from(self.options.wait_timeout.as_millis())
                .map_err(|_| CoordinatorError::operational("INVALID_STATE", "verification timeout exceeds u64"))?,
            output_bytes_cap: VERIFICATION_COMMAND_OUTPUT_CAP_BYTES,
        }).await.map_err(|error| communication_error("command/exec", None, error))?;
        self.store.complete_verification_command(
            &run_id, request_hash, index as u32,
            result.exit_code, &result.stdout, &result.stderr
        )?
    }
};
```

Map every completed record to `TestEvidence`; combine stdout and stderr only
through the existing UTF-8-safe bounded diagnostic helper.

- [ ] **Step 5: Reject participant-side verification tools**

Before coordinator execution, require the marker-only turn to contain no
`commandExecution`, `fileChange`, `mcpToolCall`, or `dynamicToolCall` item.
User message, reasoning, context compaction, and final agent message remain
allowed. Then pass the coordinator result into `verify_message_evidence`
instead of calling `authoritative_test_evidence` on the task turn.

- [ ] **Step 6: Extend FakeAppServer**

Implement `execute_command`, record each request, and map existing
`VerificationBehavior` values to structured exits. Remove fake verification
`commandExecution` items from newly dispatched marker-only turns while
retaining legacy fixtures used by migration tests.

- [ ] **Step 7: Verify GREEN**

```bash
cargo test --locked -p consensus-core --test prompts
cargo test --locked -p consensus-daemon --test coordinator
```

Expected: all prompt and coordinator tests pass; failure routing still uses the same Run.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/prompts.rs crates/core/tests/prompts.rs crates/daemon/src/coordinator.rs crates/daemon/tests/coordinator.rs
git commit -m "feat: verify integrations through app server commands"
```

---

### Task 4: One-Time 0.2.4 Run Migration

**Files:**
- Modify: `crates/core/src/state.rs`
- Modify: `crates/core/tests/state_machine.rs`
- Modify: `crates/daemon/src/coordinator.rs`
- Modify: `crates/daemon/src/store.rs`
- Modify: `crates/daemon/tests/coordinator.rs`
- Modify: `crates/daemon/tests/store.rs`

**Interfaces:**
- Consumes: exact blocked verification diagnostic, pending turn identity, archived attempt history, canonical legacy turn, current integration Git facts, and frozen refs.
- Produces: one atomic restoration to `REQUEST_PRIMARY_VERIFICATION` with terminal status `completed-unattended-verification-migration`.

- [ ] **Step 1: Write the exact migration regression**

Construct the same archived sequence as the original Run, then a final
completed marker turn with no canonical side-effect item. Assert one resume:

```rust
assert_eq!(resumed.facts.run_id, original_run_id);
assert_eq!(resumed.integration_sha.as_deref(), Some(original_sha));
assert_eq!(resumed.next_action, NextAction::RequestPrimaryVerification);
assert_eq!(store.successful_patch_count(RUN_ID).unwrap(), 1);
```

Assert a second migration attempt fails, and variants with changed SHA, source
drift, dirty target, wrong turn, accepted evidence, or a canonical side-effect
item fail without changing state.

- [ ] **Step 2: Run focused tests and verify RED**

```bash
cargo test --locked -p consensus-daemon --test coordinator unattended_verification_migration
cargo test --locked -p consensus-daemon --test store unattended_verification_migration
```

Expected: FAIL because the bounded migration status and transaction do not exist.

- [ ] **Step 3: Add a distinct retry kind**

Replace the boolean compatibility flag with:

```rust
enum VerificationRetryKind {
    EmptyTurn,
    EventEvidenceCompatibility,
    UnattendedVerificationMigration,
}
```

Select `UnattendedVerificationMigration` only when the prior evidence
compatibility retry is already recorded, the current exact turn is completed
and side-effect-free in canonical history, no accepted evidence exists, and
the new migration status is absent.

- [ ] **Step 4: Add the atomic store migration**

Add:

```rust
pub fn reactivate_blocked_run_with_unattended_verification_retry(
    &self,
    blocked_state: &RunState,
    resumed_state: &RunState,
    message_hash: &str,
    thread_id: &str,
    turn_id: &str,
    observed_status: &str,
) -> Result<(), StoreError>
```

Validate exact state identity, acquire the existing repository lock, archive
only the bound turn as `completed-unattended-verification-migration`, clear
only its pending delivery, update the Run, and commit one SQLite transaction.

- [ ] **Step 5: Verify GREEN**

```bash
cargo test --locked -p consensus-core --test state_machine
cargo test --locked -p consensus-daemon --test store
cargo test --locked -p consensus-daemon --test coordinator
```

Expected: all recovery tests pass and no integration action is repeated.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/state.rs crates/core/tests/state_machine.rs crates/daemon/src/coordinator.rs crates/daemon/src/store.rs crates/daemon/tests/coordinator.rs crates/daemon/tests/store.rs
git commit -m "fix: migrate legacy verification to unattended execution"
```

---

### Task 5: Plugin Guidance, Release, Deployment, and Exact-Run Acceptance

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `README.md`
- Modify: `docs/protocol-v2.md`
- Modify: `docs/real-codex-smoke-test.md`
- Modify: `plugin/skills/worktree-merge-consensus/SKILL.md`
- Modify: `plugin/skills/worktree-merge-consensus/references/protocol.md`
- Modify: `.codex-plugin/plugin.json` or the existing plugin manifest version file
- Modify: release metadata files found by `rg -n '0\.2\.4'`
- Test: `crates/mcp-server/tests/plugin_contract.rs`

**Interfaces:**
- Consumes: completed code behavior and v0.2.5 release pipeline.
- Produces: published v0.2.5 assets, matching binary/plugin installations on both servers, and an accepted original Run.

- [ ] **Step 1: Write failing documentation/contract assertions**

Require the plugin contract to find all of these concepts in the installed Skill:

```text
dangerFullAccess
trusted tasks
coordinator-owned verification
do not run Shell in the verification marker turn
```

- [ ] **Step 2: Run plugin contract and verify RED**

```bash
cargo test --locked -p consensus-mcp-server --test plugin_contract
```

Expected: FAIL because 0.2.4 guidance still describes bounded writable roots and participant command evidence.

- [ ] **Step 3: Update guidance and bump to 0.2.5**

Document the trusted execution boundary, marker-only Primary verification,
coordinator command evidence, uncertain-execution fail-closed behavior, and
one-time 0.2.4 migration. Keep installation minimum `>=0.144.1` and no maximum.

- [ ] **Step 4: Run complete source verification on Basestream**

```bash
cargo fmt --all -- --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo check --locked --workspace --all-targets --all-features
```

Expected: every command exits 0 with no warning promoted by Clippy.

- [ ] **Step 5: Publish v0.2.5**

Push reviewed commits to `main`, tag the exact commit `v0.2.5`, wait for CI and
Release to succeed, verify musl SHA256 assets, and publish the GitHub release.

- [ ] **Step 6: Deploy matching v0.2.5 binary and plugin**

On Basestream and Huoshan: verify release checksums, replace
`codex-consensus`, replace the marketplace/plugin cache with v0.2.5, restart
the coordinator daemon, and require `codex-consensus doctor --json` to report
App Server and daemon reachable.

- [ ] **Step 7: Check the original Run before mutation**

On Basestream verify:

```bash
codex-consensus status f83cd777-9ed1-4369-8270-0fedd282f912 --json
git -C /gpfs/users/i-zhangguoqiang/workspace/gh_testtest rev-parse refs/heads/master
git -C /gpfs/users/i-zhangguoqiang/workspace/gh_testtest rev-parse refs/heads/codex/feature-expansion
git -C /gpfs/users/i-zhangguoqiang/workspace/gh_testtest rev-parse refs/heads/consensus/f83cd777-9ed1-4369-8270-0fedd282f912
```

Expected SHAs: frozen refs `3ad09cf...`, `e9d2475...`, integration `cdf8d7a...`.

- [ ] **Step 8: Resume exactly once and wait for terminal state**

```bash
codex-consensus resume f83cd777-9ed1-4369-8270-0fedd282f912 --json
```

Do not send a second resume. Poll status until `ACCEPTED` or a new evidence-backed terminal diagnostic.

- [ ] **Step 9: Final acceptance checks**

Require six coordinator-owned test evidence rows, exact Reviewer approval SHA,
unchanged frozen refs, one existing controlled patch, no repeated merge or
integration commit, clean worktrees, no remotes in the verification clone, and
no pushed branch or PR.

- [ ] **Step 10: Commit smoke evidence if acceptance succeeds**

Update `docs/real-codex-smoke-test.md` with redacted reproducible evidence and
commit it separately from implementation.
