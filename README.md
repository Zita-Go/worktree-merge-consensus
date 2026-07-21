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
is not evidence. Frozen tests must be direct non-Git commands; Git executables,
shell control, and dynamic shell or interpreter launchers are rejected.
Deterministic approval rules cancel publication, destructive Git, shell
chaining, wrong-directory commands, and permission escalation.
Conflict scanning uses Git's actual primary-to-result diff, including large
text files, rather than the task's file list.

Version 0.1.23 also supports Linux containers whose security policy prevents
Codex's bwrap-backed file-change helper from starting. The Primary participant
may use `consensus_apply_patch` only for the exact active Run and request hash,
after the authorized branch is clean and contains both frozen commits. The
daemon accepts one successful text-only patch of at most 512 KiB, preflights it
with Git without unsafe paths, revalidates both source refs, and records the
single-use result in SQLite. This capability has no public CLI equivalent and
cannot select a repository, create a branch, start a Run, or publish anything.

Read [the v1 protocol](docs/protocol-v1.md),
[compatibility policy](docs/compatibility.md), and [security policy](SECURITY.md)
for the exact boundaries.

## Preconditions

- Linux x86_64 or ARM64 for released binaries. Other Unix systems may work for
  development but are not release targets in v0.1.
- Git and Codex CLI available in `PATH`.
- Codex CLI `>=0.144.1` with the required experimental App Server methods.
- Exactly two existing Codex tasks under the same local account and host.
- Two different registered source worktrees in the same Git common directory,
  selected independently from the tasks. A task cwd is display metadata only;
  both tasks may report the same cwd or a directory outside Git.
- Both implementations are committed and both worktrees are clean. A detached
  source HEAD is allowed because identity is frozen by SHA; the result is still
  created on a new attached local branch.

## Install the standalone binary

Download the static musl archive for `x86_64-unknown-linux-musl` or
`aarch64-unknown-linux-musl` from this repository's GitHub Release. These
binaries do not depend on the host's GLIBC version. Download `SHA256SUMS` as
well, then verify every downloaded asset before extracting it:

```bash
sha256sum --check SHA256SUMS
tar -xzf codex-consensus-v0.1.23-x86_64-unknown-linux-musl.tar.gz
install -m 0755 codex-consensus-v0.1.23-x86_64-unknown-linux-musl/codex-consensus ~/.local/bin/codex-consensus
```

The v0.1.0 GNU archives require GLIBC 2.39 and are superseded. Use v0.1.1 or
later on supported Linux hosts.

Release assets also include a CycloneDX JSON SBOM and the Codex plugin bundle.
Until the real-Codex checklist is recorded, v0.1 releases are marked
pre-release; see [the smoke-test record](docs/real-codex-smoke-test.md).

To build from a source checkout instead:

```bash
cargo install --locked --path crates/cli
```

Rust 1.85 or newer is required to build the workspace.

## Install the Codex plugin

The `codex-consensus` binary must already be in `PATH`. The binary/plugin
artifacts must come from the same release. From a source checkout, register
this repository as a local marketplace and install its plugin:

```bash
codex plugin marketplace add /absolute/path/to/worktree-merge-consensus
codex plugin add worktree-merge-consensus@worktree-merge-consensus
```

If you downloaded the plugin archive, extract it and register the directory
that contains `.agents/plugins/marketplace.json`. Restart Codex after plugin
installation or update, then restart Codex or open a new task. In a Codex task,
invoke `$worktree-merge-consensus`; the plugin exposes eight MCP tools. Seven,
including `consensus_list_worktrees`, launch and control the persistent
coordinator. The eighth, `consensus_apply_patch`, is a participant-only,
request-bound write capability described below. It does not relay review turns
through a third agent.

Names such as `consensus_doctor` are MCP tool names, not shell executables.
Codex starts the plugin server as `codex-consensus mcp-server`; the equivalent
terminal diagnostic is `codex-consensus doctor`. Do not run
`command -v consensus_doctor`.

If `codex-consensus doctor` reports `LEGACY_SKILL_CONFLICT`, an older manually
installed `$CODEX_HOME/skills/worktree-merge-consensus` is shadowing the plugin
workflow. Back it up or remove it manually, reinstall matching binary/plugin
versions, and restart Codex or open a new task. The tool never deletes it.

## Use the CLI

Check the environment first:

```bash
codex-consensus doctor
codex-consensus threads list
codex-consensus worktrees list --repository /absolute/path/to/repo --json
```

Start interactively to choose the two task roles and then independently choose
the two registered source worktrees. The task cwd shown in task rows is not a
source binding:

```bash
codex-consensus run
```

For scripts and JSON calls, provide both task IDs and both absolute worktree
paths. The branch flag is optional; without it the coordinator reserves
`consensus/<run-id>`. Every `--test` value is an exact direct command the
primary must run during the isolated verification turn.
Git commands, shell control operators, and dynamic shell/interpreter launchers
are rejected. For composed checks, invoke a committed test script directly.

```bash
codex-consensus run \
  --primary-thread THREAD_ID_A \
  --primary-worktree /repo/.worktrees/change-a \
  --reviewer-thread THREAD_ID_B \
  --reviewer-worktree /repo/.worktrees/change-b \
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

The seven public command groups are therefore `codex-consensus doctor`,
`codex-consensus threads`, `codex-consensus worktrees`, `codex-consensus run`,
`codex-consensus status`, `codex-consensus resume`, and
`codex-consensus cancel`. All support stable machine-readable JSON at their
operational leaf where shown by `--help`.

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
which recovers runnable work idempotently. If the managed App Server restarts
while the coordinator daemon remains alive, the daemon replaces its closed
proxy before retrying idempotent reads; `doctor` probes this daemon-owned
connection as well as a fresh direct connection. A non-idempotent `turn/start`
is never blindly retried after uncertain delivery. On an explicit resume from
`COMMUNICATION_FAILURE`, an exact `failed` or `interrupted` turn is replaced
only when canonical history shows no command, file-change, or unknown
side-effect item; the old attempt remains audited in SQLite. If a contract or
plan declares a forbidden Git test, the run pauses with `INVALID_TEST_COMMAND`.
An explicit resume revalidates both frozen sources, archives the completed
pre-integration read-only turn, and requests one corrected response only when
canonical history contains no file change, incomplete command, or unknown item.
The only retry-safe MCP history is a completed query to this plugin's exact
`consensus_list_threads`, `consensus_list_worktrees`, or `consensus_status`
tool; mutating, external, and unknown MCP calls fail closed. Version 0.1.10 and
later can also recover the equivalent legacy `BLOCKED` state produced by 0.1.9
while atomically reacquiring the repository lock. Version 0.1.12 can similarly
reactivate a pre-integration `BLOCKED / INVALID_RESPONSE` caused by malformed
model output, but only for the exact completed read-only turn and after the same
canonical-history checks. Post-integration and side-effectful invalid responses
remain terminal. Version 0.1.14 explicitly selects the same-host `local`
execution environment for every pinned App Server turn; an empty environment
selection disables command and file tools. It can resume an exact
pre-integration `BLOCKED / EXECUTION_TOOL_UNAVAILABLE` response only after
canonical history, the response hash, source refs, clean worktrees, and target
branch absence jointly prove that no integration side effect occurred. Do not
use `resume` for unrelated `BLOCKED` runs or for a `CANCELLED` run. Version
0.1.15 treats an App Server `proposedExecpolicyAmendment` as non-applied
metadata when the daemon returns the one-time `accept` decision; network and
additional-permission requests are still cancelled. It can also resume the
same run after an exact first-integration `BLOCKED / FORBIDDEN_OPERATION` only
when the denied turn is canonically `failed` or `interrupted`, has no
side-effect-capable item, both worktrees and refs remain frozen and clean, and
the target branch is absent. Version 0.1.16 recognizes the App Server's one
canonical known-shell `-c` or `-lc` wrapper, removes that wrapper exactly once,
and applies the unchanged Git or frozen-test allowlist to the inner command.
Nested shell launchers, subcommand approval callbacks, non-local execution
environments, and added permissions still fail closed. Version 0.1.17 adds only
the exact `git show-ref --verify refs/heads/<target-integration-branch>`
preflight to that integration allowlist. Version 0.1.19 additionally accepts
only `git branch --list <target-integration-branch>` as the equivalent exact
target-existence query; every other `git branch` form remains denied. Same-run
forbidden-operation recovery may retain terminal read-only Git queries only when each canonical
item used the frozen primary cwd and still passes that allowlist. Version 0.1.20
marks coordinator-authored Primary and Reviewer turns as internal participants,
so they do not recursively invoke the launcher skill. Recovery may discard only
the exact legacy `sed -n 1,240p` read of this plugin's versioned `SKILL.md` after
the command was denied; that read remains outside the live execution allowlist.
Version 0.1.21 also recognizes Codex App Server's exact internal
`contextCompaction` marker as retry-safe only when it contains no fields beyond
a nonempty `id` and the fixed `type`. This marker records context lifecycle, not
a command, file change, or tool call. Extra fields, `inProgress`, a write, wrong
cwd, unknown item, or other side effect remains terminal.
Version 0.1.22 permits exactly `rg --files -g AGENTS.md` in the frozen primary
cwd for required repository-instruction discovery. Every other `rg` form stays
denied, and the participant prompt requires subsequent tracked-file inspection
through the existing read-only Git query allowlist.
Version 0.1.23 adds the request-bound `consensus_apply_patch` path described
above. It can also recover the same Run after a communication timeout followed
by an exact completed `BLOCKED / FILE_CHANGE_TOOL_UNAVAILABLE` response, but
only when canonical history, approved plan identity, bwrap permission evidence,
reported merge SHA, the clean authorized target branch, both source ancestors,
and unchanged frozen refs all agree. Recovery retains the existing merge and
archives the failed participant turn; it does not recreate the branch, merge a
second time, or create a replacement Run.
Version 0.1.13 also
places concrete, authoritative, direct-field
payload templates for both approval message types next to the requested output;
the JSON Schema rejects approval identities supplied only in nested objects. A pending verification turn
may leave test artifacts in its clone; recovery permits that clone to be dirty
only while still requiring the persisted path, exact detached SHA, independent
Git common directory, and no remote.

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

- Missing `consensus_*` tools: first run `codex-consensus doctor` in the same
  host environment. A successful result verifies the binary and coordinator,
  but not plugin tool registration. Run `codex mcp list --json` and check that
  `worktreeMergeConsensus` is present and enabled. Reinstall a matching plugin
  version and open a new task when it is absent. Never look for a
  `consensus_doctor` executable. The bundled launcher checks
  `CODEX_CONSENSUS_BIN`, `PATH`, the directory containing `codex`,
  `/usr/local/bin`, and `~/.local/bin` in that order.
- `INCOMPATIBLE_CODEX`: confirm `codex --version`, then compare it with
  [the compatibility policy](docs/compatibility.md). Versions below `0.144.1`,
  malformed output, and App Server identity/method/shape mismatches fail closed.
- `INCOMPATIBLE_STATE`: a prerelease database has a missing or unknown run-state
  schema. Preserve it for audit and use a fresh `--state-dir`; do not edit
  SQLite manually.
- `DIRTY_WORKTREE`: commit or intentionally remove local changes in both source
  worktrees before starting a new run.
- `UNREGISTERED_WORKTREE`, `DUPLICATE_WORKTREE`, or `REPOSITORY_MISMATCH`:
  select two different paths returned by `codex-consensus worktrees list` for
  one repository.
- `WORKTREE_UNAVAILABLE`: a selected frozen worktree is missing or
  inaccessible; restore it and start a new run.
- `SOURCE_BINDING_MISMATCH`: a task determined that the confirmed worktree does
  not contain the implementation represented by its history. Correct the
  mapping and start a new run; `resume` cannot remap frozen identity.
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
