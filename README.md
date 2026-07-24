# Worktree Merge Consensus

[简体中文](README.zh-CN.md)

`worktree-merge-consensus` coordinates a reviewed integration between two
existing Codex tasks whose committed changes live in separate worktrees of the
same Git repository. The primary task proposes and writes the integration; the
reviewer task checks that its own behavior and implementation details survive.
The result stops on a new local branch.

> **Experimental dependency:** this project uses the experimental Codex App
> Server protocol. Version 0.2 supports Codex CLI `>=0.144.1`. App Server
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
   exact result SHA. A separate Primary verification turn returns only a ready
   marker; the coordinator then runs every frozen command there through App
   Server `command/exec`, journals the structured results, and checks Git
   invariants.
6. The reviewer audits the exact resulting SHA; acceptance is recorded only for
   that SHA.

The `same-host` and `no-push` contracts are deliberate. Both tasks, both
worktrees, the Git common directory, the Codex App Server, and the coordinator
must be on one host. The coordinator does not push, open a pull request, merge
into an existing branch, update either source ref, rebase, reset, delete, or
clean up worktrees.

This is enforced beyond prompt text through frozen identities, request-bound
operations, canonical task history, exact Git revalidation, and acceptance
checks. Every coordinator-started turn uses App Server approval policy `never`
and sandbox policy `dangerFullAccess`, so the two participant tasks do not pause
for interactive approvals and are not contained by an App Server OS sandbox.
Use unattended runs only with trusted tasks and repository contents. The daemon
can reject a Run after forbidden evidence or drift, but it cannot undo an
action that a participant already performed.

The Primary verification turn is marker-only and must not run Shell, Git,
file, MCP, or patch tools. After that side-effect-free marker, the coordinator
executes each frozen direct non-Git command exactly once, in order, with the
exact detached-clone cwd and `sandboxPolicy.type: "dangerFullAccess"`. The
participant marker turn retains `approvalPolicy: "never"`. The coordinator
journals STARTED before dispatch and COMPLETED with the structured exit code
and bounded output. An exact completed result is reusable after restart; a
command left STARTED fails closed as `VERIFICATION_EXECUTION_UNCERTAIN` instead
of being executed again automatically. Git executables, shell control, and
dynamic shell or interpreter launchers remain invalid frozen tests. A model's
self-reported success is never test evidence.

The coordinator rejects acceptance when participant history contains
publication, destructive Git, shell chaining, wrong-directory commands, or
unexpected side effects. Each accepted integration action must still match its
bound request and repository invariants, and every coordinator-owned
verification result must match the frozen command and cwd.
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

Version 0.1.24 configures Codex to approve only that request-bound plugin tool:
`plugins.worktree-merge-consensus.mcp_servers.worktreeMergeConsensus.tools.consensus_apply_patch.approval_mode = "approve"`.
Run `codex-consensus configure` once as the same local account that runs Codex.
It writes the setting through App Server `config/batchWrite`, hot reloads user
configuration, and verifies the effective value. It does not change command
approvals, sandboxing, other MCP tools, or any global approval policy.
`doctor`, new-run start, and controlled-patch recovery fail closed when the
setting is absent or overridden.

Version 0.1.25 also recovers the narrow hot-reload race in which App Server
continues an old approval while the Run is still paused, so the coordinator
safely rejects the request-bound patch with `PATCH_NOT_AUTHORIZED`. Explicit
same-Run resume archives and replaces that completed Primary turn only when
canonical history contains exactly one matching failed patch call, the blocker
identity is exact, no successful patch was recorded, the authorized target is
still clean at the reported merge SHA with both frozen ancestors, and both
source refs remain unchanged. It reuses the existing merge and never creates a
replacement Run.

Version 0.1.26 covers the equivalent App Server residue in which that exact
failed patch call and final blocker are present but the Primary turn remains
`inProgress` with `waitingOnApproval`. Same-Run resume applies the same
identity, no-write, clean-target, ancestry, and frozen-ref checks before
interrupting and replacing only that stale turn. Participant waits now use a
30-minute bounded idle window instead of a five-minute total window; changes
to canonical task status or turn history renew the window, while a task with
no canonical progress still pauses with `COMMUNICATION_FAILURE`.

Version 0.1.27 handles the still earlier residue in which the one exact patch
call is canonically `failed`, but App Server has not persisted a final
assistant JSON and leaves the turn `inProgress + waitingOnApproval`. Same-Run
resume is permitted only when every other canonical item is complete and
allowlisted, SQLite records no successful patch, and the authorized integration
branch is clean at the same verified merge SHA before and after interruption.
Unknown items, ambiguous writes, a changed target, or source drift fail closed.

Version 0.1.28 treats `payload.role` and free-form `blocking_condition` as
non-authoritative diagnostics in this recovery. A completed
`PATCH_NOT_AUTHORIZED` blocker may omit them, because the persisted pending send
already binds the Primary task and the paused daemon state determines why the
single patch call was rejected before Git access. All machine identity fields,
the reported and authoritative merge SHA, canonical tool history, SQLite
no-write proof, and frozen refs remain mandatory.

Version 0.2.0 introduces `worktree-merge-consensus/v2` and replaces
participant-authored protocol envelopes with one
`<consensus-result>...</consensus-result>` marker plus free-form Markdown.
Contracts retain one JSON body because
the coordinator must extract exact test commands; plans, review feedback,
integration summaries, and result reviews have no field-level prose schema.
The coordinator binds verdicts to the exact task turn, computes the plan hash,
and derives branch, SHA, changed files, and test evidence from Git and App
Server history. Valid v1 envelopes remain readable for in-flight migration.
The same release can recover an integration whose controlled patch and commit
already succeeded but whose legacy final JSON was invalid: it audits the exact
patch hash and repository result, then requests one read-only marker response
without repeating the patch, branch creation, or merge.

Version 0.2.1 makes the verification instruction explicit: the Primary must
create one completed command item for each frozen test before returning
`VERIFICATION_READY`. If a completed verification turn returned only the
marker and executed no command at all, explicit same-Run resume may archive
that empty turn and retry verification once against the unchanged integration
SHA. Any partial execution, second empty attempt, unknown item, repository
drift, or accepted result remains terminal.

Version 0.2.2 makes `VERIFICATION_READY` mean “the complete evidence set is
ready,” not “every test passed.” The Primary must run every frozen command even
after an earlier nonzero exit. The coordinator derives exit codes and bounded
diagnostic output from the canonical command items; failed commands route the
same Run back to a new controlled integration round, and the final tested SHA
still requires Reviewer approval. After Cargo is installed, one exact
side-effect-free `CARGO_UNAVAILABLE` verification blocker can also be resumed
once without replacing the Run or integration branch.

Version 0.2.3 persists App Server `item/started`, `item/completed`, and
`turn/completed` events before accepting a participant turn. This preserves
authoritative command and controlled-tool evidence when newer App Server
storage returns only user and final-agent messages from `thread/read`.
Completed event evidence is merged with persisted task history under the exact
run, task, and turn identity; older App Server history remains a compatible
fallback. One migration-only, side-effect-free verification retry is available
for the exact prior sequence of an empty verification attempt followed by one
`CARGO_UNAVAILABLE` recovery and then missing persisted command evidence. It
never repeats a patch, branch creation, merge, or source-ref update.

Version 0.2.4 makes all coordinator-started turns fully unattended by sending
App Server approval policy `never` for integration and isolated verification as
well as read-only review. This removes per-command human confirmation without
changing the pinned writable roots, offline sandbox, exact command-evidence
checks, source-ref validation, or the request-bound patch-tool approval.

Version 0.2.5 sends `dangerFullAccess` for every participant turn and moves
test execution out of the Primary marker turn into coordinator-owned
verification through App Server `command/exec`. Structured command results are
journaled in SQLite for exact restart behavior. One bounded migration can
resume only the exact legacy 0.2.4 blocked history on the same Run, branch, and
integration SHA; it archives one final side-effect-free verification turn and
cannot repeat a patch, branch creation, merge, commit, or source-ref update.

Version 0.2.6 clears archived App Server event rows before reusing a persisted
turn record. It also performs one fail-closed startup repair for the exact
v0.2.5 post-migration completion collision. That repair preserves the active
turn, Run, integration branch and SHA, source refs, patch record, merge, and
commit; it neither sends a second resume nor executes a test during repair.

Version 0.2.7 makes participant patch-tool availability coordinator-owned and
establishes a durable binding before the first Primary action. The frozen
selected task is the **Source Primary**. If App Server reports it as
`notLoaded`, the coordinator loads it with the task-scoped
`worktreeMergeConsensusParticipant` configuration and binds the **Effective
Primary** directly to that same task. A preloaded Source Primary that already
has exactly `consensus_apply_patch` also binds directly. A preloaded Source
Primary without that exact tool is not mutated in place: the coordinator calls
`thread/goal/get` on the Source Primary and requires null before it calls
`thread/fork` with `ephemeral: true`, `excludeTurns: false`, and the participant
configuration. In other words, a preloaded Source Primary without the tool uses
an ephemeral full-history mirror. The fork request does not carry or continue a
goal. The coordinator accepts the ephemeral full-history mirror only after its
complete turn-ID sequence matches, it is idle, and its paginated MCP inventory
is exact. The Effective Primary mirror represents the Source Primary; it is not
a third source or reviewer and carries no active Source goal. Supported Codex
runtimes may reject goal queries on ephemeral tasks, so the coordinator never
calls `thread/goal/get` on the mirror.

Before every Primary action, including contract, plan, integration, and
verification, the coordinator resumes the Effective Primary and consumes every
page of `mcpServerStatus/list` with `detail: "toolsAndAuthOnly"` before
`turn/start`. The participant server must expose exactly
`consensus_apply_patch`; the operator plugin's eight-tool visibility is not
participant evidence. Reviewer routing is unchanged. Both selected source task
IDs, source refs, and source worktrees remain frozen. If an ephemeral mirror is
lost, it may be recreated only between completed actions when no send is
pending or uncertain; a pending or uncertain turn is never reforked or resent.
Because `thread/fork` is non-idempotent, an uncertain fork response is never
automatically repeated. This contract requires Codex CLI `>=0.144.1`.

After a matching 0.2.8 deployment, explicit `consensus_resume` may recover
only the exact post-0.2.6 `CONTROLLED_PATCH_TOOL_UNAVAILABLE` correction
blocker. It preserves the same Run, round, branch, old SHA, and failed frozen
verification evidence; archives only the empty correction turn; reacquires the
lock; repeats participant preflight; and retries one request-bound correction
patch and correction commit. The new SHA must advance and every frozen
verification command reruns.
Installing or enabling the operator plugin alone never mutates the blocked Run.

Version 0.2.8 adapts ephemeral Effective Primary execution to the App Server
contract used by Codex 0.145.0. Ephemeral tasks are checked with
`thread/read(includeTurns: false)`, are never passed to `thread/resume`, and
complete turns from durably journaled `item/*` and `turn/completed` events.
The coordinator freezes a hash of the Source Primary turn-ID sequence before
forking and records turn-start intent before dispatch. A lost start response is
therefore never resent, and a missing terminal event fails closed instead of
querying unsupported ephemeral history. Stored Source, Reviewer, and direct
Primary tasks retain canonical full-history recovery.

Version 0.2.9 makes completed integration auditing side-effect-aware. Approved
write commands still require a canonical completed result with exit code zero;
retry-safe read-only inspections may have a canonical nonzero terminal result
without being mistaken for a failed write. Explicit `consensus_resume` may
recover only the exact completed integration turn whose request-bound patch and
commit already succeeded before the old audit blocked it. Recovery revalidates
the frozen sources, patch provenance, clean target branch, ancestry, and final
SHA, archives only that response attempt, and requests one read-only
`INTEGRATION_READY` confirmation on the same Run. It never repeats the patch,
branch creation, merge, staging, or commit. An injected participant call may
carry an explicit null App Server `pluginId` only when its server and tool
identity are exact; missing or mismatched identity still fails closed.

Read [the v2 participant protocol](docs/protocol-v2.md), the
[legacy v1 protocol](docs/protocol-v1.md),
[compatibility policy](docs/compatibility.md), and [security policy](SECURITY.md)
for the exact boundaries.

## Preconditions

- Linux x86_64 or ARM64 for released binaries. Other Unix systems may work for
  development but are not release targets.
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
tar -xzf codex-consensus-v0.2.9-x86_64-unknown-linux-musl.tar.gz
install -m 0755 codex-consensus-v0.2.9-x86_64-unknown-linux-musl/codex-consensus ~/.local/bin/codex-consensus
```

The v0.1.0 GNU archives require GLIBC 2.39 and are superseded. Use v0.1.1 or
later on supported Linux hosts.

Release assets also include a CycloneDX JSON SBOM and the Codex plugin bundle.
Until the real-Codex checklist is recorded, releases are marked
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
codex-consensus configure
codex-consensus doctor
```

If you downloaded the plugin archive, extract it and register the directory
that contains `.agents/plugins/marketplace.json`. Restart Codex or open a new
task after plugin installation or update. In a Codex task,
invoke `$worktree-merge-consensus`; the plugin exposes eight MCP tools. Seven,
including `consensus_list_worktrees`, launch and control the persistent
coordinator. The eighth, `consensus_apply_patch`, is a participant-only,
request-bound write capability described below. It does not relay review turns
through a third agent.

The operator plugin's eight tools are not the Primary participant's tool
inventory. The coordinator injects and preflights the task-scoped participant
server through the direct or ephemeral Effective Primary binding before every
Primary action; plugin installation alone does not change a blocked Run.

Names such as `consensus_doctor` are MCP tool names, not shell executables.
Codex starts the plugin server as `codex-consensus mcp-server`; the equivalent
terminal diagnostic is `codex-consensus doctor`. Do not run
`command -v consensus_doctor`.

If `codex-consensus doctor` reports `LEGACY_SKILL_CONFLICT`, an older manually
installed `$CODEX_HOME/skills/worktree-merge-consensus` is shadowing the plugin
workflow. Back it up or remove it manually, reinstall matching binary/plugin
versions, and restart Codex or open a new task. The tool never deletes it.

`codex-consensus configure` is the installation flow's only intentional Codex
configuration write. It sets and verifies the exact per-plugin, per-server,
per-tool approval key above. If a managed configuration layer overrides that
value, configuration and startup report `APPROVAL_CONFIGURATION_REQUIRED`
instead of asking the operator to weaken a broader approval policy.

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
coordinator runs after the marker-only Primary verification turn in the
isolated clone.
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

The eight public command groups are therefore `codex-consensus configure`,
`codex-consensus doctor`, `codex-consensus threads`,
`codex-consensus worktrees`, `codex-consensus run`, `codex-consensus status`,
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
Version 0.1.24 prevents that internal patch call from becoming a user approval
deadlock by requiring the exact per-tool approval setting described above. If
an older attempt is already canonically `waitingOnApproval` with one
`inProgress` `consensus_apply_patch` call, explicit same-Run resume may
interrupt and replace only that exact Primary integration turn. The daemon
first verifies the request-bound tool arguments, successful allowlisted command
history, absence of a recorded successful patch, clean authorized target,
source ancestry, and unchanged frozen refs. Unknown or multiple tool calls,
other incomplete items, drift, or any possible write fail closed. A turn that
completed during the interrupt race is reused rather than duplicated.
Version 0.1.25 handles the corresponding completed rejection race: App Server
may continue the old approval immediately after configuration hot reload while
the Run is still paused, causing the daemon to reject the exact patch call with
`PATCH_NOT_AUTHORIZED`. Explicit resume may archive and replace only a
canonically completed Primary turn containing one request-bound failed
`consensus_apply_patch` call and an exact blocker. The daemon additionally
requires no successful patch record, the clean existing target at the reported
merge SHA, both frozen commits as ancestors, and unchanged source refs. Unknown
or additional tool calls, a successful or ambiguous write, mismatched evidence,
or repository drift remain terminal; branch creation and merge are not repeated.
Version 0.1.26 additionally handles only the same exact failed call and blocker
when App Server has persisted the final assistant JSON but leaves the turn
`inProgress` and `waitingOnApproval`. Explicit resume revalidates all 0.1.25
conditions, interrupts that one stale turn, and archives it atomically before
retrying the same request. It also changes participant waiting from a fixed
five-minute total timeout to a 30-minute inactivity timeout renewed by changes
in canonical task status or turn history. Unchanged active state remains
bounded and fails closed.
Version 0.1.27 additionally permits the same stale turn to have no final
assistant JSON only when its one request-bound patch item is already
canonically `failed`. The daemon proves no successful patch was recorded,
requires every command to be complete and allowlisted, snapshots the clean
authorized merge SHA, interrupts only that turn, and requires the same SHA and
clean repository state afterward before atomically retrying the request.
Any assistant message that is present must still be the exact validated
`PATCH_NOT_AUTHORIZED` blocker.
Version 0.1.28 clarifies that the exact validated blocker is defined by its
protocol envelope and direct machine identity fields. The redundant
`payload.role` label and free-form `blocking_condition` prose may be absent;
the pending-send role binding and paused daemon authorization check are the
authoritative evidence for those facts. Missing plan/source/request/branch/SHA
identity remains terminal.
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
  source worktrees and is retained for audit/recovery.

The directory is mode `0700`. The database stores canonical coordinator state,
participant response bodies, and evidence, but not full task conversation
transcripts or generated prompts; Codex itself retains messages in the two
selected task histories. Sensitive App Server diagnostics are redacted. The
managed daemon does not create a persistent log file by default, so CLI output,
Codex task history, and `status --json` are the operational record.

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
- `APPROVAL_CONFIGURATION_REQUIRED`: run `codex-consensus configure` as the
  same account and `CODEX_HOME` used by Codex, then rerun `doctor`. The required
  value is the exact `consensus_apply_patch` key documented above; do not set a
  global auto-approval policy. A managed override must be corrected at its
  controlling layer.
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

## Current non-goals

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
