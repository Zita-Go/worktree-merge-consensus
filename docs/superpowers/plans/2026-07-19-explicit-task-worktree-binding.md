# Explicit Task-Worktree Binding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let two arbitrary existing Codex tasks be explicitly and immutably bound to two committed registered worktrees, without inferring either source from the task's App Server `cwd`.

**Architecture:** Keep task identity and Git source identity as separate inputs until preflight combines them into the existing `RunFacts`. Add read-only registered-worktree discovery in `consensus-core`, expose it through CLI and MCP, make interactive task and worktree selection independent, and retain the existing daemon phase machine while removing task-cwd identity checks. Every dispatched turn continues to receive the frozen bound worktree as its explicit execution policy.

**Tech Stack:** Rust 2024, Clap, Dialoguer, Tokio, Serde/JSON, Git porcelain commands, JSON-RPC/MCP, SQLite-backed daemon, process-level fake Codex App Server, Bash documentation/release checks.

## Global Constraints

- Follow the approved amendment in `docs/superpowers/specs/2026-07-19-explicit-task-worktree-binding-design.md`.
- Support Codex `>=0.144.1` with no upper version ceiling.
- Treat `ThreadSummary.cwd` as display metadata only. No selection, freeze, resume, or runtime identity check may derive a source worktree from it.
- Require distinct task IDs and distinct canonical registered worktrees in one canonical Git common directory.
- Accept attached and detached source worktrees; require clean committed source state.
- Freeze task IDs, canonical paths, refs, and SHAs in the existing `RunFacts`; do not migrate persisted state or alter protocol family `worktree-merge-consensus/v1`.
- Never push, create a PR, merge into an existing branch, rewrite source refs, clean user files, or delete/overwrite a legacy Skill.
- Preserve fail-closed runtime verification, exact-SHA verification clone, reviewer read-only policy, primary-only integration writes, source-ref immutability, and new-local-integration-branch result.
- Use TDD for every behavior change: add the focused failing test, run it and observe the expected failure, implement the smallest production change, rerun it, then run the owning crate suite.
- Use `apply_patch` for edits, keep unrelated user changes intact, and commit each task only after its focused verification passes.

---

## Task 1: Add Registered Worktree Discovery and Pair Validation

**Files:**

- Modify: `crates/core/src/git.rs`
- Modify: `crates/core/tests/git_inspector.rs`

**Interfaces:**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeIssue {
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredWorktree {
    pub worktree: PathBuf,
    pub common_dir: PathBuf,
    pub head_sha: Option<String>,
    pub source_ref: Option<SourceRef>,
    pub clean: Option<bool>,
    pub bare: bool,
    pub issue: Option<WorktreeIssue>,
}

impl GitInspector {
    pub fn list_registered_worktrees(
        &self,
        repository: impl AsRef<Path>,
    ) -> Result<Vec<RegisteredWorktree>, GitSafetyError>;

    pub fn inspect_registered_worktree(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<WorktreeSnapshot, GitSafetyError>;

    pub fn inspect_registered_pair(
        &self,
        primary: impl AsRef<Path>,
        reviewer: impl AsRef<Path>,
    ) -> Result<(WorktreeSnapshot, WorktreeSnapshot), GitSafetyError>;
}
```

- [ ] **Step 1: Add failing discovery tests**

Extend `GitFixture` coverage to assert that `list_registered_worktrees` returns every registered entry with canonical path/common directory, full SHA, attached ref or detached state, and clean status. Add a stale registered worktree fixture and assert it remains in output with `issue.code == "WORKTREE_UNAVAILABLE"` rather than being pruned.

```rust
#[test]
fn registered_worktree_discovery_reports_canonical_git_facts() {
    let fixture = GitFixture::two_worktrees();
    let entries = GitInspector::default()
        .list_registered_worktrees(fixture.primary())
        .unwrap();

    assert!(entries.iter().any(|entry| {
        entry.worktree == fs::canonicalize(fixture.reviewer()).unwrap()
            && entry.head_sha.as_deref() == Some(fixture.reviewer_sha())
            && entry.clean == Some(true)
    }));
}
```

- [ ] **Step 2: Run the focused tests and observe red**

Run:

```bash
cargo test -p consensus-core --test git_inspector registered_worktree_discovery -- --nocapture
```

Expected: compilation fails because the discovery API and result types do not exist.

- [ ] **Step 3: Implement read-only porcelain discovery**

Run `git worktree list --porcelain -z` through the existing read-only Git command gate. Parse NUL-delimited records without shell interpolation. Resolve the anchor's canonical common directory once, preserve bare/unavailable entries, inspect accessible non-bare entries through `inspect_worktree`, sort by canonical/declared path, and never call `prune`, `repair`, or another mutating command.

Extend `validate_read_only_command` only for this exact command:

```rust
["worktree", "list", "--porcelain", "-z"] => true,
```

- [ ] **Step 4: Add failing selection-safety tests**

Cover absolute registered paths, a relative path, a subdirectory instead of the registered root, duplicate canonical paths, separate repositories, dirty sources, a missing registered path, and detached HEAD. Assert exact codes:

```rust
assert_eq!(relative.code(), "UNREGISTERED_WORKTREE");
assert_eq!(duplicate.code(), "DUPLICATE_WORKTREE");
assert_eq!(mismatch.code(), "REPOSITORY_MISMATCH");
assert_eq!(dirty.code(), "DIRTY_WORKTREE");
assert_eq!(missing.code(), "WORKTREE_UNAVAILABLE");
```

- [ ] **Step 5: Implement registered pair inspection**

Require absolute selected paths, canonicalize them, require exact equality with the registered root returned by porcelain, reject bare entries, inspect each path against its own repository before comparing common directories, and then call frozen-source validation. Rename only the pair-level old codes (`SAME_WORKTREE`, `DIFFERENT_REPOSITORY`) to the approved public codes; retain existing internal Git errors where they are not public preflight outcomes.

- [ ] **Step 6: Verify and commit**

Run:

```bash
cargo test -p consensus-core --test git_inspector
cargo test -p consensus-core
cargo fmt --all --check
```

Expected: all pass.

Commit:

```bash
git add crates/core/src/git.rs crates/core/tests/git_inspector.rs
git commit -m "feat: discover and validate registered worktrees"
```

---

## Task 2: Separate CLI Task Selection from Worktree Selection

**Files:**

- Modify: `crates/cli/src/args.rs`
- Modify: `crates/cli/src/select.rs`
- Modify: `crates/cli/src/main.rs`
- Modify: `crates/cli/tests/cli.rs`

**Interfaces:**

```rust
pub struct SelectedTaskPair {
    pub primary: ThreadSummary,
    pub reviewer: ThreadSummary,
}

pub struct SelectedBinding {
    pub tasks: SelectedTaskPair,
    pub primary_snapshot: WorktreeSnapshot,
    pub reviewer_snapshot: WorktreeSnapshot,
}

pub struct RunArgs {
    pub primary_thread: Option<String>,
    pub reviewer_thread: Option<String>,
    pub primary_worktree: Option<PathBuf>,
    pub reviewer_worktree: Option<PathBuf>,
    pub repository: Option<PathBuf>,
    // existing branch/test/json fields remain
}
```

- [ ] **Step 1: Add failing CLI argument tests**

Add assertions that help lists `worktrees`, one member of either pair fails, and `--json` without all four binding flags fails with `INVALID_ARGUMENTS`. Also assert the complete four-flag form passes argument validation far enough to attempt App Server connection.

```rust
#[test]
fn json_run_requires_complete_thread_and_worktree_pairs() {
    Command::cargo_bin("codex-consensus")
        .unwrap()
        .args(["run", "--json"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("all four binding flags"));
}
```

- [ ] **Step 2: Observe red and implement Clap validation**

Run:

```bash
cargo test -p codex-consensus --test cli json_run_requires_complete_thread_and_worktree_pairs -- --nocapture
```

Expected: the new assertion fails because worktree/repository flags and non-interactive validation do not exist.

Add `Worktrees(WorktreesArgs)` / `WorktreesCommand::List(WorktreeListArgs)` and include it in JSON-output routing. Validate each pair independently. In `main`, additionally reject omitted pairs whenever `--json` is set or stdin is not a TTY; a TTY may prompt for either omitted complete pair.

- [ ] **Step 3: Replace cwd-filtered selection tests**

Delete the test contract that reviewer candidates come from different task cwds. Add tests where both tasks report the same non-Git cwd and are selectable, while worktree selection independently chooses two entries from one repository.

```rust
let threads = vec![thread("primary", shared_non_git), thread("reviewer", shared_non_git)];
let selected = select_task_pair(&threads, &mut selector).unwrap();
assert_eq!(selected.reviewer.id, "reviewer");
```

The selector abstraction must expose separate primary/reviewer task choices, repository path input, primary/reviewer worktree choices, and final mapping confirmation so all logic is unit-testable without a real terminal.

- [ ] **Step 4: Implement independent interactive flow**

Implement the approved order: all visible tasks, distinct reviewer task, repository anchor from `--repository` then current directory if valid then `dialoguer::Input` absolute path, read-only worktree discovery, two distinct worktree choices, and one complete mapping confirmation. Task labels may show `cwd` only as metadata. Worktree labels show path, ref/detached, short SHA, and clean/dirty/unavailable state.

- [ ] **Step 5: Implement explicit binding and discovery command**

Change explicit selection to read the two task IDs only and call `inspect_registered_pair` on the two explicit worktree paths. Do not inspect either `ThreadDetail.summary.cwd`. Add:

```bash
codex-consensus worktrees list --repository /repo --json
```

JSON returns `{ "worktrees": [...] }`; human output uses the same worktree labels. Freeze the resulting `SelectedBinding` into the unchanged `RunFacts` fields.

- [ ] **Step 6: Verify and commit**

Run:

```bash
cargo test -p codex-consensus
cargo test -p consensus-core --test git_inspector
cargo fmt --all --check
```

Expected: all pass.

Commit:

```bash
git add crates/cli/src/args.rs crates/cli/src/select.rs crates/cli/src/main.rs crates/cli/tests/cli.rs
git commit -m "feat: bind tasks to explicit worktrees in the cli"
```

---

## Task 3: Expose Worktree Binding Through MCP

**Files:**

- Modify: `crates/mcp-server/src/tools.rs`
- Modify: `crates/mcp-server/tests/mcp.rs`
- Modify: `crates/cli/src/main.rs`
- Modify: `crates/cli/tests/cli.rs`

- [ ] **Step 1: Add failing seven-tool contract tests**

Update the exact tool order to:

```text
consensus_doctor
consensus_list_threads
consensus_list_worktrees
consensus_start
consensus_status
consensus_resume
consensus_cancel
```

Assert `consensus_list_worktrees` requires one non-empty `repository_path`, and `consensus_start` requires `primary_thread`, `reviewer_thread`, `primary_worktree`, and `reviewer_worktree`.

- [ ] **Step 2: Run focused MCP test and observe red**

Run:

```bash
cargo test -p consensus-mcp-server --test mcp initializes_and_lists_exactly_the_seven_public_tools -- --nocapture
```

Expected: the test fails because only six definitions exist.

- [ ] **Step 3: Implement schemas and strict validation**

Change `MCP_TOOL_NAMES` to length seven, add `WorktreeListArguments`, add both worktree strings to `StartArguments`, deny unknown fields as before, and validate all strings plus distinct task IDs and distinct worktree strings before backend dispatch.

```rust
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorktreeListArguments {
    repository_path: String,
}
```

- [ ] **Step 4: Connect CLI MCP backend**

Decode `consensus_list_worktrees` and call the same `list_worktrees_value` used by the CLI command. Decode the two required worktree paths for `consensus_start` and populate `RunArgs`; no MCP path may enter interactive selection.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo test -p consensus-mcp-server
cargo test -p codex-consensus
cargo fmt --all --check
```

Expected: all pass and hidden MCP stdio help returns seven tools.

Commit:

```bash
git add crates/mcp-server/src/tools.rs crates/mcp-server/tests/mcp.rs crates/cli/src/main.rs crates/cli/tests/cli.rs
git commit -m "feat: expose explicit worktree binding over mcp"
```

---

## Task 4: Remove Task-Cwd Identity Checks and Strengthen Binding Prompts

**Files:**

- Modify: `crates/daemon/src/coordinator.rs`
- Modify: `crates/daemon/tests/coordinator.rs`
- Modify: `crates/core/src/prompts.rs`
- Modify: `crates/core/tests/prompts.rs`

- [ ] **Step 1: Add failing same/non-Git cwd coordinator test**

Make the fake App Server return the same `/unrelated/non-git/task-home` cwd for primary and reviewer. Assert a complete coordinator run still dispatches primary turns at `/repo/primary`, reviewer turns at `/repo/reviewer`, and verification at the detached clone from `RunFacts`/run state.

- [ ] **Step 2: Run focused daemon test and observe red**

Run:

```bash
cargo test -p consensus-daemon --test coordinator task_cwd_is_metadata_and_bound_worktrees_drive_turns -- --nocapture
```

Expected: the current `verify_thread_worktree` path check rejects the non-Git cwd or the new policy assertion fails.

- [ ] **Step 3: Remove cwd identity from repository safety**

Delete `RepositorySafety::verify_thread_worktree`, its `GitRepositorySafety` implementation, and the call in `verify_thread_identity`. Keep the exact returned-task-ID check. Preserve `verify_frozen` before turns and all existing explicit `turn_execution_policy` worktree/root policies.

Map a frozen worktree that becomes missing/inaccessible to public `WORKTREE_UNAVAILABLE`; preserve `SOURCE_DRIFT` for path identity, HEAD, or ref movement.

- [ ] **Step 4: Add binding-mismatch prompt test and instruction**

Assert every first-role prompt states the role-specific bound path/ref/SHA, instructs inspection only at the supplied execution cwd, and requires a `BLOCKED` response with `reason_code: "SOURCE_BINDING_MISMATCH"` when history and source do not correspond. It must explicitly forbid searching for or switching to another source directory.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo test -p consensus-core --test prompts
cargo test -p consensus-daemon --test coordinator
cargo test -p consensus-daemon
cargo fmt --all --check
```

Expected: all pass; policy assertions prove task-reported cwd has no authority.

Commit:

```bash
git add crates/daemon/src/coordinator.rs crates/daemon/tests/coordinator.rs crates/core/src/prompts.rs crates/core/tests/prompts.rs
git commit -m "fix: execute tasks from frozen worktree bindings"
```

---

## Task 5: Prove the Process Boundary with Same-Cwd Fake Tasks

**Files:**

- Modify: `tests/fake-app-server/src/main.rs`
- Modify: `tests/e2e/tests/acceptance.rs`

- [ ] **Step 1: Add failing acceptance setup**

Add explicit `primary_thread_cwd` and `reviewer_thread_cwd` fields to fake configuration and set both to one non-Git directory in `AcceptanceFixture`. Pass both explicit worktree flags in every `run` invocation. Record `thread/read` cwd metadata and retain existing strict `turn/start` cwd/runtime-root checks against frozen source paths.

- [ ] **Step 2: Observe the process-level failure**

Run:

```bash
cargo test -p consensus-e2e --test acceptance accepted_happy_path_preserves_both_sources -- --nocapture
```

Expected before implementation: start fails because CLI derives sources from fake task cwd or required worktree flags/config fields are absent.

- [ ] **Step 3: Implement fake-server metadata separation**

Return configured task cwd values from `thread/list` and `thread/read`, but continue validating each `turn/start` policy against `primary_worktree`, `reviewer_worktree`, or `verification_worktree`. This proves source execution does not depend on task registration cwd.

- [ ] **Step 4: Add explicit regression assertions**

Assert both fake summaries use the same non-Git cwd, all primary/reviewer turns use their different bound paths, both frozen source refs remain unchanged, the accepted SHA contains both source SHAs, and no remote/push action occurs.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo test -p consensus-e2e --test acceptance
cargo test -p fake-app-server
cargo fmt --all --check
```

Expected: all acceptance scenarios pass.

Commit:

```bash
git add tests/fake-app-server/src/main.rs tests/e2e/tests/acceptance.rs
git commit -m "test: accept explicit bindings for same-cwd tasks"
```

---

## Task 6: Diagnose the Conflicting Legacy Standalone Skill

**Files:**

- Create: `crates/cli/src/installation.rs`
- Modify: `crates/cli/src/main.rs`
- Modify: `crates/cli/tests/cli.rs`

- [ ] **Step 1: Add failing pure-path diagnostic tests**

Test a temporary effective Codex home with and without `skills/worktree-merge-consensus/SKILL.md`. The detector must return the exact path and `LEGACY_SKILL_CONFLICT`, and must never remove or modify it.

```rust
let conflict = legacy_skill_conflict(temp.path()).unwrap();
assert_eq!(conflict.code(), "LEGACY_SKILL_CONFLICT");
assert!(legacy_skill.exists());
```

- [ ] **Step 2: Implement effective Codex-home resolution**

Resolve `$CODEX_HOME`, otherwise `$HOME/.codex`, without shell expansion. Add a doctor surface enum so direct CLI diagnostics fail with migration guidance when the legacy standalone Skill exists, while a call already executing through the plugin MCP surface can report the plugin as active instead of falsely blocking itself.

Migration guidance must tell the user to back up/remove the legacy standalone directory manually, install matching binary/plugin release versions, then restart Codex or open a new task. Do not automate those mutations.

- [ ] **Step 3: Wire both doctors and verify**

Use the direct surface in `codex-consensus doctor` and the plugin-active surface in `consensus_doctor`. Include a machine-readable `legacy_skill`/`plugin_surface` diagnostic in successful MCP output.

Run:

```bash
cargo test -p codex-consensus installation -- --nocapture
cargo test -p codex-consensus
cargo fmt --all --check
```

Expected: all pass and the temporary legacy file remains intact.

Commit:

```bash
git add crates/cli/src/installation.rs crates/cli/src/main.rs crates/cli/tests/cli.rs
git commit -m "feat: diagnose legacy skill conflicts"
```

---

## Task 7: Update the Plugin Contract and User Documentation

**Files:**

- Modify: `plugin/skills/worktree-merge-consensus/SKILL.md`
- Modify: `plugin/skills/worktree-merge-consensus/references/protocol.md`
- Modify: `README.md`
- Modify: `README.zh-CN.md`
- Modify: `docs/protocol-v1.md`
- Modify: `docs/compatibility.md`
- Modify: `docs/real-codex-smoke-test.md`
- Modify: `SECURITY.md`
- Modify: `tests/docs.sh`

- [ ] **Step 1: Tighten documentation checks first**

Change `tests/docs.sh` to require the `worktrees` command group, seven MCP tools, both explicit worktree flags in English/Chinese/script examples, the no-cwd-inference contract, legacy conflict migration, matching plugin/binary versions, and restart/new-task guidance.

- [ ] **Step 2: Run docs test and observe red**

Run:

```bash
bash tests/docs.sh
```

Expected: it fails because current docs still describe six tools/commands and thread-cwd-derived selection.

- [ ] **Step 3: Update launcher Skill and protocol reference**

The launcher sequence must be exactly: doctor, list all tasks, assign distinct task roles, collect repository anchor, list registered worktrees, assign distinct source roles, show/freeze complete mapping, start with four required fields, report run ID, and stop. It must not ask users to create tasks in particular directories or infer a source from task cwd.

- [ ] **Step 4: Update public docs**

Document interactive and non-interactive CLI use, `worktrees list`, same/non-Git task cwd support, immutable bindings, exact error meanings, local-only final branch, legacy Skill migration, release-version matching, and restart behavior. Update the real smoke command with:

```bash
--primary-worktree /gpfs/users/i-zhangguoqiang/workspace/gh_testtest \
--reviewer-worktree /gpfs/users/i-zhangguoqiang/workspace/gh_testtest/.worktrees/feature-expansion
```

Do not claim the real smoke test has passed until it is actually run on basestream.

- [ ] **Step 5: Validate the Skill and docs, then commit**

Run:

```bash
python3 /Users/zitago/.codex/skills/.system/skill-creator/scripts/quick_validate.py plugin/skills/worktree-merge-consensus
bash tests/docs.sh
bash tests/release-gate.sh
```

Expected: all pass.

Commit:

```bash
git add plugin README.md README.zh-CN.md docs/protocol-v1.md docs/compatibility.md docs/real-codex-smoke-test.md SECURITY.md tests/docs.sh
git commit -m "docs: explain explicit task worktree binding"
```

---

## Task 8: Full Qualification, Review, and Final Local Commit

**Files:**

- Review all files changed since `b65f1f57268aaa4c53933e3bc5065f84715ec58d`
- Modify only files required by findings

- [ ] **Step 1: Run formatting and static checks**

Run:

```bash
cargo fmt --all --check
cargo check --locked --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all exit 0 with no warnings.

- [ ] **Step 2: Run the complete automated suite**

Run:

```bash
cargo test --workspace
bash tests/docs.sh
bash tests/release-gate.sh
```

Expected: all pass, including real-Git and process-level fake App Server acceptance tests.

- [ ] **Step 3: Audit the implementation against the approved design**

Inspect the final diff and search for prohibited inference and stale public counts:

```bash
git diff b65f1f57268aaa4c53933e3bc5065f84715ec58d...HEAD --check
rg -n "summary\.cwd|inspect_worktree\(&.*cwd|six MCP|六个 MCP|SAME_WORKTREE|DIFFERENT_REPOSITORY" crates tests plugin README.md README.zh-CN.md docs
```

Expected: `summary.cwd` remains only display metadata/test setup; no source binding or daemon identity logic consumes it. No stale six-tool wording or old pair-level reason code remains.

- [ ] **Step 4: Perform an independent cold review**

Review security boundaries, Git command allowlist, path canonicalization, porcelain parsing, JSON/non-interactive no-prompt behavior, MCP schema/backend parity, daemon policies, source-drift revalidation, and legacy-file non-mutation. Add a failing regression test before fixing every substantive finding.

- [ ] **Step 5: Commit review fixes if needed**

```bash
git add <only-reviewed-files>
git commit -m "fix: address explicit binding review findings"
```

Skip this commit when the review finds no changes.

- [ ] **Step 6: Record final local state**

Run:

```bash
git status --short --branch
git log --oneline --decorate -10
```

Expected: clean `feat/initial-implementation`; no push, PR, remote integration, server installation, or merge into any existing branch has occurred. The next separately authorized step is packaging/installing the matching binary and plugin on basestream, manually resolving the legacy Skill conflict, restarting Codex, and executing the documented real smoke test against the ordinary `Zita-Go/eva` fork.
