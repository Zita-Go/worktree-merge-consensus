# Worktree Merge Consensus

[简体中文](README.zh-CN.md)

`worktree-merge-consensus` coordinates a reviewed integration between two
existing Codex tasks whose committed changes live in separate worktrees of the
same Git repository. The primary task proposes and writes the integration; the
reviewer task checks that its own behavior and implementation details survive.
The result stops on a new local branch.

> **Experimental dependency:** this project uses the experimental Codex App
> Server protocol. Version 0.1 supports Codex CLI `>=0.144.1`. App Server
> identity, required-method, and response-shape mismatches still fail closed.
> Run `codex-consensus doctor` before starting a real integration.

## Safety model

The coordinator freezes both task IDs, worktree paths, source refs, and commit
SHAs before review. It then enforces this sequence:

1. Both tasks independently state behavior, constraints, tests, and protected
   implementation details.
2. The primary proposes a coverage plan.
3. The reviewer requests concrete changes until it approves the exact plan
   revision.
4. Only the primary creates a unique new local integration branch and combines
   the two frozen commits.
5. The coordinator materializes a clean, detached, remote-free clone of the
   exact result SHA. A separate primary verification turn runs every frozen
   test there, while the coordinator derives evidence from App Server command
   execution items and checks Git invariants.
6. The reviewer audits the exact resulting SHA; acceptance is recorded only for
   that SHA.

The `same-host` and `no-push` contracts are deliberate. Both tasks, both
worktrees, the Git common directory, the Codex App Server, and the coordinator
must be on one host. The coordinator does not push, open a pull request, merge
into an existing branch, update either source ref, rebase, reset, delete, or
clean up worktrees.

This is enforced beyond prompt text: review turns are read-only and offline;
the primary integration turn is offline, has bounded source-repository writable
roots, and can run only a narrow Git command set. The separate verification
turn can write only inside the isolated clone and can run only the exact frozen
test commands. Each command must appear exactly once as a successful App Server
`commandExecution` item with the expected cwd; a model's self-reported success
is not evidence. Deterministic approval rules cancel publication, destructive
Git, shell chaining, wrong-directory commands, and permission escalation.
Conflict scanning uses Git's actual primary-to-result diff, including large
text files, rather than the task's file list.

Read [the v1 protocol](docs/protocol-v1.md),
[compatibility policy](docs/compatibility.md), and [security policy](SECURITY.md)
for the exact boundaries.

## Preconditions

- Linux x86_64 or ARM64 for released binaries. Other Unix systems may work for
  development but are not release targets in v0.1.
- Git and Codex CLI available in `PATH`.
- Codex CLI `>=0.144.1` with the required experimental App Server methods.
- Exactly two existing Codex tasks under the same local account and host.
- Each task has a different worktree in the same Git common directory.
- Both implementations are committed and both worktrees are clean. A detached
  source HEAD is allowed because identity is frozen by SHA; the result is still
  created on a new attached local branch.

## Install the standalone binary

Download the archive for `x86_64-unknown-linux-gnu` or
`aarch64-unknown-linux-gnu` from this repository's GitHub Release. Download
`SHA256SUMS` as well, then verify every downloaded asset before extracting it:

```bash
sha256sum --check SHA256SUMS
tar -xzf codex-consensus-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
install -m 0755 codex-consensus-v0.1.0-x86_64-unknown-linux-gnu/codex-consensus ~/.local/bin/codex-consensus
```

Release assets also include a CycloneDX JSON SBOM and the Codex plugin bundle.
Until the real-Codex checklist is recorded, v0.1 releases are marked
pre-release; see [the smoke-test record](docs/real-codex-smoke-test.md).

To build from a source checkout instead:

```bash
cargo install --locked --path crates/cli
```

Rust 1.85 or newer is required to build the workspace.

## Install the Codex plugin

The `codex-consensus` binary must already be in `PATH`. From a source checkout,
register this repository as a local marketplace and install its plugin:

```bash
codex plugin marketplace add /absolute/path/to/worktree-merge-consensus
codex plugin add worktree-merge-consensus@worktree-merge-consensus
```

If you downloaded the plugin archive, extract it and register the directory
that contains `.agents/plugins/marketplace.json`. Restart Codex after plugin
installation. In a Codex task, invoke `$worktree-merge-consensus`; the Skill
uses six MCP tools only to launch and control the persistent coordinator. It
does not relay review turns through a third agent.

## Use the CLI

Check the environment first:

```bash
codex-consensus doctor
codex-consensus threads list
```

Start interactively to choose the primary and reviewer from local tasks:

```bash
codex-consensus run
```

For scripts, provide both task IDs. The branch flag is optional; without it the
coordinator reserves `consensus/<run-id>`. Every `--test` value is an exact
direct command the primary must run during the isolated verification turn.
Git commands, shell control operators, and dynamic shell/interpreter launchers
are rejected. For composed checks, invoke a committed test script directly.

```bash
codex-consensus run \
  --primary-thread THREAD_ID_A \
  --reviewer-thread THREAD_ID_B \
  --integration-branch consensus/my-integration \
  --test "cargo test --workspace" \
  --json
```

Inspect one run or list all runs:

```bash
codex-consensus status RUN_ID
codex-consensus status --json
```

If a run pauses for an explicit user action, resolve the displayed reason and
resume the same durable run:

```bash
codex-consensus resume RUN_ID
```

Cancel only when the run should stop. Cancellation preserves all Git state,
including any integration branch already created:

```bash
codex-consensus cancel RUN_ID
```

The six public command groups are therefore `codex-consensus doctor`,
`codex-consensus threads`, `codex-consensus run`, `codex-consensus status`,
`codex-consensus resume`, and `codex-consensus cancel`. All support stable
machine-readable JSON at their operational leaf where shown by `--help`.

## Statuses and recovery

| Status | Meaning |
| --- | --- |
| `RUNNING` | The daemon can dispatch the next deterministic step. |
| `WAITING_THREAD` | A selected Codex task already has an active turn. |
| `PAUSED_USER_ACTION` | Explicit task input or another external action is required. Resolve it, then resume. |
| `ACCEPTED` | Tests, source-ref invariants, and reviewer approval all match the exact integration SHA; `accepted_result` records test results and the local-only/no-push boundary. |
| `BLOCKED` | A terminal protocol, safety, round-limit, or no-progress condition stopped the run. |
| `CANCELLED` | The user cancelled; existing Git state was preserved. |
| `INCOMPATIBLE_CODEX` | Codex is outside the checked adapter range or lacks a required method. |

Before each App Server turn, the daemon persists the intended send in SQLite.
After a process restart, the next CLI or MCP request reconnects to the daemon,
which recovers runnable work idempotently. Do not use `resume` for `BLOCKED` or
`CANCELLED` runs. A pending verification turn may leave test artifacts in its
clone; recovery permits that clone to be dirty only while still requiring the
persisted path, exact detached SHA, independent Git common directory, and no
remote.

## State, logs, and privacy

The default state directory is `$XDG_STATE_HOME/codex-consensus`, or
`~/.local/state/codex-consensus` when `XDG_STATE_HOME` is unset. Override it
with the global `--state-dir DIR` option. It contains:

- `state.db`: SQLite run state, frozen Git facts, transitions, and pending-send
  metadata;
- `daemon.sock`: the local Unix socket, mode `0600`;
- `daemon.pid`: the managed daemon process ID.
- `verification/<run-id>-<integration-sha>`: a detached, remote-free clone used
  only for exact-SHA tests. It has a Git common directory independent of both
  source worktrees and is retained for audit/recovery in v0.1.

The directory is mode `0700`. The database stores canonical protocol payloads
and evidence but not full task conversation transcripts or generated prompts;
Codex itself retains messages in the two selected task histories. Sensitive App
Server diagnostics are redacted. The managed daemon does not create a
persistent log file by default, so CLI output, Codex task history, and
`status --json` are the operational record.

## Troubleshooting

- `INCOMPATIBLE_CODEX`: confirm `codex --version`, then compare it with
  [the compatibility policy](docs/compatibility.md). Versions below `0.144.1`,
  malformed output, and App Server identity/method/shape mismatches fail closed.
- `INCOMPATIBLE_STATE`: a prerelease database has a missing or unknown run-state
  schema. Preserve it for audit and use a fresh `--state-dir`; do not edit
  SQLite manually.
- `DIRTY_WORKTREE`: commit or intentionally remove local changes in both source
  worktrees before starting a new run.
- `INTEGRATION_BRANCH_EXISTS`: choose a new branch name. Existing branches are
  never reused or deleted.
- `SOURCE_DRIFT`: a frozen source ref or worktree HEAD changed. Inspect the Git
  state and start a new run with newly frozen commits.
- `PERMISSION_REQUIRED`: answer the pending explicit task-input request in the
  relevant task, then use `codex-consensus resume RUN_ID`. Command or permission
  escalation is denied instead and ends as `FORBIDDEN_OPERATION`.
- `NO_PROGRESS` or `ROUND_LIMIT`: the run is terminal. Review the contracts and
  start a new run rather than forcing acceptance.
- Daemon startup failure: check ownership and permissions of the state
  directory, remove no files automatically, and retry `codex-consensus doctor`
  with `--state-dir` if isolation is needed.

## Non-goals for v0.1

- Cross-host or cross-account task communication.
- More than two tasks per run.
- Reading hidden context from another task outside normal App Server history.
- Pushing, opening a PR, merging into a target branch, or choosing a deployment
  baseline.
- Reusing, overwriting, deleting, or cleaning source/integration branches and
  worktrees.
- Replacing human review for security-sensitive or production releases.

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash tests/docs.sh
bash tests/release-gate.sh
```

The end-to-end suite uses a process-level fake App Server and disposable Git
repositories. A real Codex release still requires the separate
[smoke-test checklist](docs/real-codex-smoke-test.md).

Licensed under [Apache License 2.0](LICENSE).
