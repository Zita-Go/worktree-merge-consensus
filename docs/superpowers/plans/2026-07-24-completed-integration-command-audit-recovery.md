# Completed Integration Command Audit Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Permit a completed, safely committed integration to continue when only narrowly allowlisted read-only inspection commands ended nonzero, while preserving exit-code-zero requirements for every Git write and recovering the same Run without repeating its branch, merge, patch, or commit.

**Architecture:** Split post-turn command validation into canonical terminal-shape validation and side-effect classification. Approved Git writes remain valid only as `completed` with exit code `0`; retry-safe read-only inspections may end canonically with a numeric nonzero code, and one exact `/dev/null` versus repository-relative-file `git diff --no-index` form is recovery-only. A new completed-integration `FORBIDDEN_OPERATION` resume path reuses the existing successful-patch provenance and authoritative integration-result checks, archives the rejected completed turn, and sends a read-only confirmation turn.

**Tech Stack:** Rust 1.85, Tokio, serde_json, rusqlite, existing fake App Server and Git safety adapters.

## Global Constraints

- Codex CLI and App Server compatibility remains `>= 0.144.1` with no upper bound.
- Never move, delete, reset, rebase, or push either frozen source reference.
- Never repeat an already successful branch creation, merge, controlled patch, staging operation, or commit.
- Every mutating integration command must be canonical `completed` with exit code `0`.
- A nonzero command may be retained only when its exact command and cwd pass the retry-safe read-only classifier.
- The exact recovery-only `git diff --no-index` form must compare `/dev/null` to one normalized repository-relative path and must not enter the live command-approval allowlist.
- Recovery must verify the successful patch record, frozen identities, clean authoritative target branch, both source ancestors, and changed-file inventory before reactivation.
- Preserve the current marker-plus-free-form participant response protocol.

---

### Task 1: Primary diagnostic identity and recovery state

**Files:**
- Modify: `crates/core/src/state.rs`
- Test: `crates/core/tests/state_machine.rs`

**Interfaces:**
- Consumes: `RunDiagnostic::{thread_id,source_thread_id,effective_thread_id,participant_binding_generation,participant_binding_mode,participant_server}`.
- Produces: `RunState::retry_blocked_completed_integration_forbidden_operation(&mut self) -> Result<NextAction, StateError>` and a shared primary-diagnostic identity predicate.

- [ ] **Step 1: Write failing state-machine tests**

Add one test proving an ephemeral Primary diagnostic can resume an integration invalid-response state and one test proving the new completed-integration forbidden audit can return to `REQUEST_PRIMARY_INTEGRATION` without accepted integration fields:

```rust
let source = state.facts.primary_thread_id.clone();
state.record_error(RunDiagnostic {
    code: "FORBIDDEN_OPERATION".into(),
    detail: "integration command is not canonically completed with exit code zero".into(),
    operation: None,
    action: NextAction::RequestPrimaryIntegration,
    role: Some(Role::Primary),
    thread_id: Some("primary-mirror".into()),
    source_thread_id: Some(source),
    effective_thread_id: Some("primary-mirror".into()),
    participant_binding_generation: Some(2),
    participant_binding_mode: Some("EPHEMERAL_FORK".into()),
    participant_server: Some("worktreeMergeConsensusParticipant".into()),
});
state.block("FORBIDDEN_OPERATION");
assert_eq!(
    state
        .retry_blocked_completed_integration_forbidden_operation()
        .unwrap(),
    NextAction::RequestPrimaryIntegration
);
```

- [ ] **Step 2: Run the focused core tests and verify RED**

Run:

```text
cargo test -p consensus-core --test state_machine completed_integration_forbidden -- --nocapture
cargo test -p consensus-core --test state_machine ephemeral_primary_diagnostic -- --nocapture
```

Expected: compilation fails because the new method and shared ephemeral identity behavior do not exist.

- [ ] **Step 3: Implement the minimal state transition**

Add a private predicate that accepts either the legacy direct Primary identity or a provenance-complete effective Primary diagnostic:

```rust
fn diagnostic_matches_primary(facts: &RunFacts, diagnostic: &RunDiagnostic) -> bool {
    diagnostic.thread_id.as_deref() == Some(facts.primary_thread_id.as_str())
        || (diagnostic.source_thread_id.as_deref()
            == Some(facts.primary_thread_id.as_str())
            && diagnostic.effective_thread_id.as_deref() == diagnostic.thread_id.as_deref()
            && diagnostic.participant_binding_generation.is_some()
            && diagnostic.participant_binding_mode.is_some()
            && diagnostic.participant_server.is_some())
}
```

Use it in all Primary retry transitions. Add the new transition with the same approved-plan and unaccepted-result guards as integration invalid-response recovery, but require `FORBIDDEN_OPERATION` and the exact completed-integration command-audit diagnostic.

- [ ] **Step 4: Run all core state-machine tests and verify GREEN**

Run:

```text
cargo test -p consensus-core --test state_machine -- --nocapture
```

Expected: all state-machine tests pass.

### Task 2: Canonical read-only terminal audit

**Files:**
- Modify: `crates/daemon/src/policy.rs`
- Modify: `crates/daemon/src/coordinator.rs`
- Test: `crates/daemon/src/policy.rs`
- Test: `crates/daemon/tests/coordinator.rs`

**Interfaces:**
- Consumes: `is_retry_safe_read_only_integration_command`, `decide_command_approval`, `recoverable_integration_turn_blocker`.
- Produces: recovery-only recognition for `/bin/bash -lc 'git diff --no-index -- /dev/null <relative-path>'` and `completed_integration_command_blocker(...) -> Option<String>`.

- [ ] **Step 1: Write failing policy tests**

Assert the exact `/dev/null` form is retry-safe but still rejected by live approval, and reject absolute destinations, `..`, a second external path, output flags, shell chaining, and missing `--`:

```rust
let command = "/bin/bash -lc 'git diff --no-index -- /dev/null tests/cli.rs'";
assert!(is_retry_safe_read_only_integration_command(
    &state,
    "/repo/primary",
    command
));
assert_eq!(
    decide_command_approval(
        &state,
        &json!({"cwd": "/repo/primary", "command": command})
    ),
    ApprovalDecision::Cancel
);
```

- [ ] **Step 2: Write failing coordinator audit tests**

Create a completed Primary integration turn with:

- a successful controlled patch matching SQLite;
- canonical branch creation, merge, add, and commit commands at exit code `0`;
- failed `rg --files -g AGENTS.md` at exit code `127`;
- failed target-absence inspection at exit code `128`;
- the exact recovery-only no-index diff at exit code `1`.

Assert the Run can be resumed, archives the completed turn, emits the read-only recovery override, and reaches acceptance without a second successful patch. Add negative cases showing a failed write command and a no-index command with an absolute destination remain terminal.

- [ ] **Step 3: Run focused daemon tests and verify RED**

Run:

```text
cargo test -p consensus-daemon completed_integration_forbidden -- --nocapture
cargo test -p consensus-daemon retry_safe_no_index -- --nocapture
```

Expected: the positive recovery fails with `MODEL_RESPONSE_RETRY_UNSAFE`, while the policy test rejects the desired recovery-only query.

- [ ] **Step 4: Implement terminal-shape and side-effect-aware auditing**

Add an exact parser for the recovery-only no-index form. Refactor command validation so:

```rust
if is_retry_safe_read_only_integration_command(state, cwd, command) {
    require_canonical_terminal_command(item)
} else {
    require_completed_exit_zero(item)?;
    require_live_policy_acceptance(state, cwd, command)
}
```

The canonical read-only terminal statuses are `completed` with any integer exit code and `failed` or `declined` with either null or integer exit code. Unknown status, missing canonical fields, wrong cwd, non-agent source, or a command outside both classifiers fails closed.

- [ ] **Step 5: Add the completed-integration forbidden recovery path**

Detect only a blocked first-integration `FORBIDDEN_OPERATION` whose diagnostic came from completed command auditing. Reuse the successful-patch hash validation, completed turn event/history reconstruction, authoritative target result, changed-file validation, and completed-turn archival already used by integration invalid-response recovery. Do not call `verify_branch_absent` for this path.

- [ ] **Step 6: Run focused daemon and policy tests and verify GREEN**

Run:

```text
cargo test -p consensus-daemon completed_integration_forbidden -- --nocapture
cargo test -p consensus-daemon policy::tests -- --nocapture
```

Expected: positive recovery passes; unsafe read-only shapes and all failed writes remain rejected.

### Task 3: Participant guidance and recovery documentation

**Files:**
- Modify: `crates/core/src/prompts.rs`
- Modify: `README.md`
- Modify: `README.zh-CN.md`
- Modify: `docs/compatibility.md`
- Modify: `docs/protocol-v1.md`
- Modify: `plugin/skills/worktree-merge-consensus/SKILL.md`
- Modify: `plugin/skills/worktree-merge-consensus/references/protocol.md`
- Modify: `CHANGELOG.md`
- Test: `crates/core/tests/prompts.rs` or the existing prompt assertions in `crates/daemon/tests/coordinator.rs`

**Interfaces:**
- Consumes: the existing Primary integration instruction and recovery override.
- Produces: explicit instructions to stage new files before inspecting them with `git diff --cached`, never use `git diff --no-index`, and use `git ls-files` as the fallback when `rg` is unavailable.

- [ ] **Step 1: Write failing prompt assertions**

Assert the normal integration prompt contains:

```text
Never use git diff --no-index
stage new files with git add -A before inspecting them with git diff --cached
if rg is unavailable, use git ls-files to discover tracked AGENTS.md files
```

- [ ] **Step 2: Run the prompt test and verify RED**

Run:

```text
cargo test -p consensus-core prompts -- --nocapture
```

Expected: the new guidance assertions fail.

- [ ] **Step 3: Update prompts and reader-facing recovery rules**

Document that nonzero read-only commands are not treated as successful checks; they are only safe to archive before a fresh read-only confirmation. State that writes still require exit code `0`, unsafe no-index forms remain forbidden, and no source ref or publication boundary changes.

- [ ] **Step 4: Run prompt and documentation tests and verify GREEN**

Run:

```text
cargo test -p consensus-core prompts -- --nocapture
cargo test --test docs -- --nocapture
```

Expected: all prompt and docs tests pass.

### Task 4: Release, deploy, and recover the original Run

**Files:**
- Modify: workspace `Cargo.toml` and `Cargo.lock`
- Modify: `.codex-plugin/plugin.json`
- Modify: `plugin/.codex-plugin/plugin.json`
- Modify: marketplace/version metadata and release assertions

**Interfaces:**
- Consumes: the green recovery implementation.
- Produces: release `v0.2.9`, Linux musl binaries, plugin archive, checksums, and a resumed original Run.

- [ ] **Step 1: Bump all package and plugin versions to `0.2.9`**

Update every version assertion and release artifact name consistently; do not change the Codex compatibility floor.

- [ ] **Step 2: Run the complete release gate**

Run:

```text
cargo fmt --all -- --check
cargo +1.85.0 check --locked --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=1
```

Expected: all commands exit `0` with no warnings.

- [ ] **Step 3: Commit, push, tag, and verify GitHub CI/release**

Commit tracked files only, push the tested commit to `main`, tag the same SHA as `v0.2.9`, and require successful CI and release jobs before deployment.

- [ ] **Step 4: Deploy `0.2.9` to basestream and huoshan**

Verify checksums, replace only `codex-consensus`, update the plugin marketplace source/cache, stop only the exact old coordinator daemon, and confirm:

```text
codex-consensus --version
codex-consensus doctor
codex plugin list
```

Expected: version `0.2.9`, doctor `Ready`, plugin enabled at `0.2.9`.

- [ ] **Step 5: Resume and accept Run `433797ff-11b2-49b9-9873-ff1179740da8`**

Resume the same Run. Verify it reuses the existing target branch and successful patch, executes all frozen tests in isolation, receives Reviewer approval for the exact final SHA, preserves both source refs and clean source worktrees, and leaves only the local integration branch.

