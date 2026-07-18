# Worktree Merge Consensus Implementation Plan

> Historical implementation plan. Later security review split integration from
> exact-SHA test verification and added canonical payload/state-schema binding.
> See [`docs/protocol-v1.md`](../../protocol-v1.md) for the implemented contract.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build an open-source Rust coordinator that lets two existing same-host Codex App tasks negotiate and produce an accepted local integration branch without a third coordinating agent.

**Architecture:** A single `codex-consensus` executable contains the CLI, MCP stdio entry point, per-user Unix-socket daemon, deterministic coordinator, SQLite state store, and Codex App Server adapter. The daemon drives turns in the selected primary and reviewer tasks; the primary task alone performs Git writes, while the daemon independently performs read-only Git verification. A Codex plugin wraps the same executable with a Skill and MCP tools.

**Tech Stack:** Rust 2024 edition (MSRV 1.85), Tokio, Serde/serde_json, Clap, Dialoguer, rusqlite with bundled SQLite, JSON Schema fixtures, SHA-256, UUID, Unix domain sockets, Git CLI, Codex App Server JSON-RPC, GitHub Actions, Apache-2.0.

## Global Constraints

- Version 1 supports exactly two existing Codex App/App Server tasks on one Linux host, under one Unix account, in different worktrees sharing one canonical Git common directory.
- The primary task is the only Git writer; coordinator code executes Git read-only commands only.
- No third coordinating task, subagent, cross-host commit transfer, push, PR, source-branch update, reset, rebase, branch deletion, or cleanup command.
- No integration branch may exist before an exact `APPROVED_PLAN`; acceptance requires `APPROVED_RESULT` for the current integration HEAD SHA.
- Persist minimal structured state under `$XDG_STATE_HOME/codex-consensus` or `$HOME/.local/state/codex-consensus`; full prompts and source contents are opt-in only.
- The first supported Codex version is `codex-cli 0.144.5`; unknown incompatible App Server protocols fail closed.
- Release targets are Linux x86_64 and Linux ARM64; runtime use must not require Node.js or Python.
- Repository, Skill, and protocol family use `worktree-merge-consensus`; the executable is `codex-consensus`.
- License is Apache-2.0; README documentation is English and Simplified Chinese.
- Every production change follows RED → GREEN → REFACTOR and ends in a focused commit.

---

## File Map

### Workspace and shared assets

- `Cargo.toml` — Rust workspace members, shared package metadata, and dependency versions.
- `rust-toolchain.toml` — pinned stable toolchain profile and formatting/lint components.
- `.gitignore` — Rust build, local state, generated bindings, and editor exclusions.
- `schemas/protocol-v1.json` — machine reply envelope and verdict schema.
- `schemas/app-server/0.144.5-methods.json` — checked-in method/capability compatibility fixture.

### `crates/core`

- `src/protocol.rs` — versioned envelopes, contracts, verdict payloads, schema validation.
- `src/state.rs` — phases, statuses, immutable facts, transitions, round/no-progress rules.
- `src/prompts.rs` — self-contained prompts for contract, plan, integration, and result turns.
- `src/git.rs` — canonical worktree/common-dir discovery and read-only safety verification.
- `src/hash.rs` — canonical JSON normalization and SHA-256 message hashes.

### `crates/app-server-client`

- `src/types.rs` — minimal 0.144.5 request/response/notification models.
- `src/transport.rs` — newline-delimited JSON-RPC transport with request correlation.
- `src/client.rs` — initialization, thread list/read/resume, turn start, event stream.
- `src/compat.rs` — Codex version and required-method compatibility gates.

### `crates/daemon`

- `src/store.rs` — SQLite schema and transactional run/turn persistence.
- `src/wire.rs` — CLI/MCP-to-daemon request and response protocol.
- `src/coordinator.rs` — two-task orchestration and recovery driver.
- `src/server.rs` — mode-0600 Unix socket server and per-repository run locking.
- `src/lifecycle.rs` — daemon path discovery, startup, liveness, and shutdown behavior.

### `crates/cli`

- `src/main.rs` — executable entry and hidden `daemon serve` / `mcp-server` modes.
- `src/args.rs` — Clap command tree and machine-readable output selection.
- `src/select.rs` — terminal task picker and reviewer filtering.
- `src/output.rs` — human and JSON status/result rendering.

### `crates/mcp-server`

- `src/lib.rs` — minimal MCP stdio initialize/tools/list/tools/call server.
- `src/tools.rs` — doctor/list/start/status/resume/cancel schemas and daemon calls.

### Plugin, docs, and CI

- `plugin/.codex-plugin/plugin.json` — plugin manifest.
- `plugin/.mcp.json` — MCP server command registration.
- `plugin/skills/worktree-merge-consensus/SKILL.md` — launcher-only workflow.
- `plugin/skills/worktree-merge-consensus/references/protocol.md` — human protocol reference.
- `.agents/plugins/marketplace.json` — repository-local marketplace snapshot.
- `README.md`, `README.zh-CN.md`, `SECURITY.md`, `LICENSE` — public documentation.
- `.github/workflows/ci.yml`, `.github/workflows/release.yml` — verification and Linux artifacts.

---

### Task 1: Bootstrap the Rust Workspace and Protocol Types

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `.gitignore`
- Create: `crates/core/Cargo.toml`
- Create: `crates/core/src/lib.rs`
- Create: `crates/core/src/protocol.rs`
- Create: `crates/core/src/hash.rs`
- Create: `crates/core/tests/protocol.rs`
- Create: `schemas/protocol-v1.json`

**Interfaces:**
- Produces: `Envelope`, `MessageType`, `ProtocolMessage`, `ProtocolError`, `validate_message(Value) -> Result<ProtocolMessage, ProtocolError>`, and `canonical_json_hash(&Value) -> String`.
- Consumes: no project code.

- [ ] **Step 1: Provision and verify the Rust toolchain**

The current implementation host has Homebrew but no `rustc` or `cargo`. Install
the Homebrew Rust toolchain, then verify it satisfies the MSRV and provides
rustfmt and Clippy:

```bash
brew install rust
rustc --version
cargo --version
cargo fmt --version
cargo clippy --version
```

Expected: every command exits 0 and `rustc` is version 1.85 or newer.

- [ ] **Step 2: Create the workspace manifest and failing protocol test**

Use workspace package metadata `version = "0.1.0"`, `edition = "2024"`, `rust-version = "1.85"`, and `license = "Apache-2.0"`. Add shared dependencies `serde`, `serde_json`, `thiserror`, `sha2`, `hex`, `uuid`, and `jsonschema`.

```rust
// crates/core/tests/protocol.rs
use consensus_core::protocol::{validate_message, MessageType};
use serde_json::json;

#[test]
fn approval_requires_exact_nonempty_source_shas() {
    let value = json!({
        "protocol": "worktree-merge-consensus/v1",
        "run_id": "4b230bd8-d870-4ef4-bf20-05a4c61020af",
        "message_type": "APPROVED_PLAN",
        "phase": "PLAN_REVIEW",
        "round": 1,
        "primary_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "reviewer_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "plan_revision": 1,
        "integration_branch": null,
        "integration_sha": null,
        "reason_code": null,
        "payload": {
            "approved_plan_revision": 1,
            "approved_primary_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "approved_reviewer_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "uncovered_items": []
        }
    });
    let parsed = validate_message(value).expect("valid approval");
    assert_eq!(parsed.envelope.message_type, MessageType::ApprovedPlan);
}

#[test]
fn natural_language_is_not_a_protocol_message() {
    let error = validate_message(json!("looks good")).unwrap_err();
    assert!(error.to_string().contains("JSON object"));
}
```

- [ ] **Step 3: Run the focused test and verify RED**

Run: `cargo test -p consensus-core --test protocol`

Expected: compilation fails because `consensus_core::protocol` and `validate_message` do not exist.

- [ ] **Step 4: Implement typed protocol validation and canonical hashes**

Define uppercase serde names for message types and phases. Reject non-object input, invalid SHA strings, zero rounds, null plan revisions for plan verdicts, payload/envelope SHA mismatches, and uncovered items on approvals. Load `schemas/protocol-v1.json` with `include_str!` and validate before typed deserialization.

```rust
pub fn validate_message(value: Value) -> Result<ProtocolMessage, ProtocolError> {
    if !value.is_object() {
        return Err(ProtocolError::ExpectedObject);
    }
    PROTOCOL_SCHEMA
        .validate(&value)
        .map_err(|errors| ProtocolError::Schema(
            errors.map(|e| e.to_string()).collect::<Vec<_>>().join("; ")
        ))?;
    let message: ProtocolMessage = serde_json::from_value(value)?;
    message.validate_invariants()?;
    Ok(message)
}
```

- [ ] **Step 5: Run protocol tests and workspace formatting**

Run: `cargo test -p consensus-core --test protocol && cargo fmt --all --check`

Expected: all protocol tests pass; rustfmt reports no diff.

- [ ] **Step 6: Commit the protocol foundation**

```bash
git add Cargo.toml rust-toolchain.toml .gitignore crates/core schemas/protocol-v1.json
git commit -m "feat: add consensus protocol foundation"
```

### Task 2: Implement the Deterministic State Machine and Prompt Builder

**Files:**
- Create: `crates/core/src/state.rs`
- Create: `crates/core/src/prompts.rs`
- Modify: `crates/core/src/lib.rs`
- Create: `crates/core/tests/state_machine.rs`
- Create: `crates/core/tests/prompts.rs`

**Interfaces:**
- Consumes: `ProtocolMessage`, `MessageType`, and `canonical_json_hash` from Task 1.
- Produces: `RunState`, `Phase`, `RunStatus`, `Role`, `NextAction`, `RunState::apply_message`, `RunState::pause`, `RunState::resume`, and `build_turn_prompt`.

- [ ] **Step 1: Write failing state transition tests**

```rust
#[test]
fn integration_is_impossible_before_plan_approval() {
    let mut state = fixture_state(Phase::PlanReview);
    let error = state.request_integration().unwrap_err();
    assert_eq!(error.code(), "PLAN_NOT_APPROVED");
}

#[test]
fn stale_result_approval_is_rejected() {
    let mut state = fixture_result_state("c".repeat(40));
    let stale = approved_result("d".repeat(40));
    let error = state.apply_message(stale).unwrap_err();
    assert_eq!(error.code(), "STALE_INTEGRATION_SHA");
}

#[test]
fn sixth_unapproved_round_blocks() {
    let mut state = fixture_state(Phase::PlanReview);
    for round in 1..=6 {
        state.apply_message(changes_required(round, format!("issue-{round}"))).unwrap();
    }
    assert_eq!(state.status, RunStatus::Blocked);
    assert_eq!(state.reason_code.as_deref(), Some("ROUND_LIMIT"));
}
```

- [ ] **Step 2: Run the state tests and verify RED**

Run: `cargo test -p consensus-core --test state_machine`

Expected: compilation fails because `RunState` and transition methods do not exist.

- [ ] **Step 3: Implement phases, actions, limits, and pause/resume**

Use immutable `RunFacts` for run ID, thread IDs, canonical paths, source SHAs, and source refs. `apply_message` must compare every immutable envelope field before advancing. Hash normalized issue IDs and plan payloads to detect two unchanged rounds. Return actions rather than executing effects:

```rust
pub enum NextAction {
    RequestPrimaryContract,
    RequestReviewerContract,
    RequestPrimaryPlan,
    RequestReviewerPlanVerdict,
    RequestPrimaryIntegration,
    RequestReviewerResultVerdict,
    RevalidateAndAccept,
    WaitForUser,
    Stop,
}
```

- [ ] **Step 4: Add self-contained prompt tests and implementation**

Verify every prompt contains protocol version, run ID, phase, round, both source SHAs, complete current payload, output JSON Schema, and an instruction that text outside one JSON object is invalid. Ensure plan/result prompts contain full contracts or coverage, not deltas.

Run: `cargo test -p consensus-core --test prompts`

Expected: prompt tests pass.

- [ ] **Step 5: Run all core tests and commit**

```bash
cargo test -p consensus-core
git add crates/core
git commit -m "feat: add deterministic consensus state machine"
```

### Task 3: Add Read-only Git Repository Verification

**Files:**
- Create: `crates/core/src/git.rs`
- Modify: `crates/core/src/lib.rs`
- Create: `crates/core/tests/git_inspector.rs`

**Interfaces:**
- Produces: `GitInspector`, `WorktreeSnapshot`, `SourceRef`, `IntegrationSnapshot`, `GitSafetyError`, `inspect_worktree`, `verify_same_repository`, `verify_frozen_sources`, and `verify_integration_result`.
- Consumes: `RunFacts` from Task 2.

- [ ] **Step 1: Write a failing real-worktree test**

The test initializes a temporary repository, configures local test identity,
commits one file, creates two branches and worktrees, then asserts canonical
common-directory equality and different worktree paths.

```rust
#[test]
fn two_worktrees_share_objects_without_sharing_paths() {
    let fixture = GitFixture::two_worktrees();
    let primary = inspect_worktree(fixture.primary()).unwrap();
    let reviewer = inspect_worktree(fixture.reviewer()).unwrap();
    verify_same_repository(&primary, &reviewer).unwrap();
    assert_ne!(primary.worktree, reviewer.worktree);
    assert_eq!(primary.common_dir, reviewer.common_dir);
}
```

- [ ] **Step 2: Run the test and verify RED**

Run: `cargo test -p consensus-core --test git_inspector`

Expected: compilation fails because the Git inspector API does not exist.

- [ ] **Step 3: Implement a non-shell Git command runner**

Use `std::process::Command::new("git").arg("-C").arg(path)` and fixed argument
arrays. Never invoke `sh -c`. Implement only read-only commands:
`rev-parse --show-toplevel`, `rev-parse --git-common-dir`, `rev-parse HEAD`,
`status --porcelain=v1 -z`, `symbolic-ref --quiet HEAD`, `show-ref --verify`,
`ls-files -u`, `merge-base --is-ancestor`, and `grep` through Rust file reads for
conflict markers in changed text files.

- [ ] **Step 4: Add dirty, drift, detached, and branch-exists tests**

Verify errors `DIRTY_WORKTREE`, `SOURCE_DRIFT`, `DIFFERENT_REPOSITORY`, and
`INTEGRATION_BRANCH_EXISTS`; verify detached sources are accepted by SHA and
that no test invokes a Git write through `GitInspector`.

Run: `cargo test -p consensus-core --test git_inspector`

Expected: all Git inspector tests pass.

- [ ] **Step 5: Commit Git safety checks**

```bash
git add crates/core/src/git.rs crates/core/src/lib.rs crates/core/tests/git_inspector.rs
git commit -m "feat: add read-only git safety inspection"
```

### Task 4: Build the Codex App Server Client and Compatibility Gate

**Files:**
- Create: `crates/app-server-client/Cargo.toml`
- Create: `crates/app-server-client/src/lib.rs`
- Create: `crates/app-server-client/src/types.rs`
- Create: `crates/app-server-client/src/transport.rs`
- Create: `crates/app-server-client/src/client.rs`
- Create: `crates/app-server-client/src/compat.rs`
- Create: `crates/app-server-client/tests/json_rpc.rs`
- Create: `crates/app-server-client/tests/compat.rs`
- Create: `schemas/app-server/0.144.5-methods.json`

**Interfaces:**
- Produces: async trait `AppServer`, `CodexAppServer`, `ThreadSummary`, `ThreadDetail`, `TurnHandle`, `AppEvent`, and `CompatibilityReport`.
- Consumes: installed `codex` executable and minimal 0.144.5 method fixture.

- [ ] **Step 1: Write failing JSON-RPC correlation tests**

```rust
#[tokio::test]
async fn correlates_out_of_order_responses_and_keeps_notifications() {
    let (client, mut fake) = duplex_transport();
    let first = client.request("thread/list", json!({"limit": 50}));
    let second = client.request("thread/read", json!({"threadId": "t-1", "includeTurns": true}));
    fake.respond_to_second_then_first().await;
    assert_eq!(second.await.unwrap()["thread"]["id"], "t-1");
    assert!(first.await.unwrap()["data"].is_array());
    assert_eq!(client.next_event().await.unwrap().method, "turn/completed");
}
```

- [ ] **Step 2: Run the transport test and verify RED**

Run: `cargo test -p app-server-client --test json_rpc`

Expected: compilation fails because transport and client types do not exist.

- [ ] **Step 3: Implement newline JSON-RPC and process connection**

Spawn `codex app-server proxy` against the managed daemon control socket. If
`doctor` determines the daemon is absent, run `codex app-server daemon start`
first. Split stdin/stdout, assign monotonically increasing request IDs, keep a
pending-request map, and broadcast notifications. Capture stderr separately and
redact it before user logs.

- [ ] **Step 4: Implement minimal App Server methods**

Model the exact 0.144.5 fields required by generated schema. Initialize with:

```json
{
  "clientInfo": {
    "name": "worktree-merge-consensus",
    "title": "Worktree Merge Consensus",
    "version": "0.1.0"
  },
  "capabilities": null
}
```

Use `thread/list` with newest-first pagination, `thread/read` with
`includeTurns: true`, `thread/resume` by thread ID only, and `turn/start` with a
single text `UserInput` plus `outputSchema` set to protocol v1.

- [ ] **Step 5: Add compatibility tests**

The fixture lists required methods and supported version range. Verify 0.144.5
passes, missing `turn/start` fails, malformed version output fails, and a future
unknown incompatible version returns `INCOMPATIBLE_CODEX` without starting a
turn.

Run: `cargo test -p app-server-client`

Expected: all client and compatibility tests pass.

- [ ] **Step 6: Commit the App Server adapter**

```bash
git add Cargo.toml crates/app-server-client schemas/app-server
git commit -m "feat: add codex app server client"
```

### Task 5: Add Transactional Persistence and the Unix-socket Daemon

**Files:**
- Create: `crates/daemon/Cargo.toml`
- Create: `crates/daemon/src/lib.rs`
- Create: `crates/daemon/src/store.rs`
- Create: `crates/daemon/src/wire.rs`
- Create: `crates/daemon/src/server.rs`
- Create: `crates/daemon/src/lifecycle.rs`
- Create: `crates/daemon/tests/store.rs`
- Create: `crates/daemon/tests/server.rs`

**Interfaces:**
- Consumes: `RunState` from core and `AppServer` abstraction.
- Produces: `SqliteRunStore`, `DaemonRequest`, `DaemonResponse`, `DaemonClient`, `run_server`, and `ensure_daemon`.

- [ ] **Step 1: Write failing persistence tests**

```rust
#[test]
fn pending_send_survives_reopen_without_storing_prompt() {
    let db = TempDb::new();
    let store = SqliteRunStore::open(db.path()).unwrap();
    store.insert_run(&fixture_run()).unwrap();
    store.record_pending_send("run-1", "primary", "PLAN_REVIEW", 2, "hash-1").unwrap();
    drop(store);
    let reopened = SqliteRunStore::open(db.path()).unwrap();
    let send = reopened.pending_send("run-1").unwrap().unwrap();
    assert_eq!(send.message_hash, "hash-1");
    assert!(send.full_prompt.is_none());
}
```

- [ ] **Step 2: Run store tests and verify RED**

Run: `cargo test -p consensus-daemon --test store`

Expected: compilation fails because the daemon crate and store do not exist.

- [ ] **Step 3: Implement migrations and atomic transitions**

Create tables `runs`, `source_facts`, `turns`, `transitions`, and `locks`. Store
validated structured payload fields and hashes, not full messages. Use one SQL
transaction for pending-send creation and another for response acceptance plus
state advancement. Enable WAL and foreign keys.

- [ ] **Step 4: Write failing Unix-socket permission and RPC tests**

Start the daemon in a temporary state directory, send newline-delimited
`DaemonRequest::Status`, and assert the response. On Unix, assert socket mode is
exactly `0600`; reject a second active run for the same canonical common dir.

- [ ] **Step 5: Implement daemon wire protocol and lifecycle**

Use one JSON request and response per line. `ensure_daemon` first connects to the
expected socket; if absent, it spawns the current executable with hidden
`daemon serve --state-dir ...`, waits up to five seconds for readiness, and then
connects. A PID file is advisory only; socket liveness is authoritative.

- [ ] **Step 6: Run daemon tests and commit**

```bash
cargo test -p consensus-daemon
git add Cargo.toml crates/daemon
git commit -m "feat: add persistent consensus daemon"
```

### Task 6: Implement the Two-task Coordinator and Recovery Driver

**Files:**
- Create: `crates/daemon/src/coordinator.rs`
- Modify: `crates/daemon/src/lib.rs`
- Modify: `crates/daemon/src/server.rs`
- Create: `crates/daemon/tests/coordinator.rs`
- Create: `tests/fixtures/transcripts/conflict-free.json`
- Create: `tests/fixtures/transcripts/plan-revision.json`
- Create: `tests/fixtures/transcripts/result-revision.json`

**Interfaces:**
- Consumes: `AppServer`, `RunState`, prompt builders, `GitInspector`, and `SqliteRunStore`.
- Produces: `Coordinator::start`, `Coordinator::drive`, `Coordinator::resume`, `Coordinator::cancel`, and daemon start/status/resume/cancel handlers.

- [ ] **Step 1: Write a failing conflict-free orchestration test**

Create `FakeAppServer` with two thread summaries and scripted replies. Assert the
request order:

```text
primary CONTRACT
reviewer CONTRACT
primary PLAN
reviewer PLAN verdict
primary INTEGRATE
reviewer RESULT verdict
```

Assert no integration request is issued before `APPROVED_PLAN`, and the final
state is `ACCEPTED` only after the exact integration SHA is approved.

- [ ] **Step 2: Run the coordinator test and verify RED**

Run: `cargo test -p consensus-daemon --test coordinator conflict_free`

Expected: compilation fails because `Coordinator` does not exist.

- [ ] **Step 3: Implement one-action-at-a-time driving**

For each `NextAction`, re-read selected thread metadata, wait for active turns,
revalidate source facts, reconstruct required payloads from canonical task
history, build a full prompt, record pending-send, start a turn, and wait for
completion or user-action notification. Parse only the final assistant JSON
object constrained by `outputSchema`.

- [ ] **Step 4: Add plan and result revision tests**

Verify full payload resend, plan revision increments, old approvals become
invalid, result fixes produce a new SHA, and reviewer approval of the previous
SHA is rejected.

- [ ] **Step 5: Add crash, duplicate, pause, and cancellation tests**

Simulate crashes before send, after send, and after response. Verify history
lookup avoids duplicate turns. Verify approval/input notifications yield
`PAUSED_USER_ACTION`; resume revalidates Git; cancel schedules no new turns and
does not interrupt an active task.

- [ ] **Step 6: Add round/no-progress and failure tests**

Cover `ROUND_LIMIT`, `NO_PROGRESS`, `INVALID_RESPONSE`, `SOURCE_DRIFT`,
`DIRTY_WORKTREE`, `TEST_FAILURE`, `COMMUNICATION_FAILURE`, and
`HISTORY_UNAVAILABLE`.

Run: `cargo test -p consensus-daemon --test coordinator`

Expected: all scripted orchestration and recovery tests pass.

- [ ] **Step 7: Commit coordinator behavior**

```bash
git add crates/daemon tests/fixtures/transcripts
git commit -m "feat: coordinate reviewed worktree integration"
```

### Task 7: Build the CLI, Interactive Selector, and Daemon Entry Modes

**Files:**
- Create: `crates/cli/Cargo.toml`
- Create: `crates/cli/src/main.rs`
- Create: `crates/cli/src/args.rs`
- Create: `crates/cli/src/select.rs`
- Create: `crates/cli/src/output.rs`
- Create: `crates/cli/tests/cli.rs`

**Interfaces:**
- Consumes: `DaemonClient`, thread summaries, `GitInspector`, and daemon lifecycle.
- Produces: executable `codex-consensus` with `doctor`, `threads list`, `run`, `status`, `resume`, `cancel`, hidden `daemon serve`, and `mcp-server` modes.

- [ ] **Step 1: Write failing command-surface tests**

```rust
#[test]
fn help_lists_public_commands_but_not_internal_modes() {
    Command::cargo_bin("codex-consensus").unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("doctor"))
        .stdout(predicate::str::contains("threads"))
        .stdout(predicate::str::contains("run"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("daemon serve").not());
}
```

- [ ] **Step 2: Run CLI tests and verify RED**

Run: `cargo test -p codex-consensus --test cli`

Expected: the binary package does not exist.

- [ ] **Step 3: Implement argument parsing and JSON output**

`run` accepts optional `--primary-thread`, `--reviewer-thread`,
`--integration-branch`, repeatable `--test`, and `--json`. Require both thread
flags together. Human output goes to stdout; diagnostics go to stderr; JSON mode
emits one object and no decoration.

- [ ] **Step 4: Implement interactive task selection behind a trait**

Define `TaskSelector` so tests inject fixed choices. Production uses Dialoguer
fuzzy selection. Select primary first, inspect its canonical repository, filter
reviewer candidates to a different thread and worktree with the same common
directory, then show a final confirmation.

- [ ] **Step 5: Add doctor and lifecycle behavior**

`doctor` verifies executable discovery, version 0.144.5 compatibility, managed
App Server start/proxy, state-directory permissions, Git availability, daemon
liveness, and required App Server methods without starting a model turn.

- [ ] **Step 6: Run CLI tests and commit**

```bash
cargo test -p codex-consensus
git add Cargo.toml crates/cli
git commit -m "feat: add consensus command line interface"
```

### Task 8: Add the MCP Server and Codex Plugin

**Files:**
- Create: `crates/mcp-server/Cargo.toml`
- Create: `crates/mcp-server/src/lib.rs`
- Create: `crates/mcp-server/src/tools.rs`
- Create: `crates/mcp-server/tests/mcp.rs`
- Modify: `crates/cli/src/main.rs`
- Create: `plugin/.codex-plugin/plugin.json`
- Create: `plugin/.mcp.json`
- Create: `plugin/skills/worktree-merge-consensus/SKILL.md`
- Create: `plugin/skills/worktree-merge-consensus/references/protocol.md`
- Create: `plugin/skills/worktree-merge-consensus/agents/openai.yaml`
- Create: `.agents/plugins/marketplace.json`

**Interfaces:**
- Consumes: CLI executable, daemon wire protocol, and protocol reference.
- Produces: MCP tools `consensus_doctor`, `consensus_list_threads`, `consensus_start`, `consensus_status`, `consensus_resume`, and `consensus_cancel`; installable plugin bundle.

- [ ] **Step 1: Write failing MCP handshake and tool-list tests**

Feed JSON-RPC requests to an in-memory stdin/stdout harness:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
```

Assert the response lists exactly the six public tools with JSON input schemas.

- [ ] **Step 2: Run MCP tests and verify RED**

Run: `cargo test -p consensus-mcp-server --test mcp`

Expected: the MCP crate does not exist.

- [ ] **Step 3: Implement minimal stdio MCP protocol**

Support `initialize`, `notifications/initialized`, `ping`, `tools/list`, and
`tools/call`. Return JSON-RPC method/parameter errors for all unsupported input.
Tool handlers call the Unix-socket daemon and return textual plus structured
content. `consensus_start` returns immediately with `run_id` and initial status.

- [ ] **Step 4: Add plugin metadata**

Use a plugin manifest with name/version `worktree-merge-consensus`/`0.1.0`,
Apache-2.0, `skills: "./skills/"`, and `mcpServers: "./.mcp.json"`. Register:

```json
{
  "mcpServers": {
    "worktreeMergeConsensus": {
      "title": "Worktree Merge Consensus",
      "description": "Coordinate reviewed integration across two existing Codex tasks.",
      "cwd": ".",
      "command": "codex-consensus",
      "args": ["mcp-server"]
    }
  }
}
```

- [ ] **Step 5: Write the launcher-only Skill**

The Skill must require two existing same-host tasks, call MCP doctor/list/start,
end the launch turn after returning the run ID, and explicitly state that it
does not relay review rounds itself. It must not mention or invoke subagents,
thread creation, push, PR creation, or source-branch mutation.

- [ ] **Step 6: Validate MCP and plugin files, then commit**

Run:

```bash
cargo test -p consensus-mcp-server
python3 ~/.codex/skills/.system/skill-creator/scripts/quick_validate.py plugin/skills/worktree-merge-consensus
```

Expected: MCP tests and Skill validation pass.

```bash
git add Cargo.toml crates/mcp-server crates/cli plugin .agents
git commit -m "feat: add codex plugin entry point"
```

### Task 9: Add Full End-to-end Safety and Recovery Tests

**Files:**
- Create: `tests/e2e/Cargo.toml`
- Create: `tests/e2e/src/lib.rs`
- Create: `tests/e2e/tests/acceptance.rs`
- Create: `tests/fake-app-server/Cargo.toml`
- Create: `tests/fake-app-server/src/main.rs`
- Modify: `Cargo.toml`

**Interfaces:**
- Consumes: the complete executable and public daemon/App Server protocols.
- Produces: process-level acceptance evidence for the design's fourteen release criteria.

- [ ] **Step 1: Write a failing process-level acceptance test**

Start the fake App Server, the real consensus daemon, and the real CLI against a
temporary Git repository with two worktrees. Script both task responses and
assert final JSON status `ACCEPTED`, exact integration SHA, clean integration
worktree, unchanged source refs, and no remote configured or push attempted.

- [ ] **Step 2: Run the acceptance test and verify RED**

Run: `cargo test -p consensus-e2e --test acceptance conflict_free -- --nocapture`

Expected: failure until daemon process wiring and fake App Server endpoint
overrides are exposed to the test.

- [ ] **Step 3: Add injectable executable/socket endpoints**

Support test-only environment variables for App Server command, state directory,
and daemon socket. Production defaults remain fixed and safe. Do not permit an
arbitrary TCP App Server endpoint in v1.

- [ ] **Step 4: Add all required acceptance scenarios**

Implement conflict-free, conflict resolution, multiple plan revisions, result
rejection/new SHA, source drift, dirty worktree, existing branch, detached
source, invalid reply, no progress, round limit, permission pause/resume,
daemon crash/restart, duplicate notification, and cancellation fixtures.

- [ ] **Step 5: Run the complete workspace suite and commit**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
git add Cargo.toml tests crates
git commit -m "test: cover consensus safety and recovery"
```

### Task 10: Add Public Documentation, Security Policy, and Linux Releases

**Files:**
- Create: `README.md`
- Create: `README.zh-CN.md`
- Create: `SECURITY.md`
- Create: `LICENSE`
- Create: `.github/workflows/ci.yml`
- Create: `.github/workflows/release.yml`
- Create: `docs/protocol-v1.md`
- Create: `docs/compatibility.md`
- Create: `docs/real-codex-smoke-test.md`

**Interfaces:**
- Consumes: stable CLI, plugin, test commands, and compatibility policy.
- Produces: install/use/operate documentation and reproducible release automation.

- [ ] **Step 1: Write documentation checks before documentation**

Add a shell test invoked by CI that verifies both READMEs mention the same six
commands, same-host limitation, Codex 0.144.5 floor, no-push boundary, plugin
installation flow, and checksum verification. Verify all Markdown links are
relative or HTTPS and all documented commands exist in `--help`.

- [ ] **Step 2: Run docs checks and verify RED**

Run: `bash tests/docs.sh`

Expected: failure because public docs and workflows do not exist.

- [ ] **Step 3: Write English and Chinese usage documentation**

Document standalone binary installation, plugin marketplace registration,
`doctor`, interactive and flag-based starts, Codex Skill invocation, statuses,
recovery, cancellation, state/log locations, privacy defaults, troubleshooting,
and non-goals. Include an explicit warning that App Server is experimental.

- [ ] **Step 4: Add security, protocol, and compatibility documents**

Use the unmodified Apache License 2.0 text. `SECURITY.md` provides private
vulnerability reporting guidance without inventing an email address: direct
users to GitHub's private security advisory form for the repository. Protocol
and compatibility docs mirror the checked-in schemas and version gate.

- [ ] **Step 5: Add CI and release workflows**

CI runs fmt, Clippy with warnings denied, workspace tests, docs checks,
`cargo audit`, and license checks. Tag releases build Linux x86_64 and ARM64,
produce SHA-256 checksums, generate CycloneDX SBOMs, and attach plugin assets.
Release jobs must verify artifacts before upload.

- [ ] **Step 6: Run final local verification**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash tests/docs.sh
git diff --check
```

Expected: every command exits 0.

- [ ] **Step 7: Commit release readiness**

```bash
git add README.md README.zh-CN.md SECURITY.md LICENSE docs .github tests/docs.sh
git commit -m "docs: prepare public project release"
```

---

## Final Verification Checklist

- [ ] `cargo fmt --all --check` exits 0.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
- [ ] `cargo test --workspace` exits 0 with every unit, fake-server, Git, daemon, MCP, and E2E test passing.
- [ ] `bash tests/docs.sh` exits 0.
- [ ] Plugin Skill passes the official Skill validator.
- [ ] A disposable real-Codex smoke run is recorded for Codex CLI 0.144.5, or the release is explicitly marked pre-release until that evidence exists.
- [ ] `git status --short` is empty.
- [ ] Source branches used by the smoke fixture remain unchanged.
- [ ] No Git remote is added and no GitHub repository is created or pushed until the user supplies the destination owner and authorizes publication.
