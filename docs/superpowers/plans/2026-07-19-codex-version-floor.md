# Unbounded Codex Version Floor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Accept every exact Codex CLI semantic version greater than or equal to `0.144.1`, with no maximum version, while preserving runtime App Server safety checks.

**Architecture:** Keep the compatibility decision in `app-server-client::compat`, but simplify its checked-in fixture to a minimum version plus the required method contract. The executable version remains bound to the managed App Server identity, and typed initialization/method/response validation remains unchanged. User-facing documentation and its executable checks become the release contract for the unbounded range.

**Tech Stack:** Rust 2024, `semver`, Serde/JSON, process-level fake Codex tests, Bash documentation checks.

## Global Constraints

- The Codex CLI compatibility rule is exactly `>=0.144.1`, with no maximum version.
- `0.144.1` is accepted; `0.144.0` and `0.144.1-beta.1` are rejected under semantic-version ordering.
- Parse failures remain `INCOMPATIBLE_CODEX` failures.
- The managed App Server user-agent version must exactly match `codex --version`.
- Required methods, typed response decoding, and role-specific sandbox-policy checks remain fail-closed.
- Rename the adapter contract to `schemas/app-server/supported-methods.json`; do not retain a misleading version-specific fixture filename.
- Do not modify the consensus protocol, Git integration workflow, publication boundary, or historical 2026-07-18 design/plan records.
- Do not push, create a pull request, or merge into an existing branch.

---

## File Map

- `crates/app-server-client/tests/compat.rs` — direct semantic-version boundary tests and fixture contract assertions.
- `crates/app-server-client/tests/process.rs` — proves the lower boundary is enforced before App Server startup.
- `crates/app-server-client/src/compat.rs` — parses the minimum-only fixture and applies the version floor.
- `schemas/app-server/supported-methods.json` — open-ended compatibility/capability contract.
- `schemas/app-server/0.144.5-methods.json` — removed after the replacement fixture is wired in.
- `tests/docs.sh` — executable checks for the published compatibility wording and fixture path.
- `README.md` and `README.zh-CN.md` — English and Chinese installation/runtime requirements.
- `docs/compatibility.md` — detailed open-ended compatibility and upgrade policy.
- `docs/real-codex-smoke-test.md` — smoke-test eligibility and prerelease caveat.
- `SECURITY.md` — precise runtime fail-closed boundary after future versions are admitted.

### Task 1: Change the executable compatibility gate

**Files:**
- Modify: `crates/app-server-client/tests/compat.rs`
- Modify: `crates/app-server-client/tests/process.rs`
- Modify: `crates/app-server-client/src/compat.rs`
- Create: `schemas/app-server/supported-methods.json`
- Delete: `schemas/app-server/0.144.5-methods.json`

**Interfaces:**
- Consumes: `check_compatibility(version_output: &str) -> CompatibilityReport` and `CodexAppServer::connect(ConnectOptions)`.
- Produces: the same public Rust interfaces, with compatibility defined by a minimum-only `MethodFixture { minimum_version, required_methods }`.

- [ ] **Step 1: Write failing semantic-version boundary tests**

Replace the old bounded-range cases in `crates/app-server-client/tests/compat.rs` with these exact behaviors. Keep the existing fixture include unchanged until the replacement fixture is created in Step 4:

```rust
#[test]
fn minimum_supported_codex_version_passes() {
    let report = check_compatibility("codex-cli 0.144.1");

    assert!(report.compatible);
    assert_eq!(report.installed_version.as_deref(), Some("0.144.1"));
}

#[test]
fn future_codex_versions_have_no_version_ceiling() {
    let report = check_compatibility("codex-cli 1.0.0");

    assert!(report.compatible);
    assert_eq!(report.installed_version.as_deref(), Some("1.0.0"));
}

#[test]
fn version_below_minimum_is_rejected() {
    let report = check_compatibility("codex-cli 0.144.0");

    assert!(!report.compatible);
    assert_eq!(report.reason_code.as_deref(), Some("INCOMPATIBLE_CODEX"));
}

#[test]
fn prerelease_of_minimum_version_is_rejected() {
    let report = check_compatibility("codex-cli 0.144.1-beta.1");

    assert!(!report.compatible);
    assert_eq!(report.reason_code.as_deref(), Some("INCOMPATIBLE_CODEX"));
}
```

- [ ] **Step 2: Write failing process-boundary tests**

In `crates/app-server-client/tests/process.rs`, make the compatible fake binary report `0.144.1`, make the incompatible fake binary report `0.144.0`, and keep the exact App Server identity mismatch test. The key inputs must be:

```rust
let binary = fake_codex(temp.path(), &log, "0.144.1");
```

and:

```rust
let binary = fake_codex(temp.path(), &log, "0.144.0");
```

- [ ] **Step 3: Run the focused tests and verify RED**

Run:

```bash
cargo test -p app-server-client --test compat --test process
```

Expected: FAIL because `0.144.1` and `1.0.0` are rejected by the old bounded gate. The failure must be caused by the old behavior, not a Rust compilation or syntax error.

- [ ] **Step 4: Replace the version-specific fixture**

Create `schemas/app-server/supported-methods.json` with the complete contract:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "fixtureVersion": "minimum-0.144.1",
  "protocolFamily": "codex-app-server/experimental-v2",
  "minimumVersion": "0.144.1",
  "requiredMethods": [
    "initialize",
    "thread/list",
    "thread/read",
    "thread/resume",
    "turn/start"
  ],
  "requiredNotifications": [
    "thread/status/changed",
    "turn/started",
    "turn/completed"
  ],
  "requestShape": {
    "initialize": ["clientInfo"],
    "thread/list": [],
    "thread/read": ["threadId", "includeTurns"],
    "thread/resume": ["threadId"],
    "turn/start": [
      "threadId",
      "input",
      "outputSchema",
      "cwd",
      "runtimeWorkspaceRoots",
      "approvalPolicy",
      "approvalsReviewer",
      "environments",
      "sandboxPolicy"
    ]
  },
  "initializeResponseShape": [
    "codexHome",
    "platformFamily",
    "platformOs",
    "userAgent"
  ],
  "turnPolicyShape": {
    "readOnly": ["type", "networkAccess"],
    "workspaceWrite": [
      "type",
      "writableRoots",
      "networkAccess",
      "excludeSlashTmp",
      "excludeTmpdirEnvVar"
    ]
  },
  "turnPolicyProfiles": {
    "reviewReadOnly": {
      "approvalPolicy": "never",
      "networkAccess": false
    },
    "primaryIntegrationWorkspaceWrite": {
      "approvalPolicy": "untrusted",
      "writableRootRoles": ["primaryWorktree", "sourceGitCommonDirectory"],
      "networkAccess": false,
      "excludeSlashTmp": true,
      "excludeTmpdirEnvVar": true
    },
    "primaryVerificationWorkspaceWrite": {
      "approvalPolicy": "untrusted",
      "writableRootRoles": ["isolatedVerificationClone"],
      "networkAccess": false,
      "excludeSlashTmp": false,
      "excludeTmpdirEnvVar": false
    }
  },
  "commandExecutionEvidenceShape": {
    "required": ["id", "type", "command", "cwd", "status", "exitCode"],
    "type": "commandExecution",
    "successfulStatus": "completed",
    "successfulExitCode": 0,
    "agentSourceWhenPresent": "agent"
  }
}
```

Remove `schemas/app-server/0.144.5-methods.json` after all includes point to the replacement.

- [ ] **Step 5: Implement the minimum-only gate**

In `crates/app-server-client/src/compat.rs`, use:

```rust
const METHOD_FIXTURE: &str =
    include_str!("../../../schemas/app-server/supported-methods.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MethodFixture {
    minimum_version: String,
    required_methods: Vec<String>,
}
```

Delete maximum-version parsing. Replace the bounded comparison with:

```rust
if installed < minimum {
    return incompatible(
        Some(&installed),
        vec![],
        format!("supported versions are >= {minimum}"),
    );
}
```

Do not change `parse_codex_version`, required-method equality, managed identity validation, or typed App Server calls.

Also change the fixture include in the write-root test to:

```rust
let fixture: Value = serde_json::from_str(include_str!(
    "../../../schemas/app-server/supported-methods.json"
))
.unwrap();
```

- [ ] **Step 6: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p app-server-client --test compat --test process
```

Expected: all compatibility and process tests pass, including the new minimum, future-version, below-minimum, and prerelease cases.

- [ ] **Step 7: Commit the executable gate**

```bash
git add crates/app-server-client/src/compat.rs crates/app-server-client/tests/compat.rs crates/app-server-client/tests/process.rs schemas/app-server
git commit -m "fix: accept Codex 0.144.1 and newer"
```

### Task 2: Publish and enforce the open-ended compatibility contract

**Files:**
- Modify: `tests/docs.sh`
- Modify: `README.md`
- Modify: `README.zh-CN.md`
- Modify: `docs/compatibility.md`
- Modify: `docs/real-codex-smoke-test.md`
- Modify: `SECURITY.md`

**Interfaces:**
- Consumes: the Task 1 fixture path and minimum-only runtime behavior.
- Produces: executable documentation checks that reject stale bounded-range claims in current user-facing documentation.

- [ ] **Step 1: Tighten the documentation test first**

In `tests/docs.sh`, add `schemas/app-server/supported-methods.json` to `required_files`, replace the README marker loop with:

```bash
for marker in same-host '>=0.144.1' no-push SHA256SUMS; do
  grep -Fq "$marker" "$readme" || fail "$readme is missing the $marker contract"
done
```

Then add:

```bash
[[ ! -e schemas/app-server/0.144.5-methods.json ]] ||
  fail 'obsolete version-specific App Server fixture still exists'

for document in README.md README.zh-CN.md docs/compatibility.md docs/real-codex-smoke-test.md; do
  grep -Fq '>=0.144.1' "$document" || fail "$document is missing the Codex version floor"
  if grep -Fq '<0.145.0' "$document"; then
    fail "$document still documents an obsolete Codex version ceiling"
  fi
done
```

- [ ] **Step 2: Run the docs test and verify RED**

Run:

```bash
bash tests/docs.sh
```

Expected: FAIL because the current README files and compatibility documents still publish `>=0.144.5, <0.145.0` after Task 1 has changed the executable contract.

- [ ] **Step 3: Update current user-facing documentation**

Use these exact current-policy statements:

`README.md`:

```markdown
> **Experimental dependency:** this project uses the experimental Codex App
> Server protocol. Version 0.1 supports Codex CLI `>=0.144.1`. App Server
> identity, required-method, and response-shape mismatches still fail closed.
> Run `codex-consensus doctor` before starting a real integration.
```

Its precondition becomes:

```markdown
- Codex CLI `>=0.144.1` with the required experimental App Server methods.
```

`README.zh-CN.md`:

```markdown
> **实验性依赖：** 本项目使用实验性的 Codex App Server 协议。v0.1 支持
> Codex CLI `>=0.144.1`；App Server 身份、必需方法或响应结构不匹配时仍会失败关闭。
> 真实集成前请先运行 `codex-consensus doctor`。
```

Its precondition becomes:

```markdown
- Codex CLI `>=0.144.1`，且提供所需的实验性 App Server 方法。
```

In `docs/compatibility.md`, set the table value to `>=0.144.1`, link
`schemas/app-server/supported-methods.json`, and replace the bounded gate text
with:

```markdown
The executable parses an exact semantic version from `codex --version` before
starting `codex app-server daemon start` and `codex app-server proxy`. An
unparseable version or a version below `0.144.1` returns
`INCOMPATIBLE_CODEX`. There is no maximum-version gate. The managed App Server
identity, required methods, and typed response shapes are still validated at
runtime and fail closed on mismatch.
```

Refer to policy fields as part of the checked-in `supported-methods` fixture,
not as 0.144.5-only shapes.

Replace the upgrade section with an open-ended policy: the version gate admits
new versions automatically, while any observed adapter incompatibility must be
fixed through a reviewed fixture/update, boundary and handshake tests, the
complete fake-App-Server E2E suite, and a recorded real-Codex smoke run.

In `docs/real-codex-smoke-test.md`, use:

```markdown
No disposable real-Codex run has yet been recorded for the supported Codex CLI
range beginning at `0.144.1`.
```

and:

```markdown
- Exact `codex --version` output satisfying `>=0.144.1`.
```

Retain every `NOT_RECORDED` field and the prerelease promotion rule.

In `SECURITY.md`, replace the stale unknown-version statement with:

```markdown
Malformed or missing required App Server responses and unknown persisted-state
versions fail closed.
```

Leave the historical 2026-07-18 design and implementation-plan records
unchanged; the 2026-07-19 design supersedes only their old compatibility range.

- [ ] **Step 4: Run documentation checks and verify GREEN**

Run:

```bash
bash tests/docs.sh
```

Expected: `documentation checks passed`.

- [ ] **Step 5: Commit the published contract**

```bash
git add README.md README.zh-CN.md SECURITY.md docs/compatibility.md docs/real-codex-smoke-test.md tests/docs.sh
git commit -m "docs: publish unbounded Codex compatibility"
```

### Task 3: Full regression and release verification

**Files:**
- Verify only: entire workspace and current branch.

**Interfaces:**
- Consumes: Tasks 1 and 2.
- Produces: fresh evidence that the compatibility change did not weaken unrelated consensus, Git, state, or release behavior.

- [ ] **Step 1: Format and compile on both supported Rust toolchains**

Run:

```bash
cargo fmt --all --check
cargo check --locked --workspace --all-targets
cargo +1.85.0 check --locked --workspace --all-targets
```

Expected: every command exits 0.

- [ ] **Step 2: Run lint and the complete test matrix**

Run:

```bash
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Expected: Clippy reports no warnings and every unit, integration, MCP, plugin, and E2E test passes.

- [ ] **Step 3: Run documentation and release gates**

Run:

```bash
bash tests/docs.sh
bash tests/release-gate.sh
CODEX_CONSENSUS_BIN=target/debug/codex-consensus bash tests/release.sh v0.1.0
git diff --check
```

Expected: both documentation/release scripts report success, release version checks pass for `v0.1.0`, and Git reports no whitespace errors.

- [ ] **Step 4: Confirm publication boundaries and branch state**

Run:

```bash
git status --short --branch
git log -4 --oneline --decorate
git remote -v
```

Expected: the worktree is clean, the branch is `feat/initial-implementation`, and no push, PR, or merge into an existing branch has occurred.
