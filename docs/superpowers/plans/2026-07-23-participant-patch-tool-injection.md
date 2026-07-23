# Participant Patch Tool Injection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the controlled patch tool deterministically available to every existing Primary integration task and safely recover the exact post-verification production blocker.

**Architecture:** Resume Primary integration tasks with a required, task-scoped participant MCP server that exposes only `consensus_apply_patch`, then verify its thread-scoped inventory before `turn/start`. Preserve the existing request-bound patch backend and add one fail-closed same-Run recovery for an empty post-verification tool-unavailable turn.

**Tech Stack:** Rust, Tokio, JSON-RPC, Codex App Server experimental v2, MCP stdio, SQLite, Git.

## Global Constraints

- Codex version must be greater than or equal to `0.144.1`; there is no upper version ceiling.
- Reuse the exact existing Primary and Reviewer task IDs and completed histories.
- Preserve both source refs and source worktrees.
- Keep the integration branch local only; do not push, create a pull request, or merge into an existing branch.
- `dangerFullAccess` and `approvalPolicy: "never"` remain the unattended turn policy.
- Participant MCP exposes only `consensus_apply_patch`.
- Missing or malformed participant capability fails before `turn/start`.
- Recovery of the existing blocker keeps the same Run and branch, never repeats the merge, and permits one new correction patch and commit.

---

### Task 1: Typed participant MCP resume and inventory preflight

**Files:**
- Modify: `crates/app-server-client/src/types.rs`
- Modify: `crates/app-server-client/src/client.rs`
- Modify: `crates/app-server-client/src/lib.rs`
- Modify: `crates/app-server-client/tests/client.rs`
- Modify: `crates/app-server-client/tests/process.rs`

**Interfaces:**
- Produces:
  `ThreadResumePolicy::{Default, PrimaryIntegration { participant_executable: PathBuf }}`
- Produces:
  `McpServerStatus { name: String, tools: BTreeMap<String, Value> }`
- Changes:
  `AppServer::resume_thread(&self, thread_id: &str, policy: &ThreadResumePolicy)`
- Produces:
  `AppServer::list_mcp_server_status(&self, thread_id: &str)`

- [ ] **Step 1: Write failing request-shape tests**

Add assertions showing that a Primary integration resume emits:

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
  }
}
```

Also assert that `ThreadResumePolicy::Default` sends only `threadId`, and that
the reconnecting adapter replays the same idempotent resume policy after a
closed proxy.

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test --locked -p app-server-client --test client typed_methods_emit_the_pinned_v2_request_shapes
cargo test --locked -p app-server-client --test process reconnecting_client_preserves_participant_resume_policy
```

Expected: compilation or assertion failure because typed resume policies do not exist.

- [ ] **Step 3: Implement typed resume policies**

Add:

```rust
pub const PARTICIPANT_MCP_SERVER: &str = "worktreeMergeConsensusParticipant";
pub const PARTICIPANT_PATCH_TOOL: &str = "consensus_apply_patch";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadResumePolicy {
    Default,
    PrimaryIntegration { participant_executable: PathBuf },
}
```

Build the exact config only for `PrimaryIntegration`. Reject a non-absolute
participant executable before sending JSON-RPC. Preserve the policy across the
reconnecting adapter's safe retry.

- [ ] **Step 4: Write failing MCP inventory tests**

Test `mcpServerStatus/list` with:

```json
{"threadId": "t-1", "detail": "toolsAndAuthOnly"}
```

Parse a response containing `worktreeMergeConsensusParticipant` and exactly
one `consensus_apply_patch` tool. Reject a missing `data` array, duplicate
server names, non-object `tools`, and malformed tool definitions.

- [ ] **Step 5: Implement and verify GREEN**

Add `list_mcp_server_status`, parse the bounded page, and run:

```bash
cargo test --locked -p app-server-client
```

Expected: all app-server-client tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/app-server-client
git commit -m "feat: inject participant patch MCP on task resume"
```

### Task 2: Participant-only MCP server mode

**Files:**
- Modify: `crates/mcp-server/src/lib.rs`
- Modify: `crates/mcp-server/src/tools.rs`
- Modify: `crates/mcp-server/tests/mcp.rs`
- Modify: `crates/cli/src/args.rs`
- Modify: `crates/cli/src/main.rs`
- Modify: `crates/cli/tests/cli.rs`

**Interfaces:**
- Produces: `ToolSurface::{Operator, ParticipantPatch}`
- Produces: `serve_stdio_surface(backend, ToolSurface)`
- Produces hidden CLI command: `participant-mcp-server`

- [ ] **Step 1: Write failing MCP surface tests**

Start both surfaces over in-memory stdio. Assert:

```rust
assert_eq!(operator_tool_names, MCP_TOOL_NAMES);
assert_eq!(participant_tool_names, ["consensus_apply_patch"]);
```

Call `consensus_status` against the participant surface and require JSON-RPC
`-32602` without invoking the backend. Call `consensus_apply_patch` and require
normal backend routing.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test --locked -p consensus-mcp-server --test mcp
```

Expected: compilation failure because `ToolSurface` does not exist.

- [ ] **Step 3: Implement the minimal surface filter**

Make `tools/list` use the selected surface and reject tools outside that
surface before argument validation or backend dispatch:

```rust
pub enum ToolSurface {
    Operator,
    ParticipantPatch,
}

impl ToolSurface {
    fn permits(self, tool: &str) -> bool {
        matches!(self, Self::Operator)
            || tool == PARTICIPANT_PATCH_TOOL
    }
}
```

Keep `serve_stdio` as the operator-compatible wrapper and add
`serve_stdio_surface`.

- [ ] **Step 4: Add and test the hidden CLI mode**

Add `Command::ParticipantMcpServer` with `hide = true`, route it through the
same `CliMcpBackend`, and test:

```bash
codex-consensus participant-mcp-server
```

lists exactly one tool while top-level help does not advertise the mode.

Run:

```bash
cargo test --locked -p consensus-mcp-server
cargo test --locked -p codex-consensus
```

Expected: all focused tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/mcp-server crates/cli
git commit -m "feat: add participant-only patch MCP server"
```

### Task 3: Coordinator preflight before Primary integration turns

**Files:**
- Modify: `crates/daemon/src/coordinator.rs`
- Modify: `crates/daemon/tests/coordinator.rs`
- Modify: `tests/fake-app-server/src/main.rs`
- Modify: `tests/e2e/tests/acceptance.rs`

**Interfaces:**
- Consumes: `ThreadResumePolicy`
- Consumes: `AppServer::list_mcp_server_status`
- Produces:
  `verify_participant_patch_capability(thread_id, statuses) -> Result<(), CoordinatorError>`

- [ ] **Step 1: Extend the fake App Server contract and write RED tests**

Record the exact method order for integration:

```text
thread/resume
mcpServerStatus/list
turn/start
```

Require the integration resume to carry the participant MCP config. Add
scenarios for missing server, missing tool, extra tool, and malformed inventory.
Assert every failure:

- returns `PATCH_TOOL_UNAVAILABLE`;
- creates no integration turn;
- records no Git write;
- leaves source refs unchanged.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test --locked -p consensus-daemon --test coordinator participant_patch
cargo test --locked -p consensus-e2e --test acceptance participant_patch
```

Expected: tests fail because integration currently resumes with only a task ID.

- [ ] **Step 3: Implement preflight ordering**

Resolve the daemon's absolute current executable once at coordinator creation
and store it in `CoordinatorOptions`:

```rust
pub participant_mcp_executable: PathBuf,
```

Use `ThreadResumePolicy::PrimaryIntegration` only for
`RequestPrimaryIntegration`. Immediately after resume, list the task-scoped
MCP inventory and require one participant server with exactly one patch tool.
Map an unavailable status method to `INCOMPATIBLE_CODEX`; map inventory
mismatch to `PATCH_TOOL_UNAVAILABLE`.

- [ ] **Step 4: Bind new canonical tool evidence to the participant server**

For new turns, require:

```text
server == worktreeMergeConsensusParticipant
tool == consensus_apply_patch
```

Retain the legacy `worktreeMergeConsensus` identity only in release-bounded
recovery validators for pre-`0.2.7` turns.

- [ ] **Step 5: Verify GREEN**

Run:

```bash
cargo test --locked -p consensus-daemon --test coordinator
cargo test --locked -p consensus-e2e --test acceptance
```

Expected: all focused coordinator and e2e tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/daemon tests/fake-app-server tests/e2e
git commit -m "fix: preflight participant patch capability"
```

### Task 4: Strict same-Run recovery for the production blocker

**Files:**
- Modify: `crates/core/src/state.rs`
- Modify: `crates/core/tests/state_machine.rs`
- Modify: `crates/daemon/src/store.rs`
- Modify: `crates/daemon/src/coordinator.rs`
- Modify: `crates/daemon/tests/store.rs`
- Modify: `crates/daemon/tests/coordinator.rs`

**Interfaces:**
- Produces:
  `RunState::retry_blocked_corrective_patch_tool_unavailable()`
- Produces:
  `SqliteRunStore::reactivate_blocked_run_with_corrective_patch_tool_retry(...)`

- [ ] **Step 1: Write state-machine RED tests**

Construct a blocked post-verification state with:

- approved plan;
- current integration branch and SHA;
- failed authoritative test evidence;
- `CONTROLLED_PATCH_TOOL_UNAVAILABLE`;
- no accepted result.

Assert the exact state restores to:

```text
status = RUNNING
phase = INTEGRATE
next_action = REQUEST_PRIMARY_INTEGRATION
round unchanged
integration branch and SHA unchanged
```

Reject missing failed evidence, an accepted result, absent integration
identity, wrong reason, and a pre-integration shape.

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test --locked -p consensus-core --test state_machine corrective_patch_tool
```

Expected: compilation failure because the recovery method does not exist.

- [ ] **Step 3: Implement the state transition**

Implement only the exact post-verification shape. Preserve current integration
payload and failure diagnostics so the retried Primary receives machine-derived
feedback and must advance the SHA.

- [ ] **Step 4: Write store and coordinator RED tests**

Seed one accepted completed blocker turn with no side-effect-capable items.
Assert resume atomically:

- validates response hash and deterministic request marker;
- archives exactly that turn;
- reacquires the repository lock;
- preserves Run, round, branch, SHA, and prior test evidence;
- clears the blocker and pending turn identity;
- cannot repeat.

Add negative fixtures for command, file-change, MCP, dynamic-tool, unknown
item, successful patch residue for the blocked request, dirty target, moved
target SHA, source drift, missing ancestor, missing failed verification, and
lock conflict.

- [ ] **Step 5: Implement canonical inspection and atomic recovery**

Add a dedicated validator rather than widening the existing pre-integration
`EXECUTION_TOOL_UNAVAILABLE` recovery. Use the established
`archive_and_reset_turn` transaction helper so stale event rows are removed
before reusing a turn record.

- [ ] **Step 6: Verify GREEN**

Run:

```bash
cargo test --locked -p consensus-core --test state_machine
cargo test --locked -p consensus-daemon --test store
cargo test --locked -p consensus-daemon --test coordinator
```

Expected: all focused tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/core crates/daemon
git commit -m "fix: recover corrective patch tool blocker"
```

### Task 5: Compatibility contract, documentation, and release version

**Files:**
- Modify: `schemas/app-server/supported-methods.json`
- Modify: `crates/app-server-client/src/compat.rs`
- Modify: `crates/app-server-client/tests/compat.rs`
- Modify: `docs/compatibility.md`
- Modify: `docs/protocol-v1.md`
- Modify: `docs/protocol-v2.md`
- Modify: `plugin/skills/worktree-merge-consensus/SKILL.md`
- Modify: `plugin/skills/worktree-merge-consensus/references/protocol.md`
- Modify: `README.md`
- Modify: `README.zh-CN.md`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: release fixture assertions as required

**Interfaces:**
- Adds required App Server method: `mcpServerStatus/list`
- Updates package and plugin version to `0.2.7`

- [ ] **Step 1: Write compatibility RED assertions**

Require the fixture to contain:

```json
"mcpServerStatus/list": ["threadId", "detail"]
```

Require `thread/resume` to allow the task-scoped config and keep the minimum
version `0.144.1` with no maximum.

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test --locked -p app-server-client --test compat
```

Expected: fixture assertion failure for the missing method.

- [ ] **Step 3: Update compatibility and operator documentation**

Document:

- participant tool injection is coordinator-owned;
- operator plugin visibility is not participant visibility;
- preflight happens before every Primary integration turn;
- the exact `0.2.6` production blocker recovery;
- one corrective patch and new SHA are required;
- installation alone does not mutate a blocked Run.

- [ ] **Step 4: Bump to `0.2.7` and verify focused gates**

Run:

```bash
cargo test --locked -p app-server-client --test compat
bash tests/docs.sh
bash tests/release-gate.sh
```

Expected: all commands exit zero.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock schemas docs plugin README.md README.zh-CN.md
git commit -m "release: prepare v0.2.7 participant patch injection"
```

### Task 6: Full verification and real App Server smoke test

**Files:**
- Modify only if verification exposes a defect in the planned implementation.

**Interfaces:**
- Consumes all prior task deliverables.

- [ ] **Step 1: Run formatting and static analysis**

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
```

Expected: both commands exit zero.

- [ ] **Step 2: Run the complete test matrix**

```bash
cargo test --locked --workspace --all-targets
cargo test --locked --workspace --all-targets --all-features
cargo test --locked --workspace --doc --all-features
```

Expected: all tests pass.

- [ ] **Step 3: Run repository gates**

```bash
bash tests/docs.sh
bash tests/static-link.sh
bash tests/release-gate.sh
```

Expected: all gates exit zero.

- [ ] **Step 4: Run minimum-toolchain verification**

Use the repository's CI-equivalent MSRV command and require all compatibility
tests to pass with the checked `0.144.1` fixture.

- [ ] **Step 5: Run a real App Server smoke test**

Against an installed Codex version greater than or equal to `0.144.1`:

1. resume an existing disposable task with the participant MCP config;
2. query `mcpServerStatus/list` for that task;
3. verify the participant server exposes exactly `consensus_apply_patch`;
4. do not send an integration turn and do not modify Git.

Expected: capability preflight succeeds without user approval.

- [ ] **Step 6: Inspect final scope and commit any verification-only fixes**

```bash
git status --short
git diff --check
git log --oneline --decorate -8
```

Expected: only intentional project files differ from `v0.2.6`; user-owned
`downloads/` remains untracked and untouched.
