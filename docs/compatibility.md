# Compatibility Policy

## Supported surface

Version 0.2 has one checked Codex adapter:

| Component | Supported value |
| --- | --- |
| Codex CLI | `>=0.144.1` |
| App Server family | `codex-app-server/experimental-v2` |
| Participant response protocol | `worktree-merge-consensus/v2` |
| Internal/legacy protocol | `worktree-merge-consensus/v1` |
| Release OS/architecture | Linux x86_64 and Linux ARM64 |
| Local transport | WebSocket over the managed Unix domain socket |
| Rust MSRV | 1.85 |

The executable parses an exact semantic version from `codex --version` before
starting `codex app-server daemon start` and `codex app-server proxy`. It then
performs the standard WebSocket HTTP Upgrade through the byte proxy and carries
one App Server message per text frame; the managed Unix-socket transport is not
the JSONL transport used by `codex app-server --stdio`. An unparseable version,
a proxy-handshake or initialization timeout, or a version below `0.144.1` fails
closed. There is no maximum-version gate. The managed App Server identity,
required methods, and typed response shapes are still validated at runtime and
fail closed on mismatch.

## Required App Server contract

The adapter is based on
[`schemas/app-server/supported-methods.json`](../schemas/app-server/supported-methods.json)
and requires these JSON-RPC methods:

- `initialize`
- `thread/list`
- `thread/read`
- `thread/resume`
- `thread/fork`
- `thread/goal/get`
- `turn/start`
- `turn/interrupt`
- `command/exec`
- `config/read`
- `config/batchWrite`
- `mcpServerStatus/list`

It consumes `thread/status/changed`, `turn/started`, and `turn/completed`
notifications. Task reads include turns. Coordinator prompts require exactly
one `<consensus-result>...</consensus-result>` marker. Only contract bodies are
JSON; plans, feedback, and summaries are free-form Markdown. The daemon parses
the marker locally, binds it to the exact pending task turn, and derives Git,
SHA, changed-file, and test evidence in coordinator code. Valid v1 envelopes
remain locally schema-validated as a migration fallback.

The adapter intentionally omits App Server `outputSchema`. Codex 0.144.6 can
accept the repository's full Draft 2020-12 schema at `turn/start` yet complete
the turn with only a user message and no assistant output. The v2 marker parser,
contract validation, authoritative command history, Git checks, and
state-machine invariants are therefore local and fail closed without relying on
the provider's structured-output schema subset.

The App Server is experimental. A Codex release may change shapes or semantics
without preserving this adapter. A method call or response-shape mismatch is a
communication/compatibility failure, never permission to continue with partial
evidence.

The `initialize` response is decoded against the pinned required fields
`codexHome`, `platformFamily`, `platformOs`, and `userAgent`. `codexHome` must
be absolute, the platform family must be Unix, and the exact semantic version
in either the managed `codex-cli/...` or `Codex Desktop/...` user-agent form
must match `codex --version`. The initialization shape used by this adapter
does not advertise a method inventory; therefore each required operational
method is invoked through a typed adapter, and JSON-RPC
`Method not found` or a shape mismatch fails closed when reached. The checked-in
fixture is an adapter contract, not a claim that the server echoed the client's
own method list.

The client declares `capabilities.experimentalApi: true` during `initialize`.
App Server requires that opt-in before accepting the pinned
`turn/start.environments` selection. Each turn selects environment `local` with
the authorized absolute cwd instead of disabling execution environments or
inheriting an arbitrary sticky environment.
Version 0.1.24 also reads the effective Codex configuration and requires the
exact key
`plugins.worktree-merge-consensus.mcp_servers.worktreeMergeConsensus.tools.consensus_apply_patch.approval_mode`
to equal `"approve"`. `codex-consensus configure` writes only that key through
`config/batchWrite` with `reloadUserConfig: true`, then verifies it through
`config/read`. It does not modify command approvals, sandbox policy, other MCP
tools, or a global approval setting. `doctor`, run start, and controlled-patch
resume fail with `APPROVAL_CONFIGURATION_REQUIRED` when the effective value is
absent or overridden. The coordinator uses `turn/interrupt` only for the exact
v0.1.24 pending controlled-patch approval recovery described by the protocol.
Version 0.1.25 does not broaden interruption behavior: it reads canonical
completed history and retries only the exact failed request-bound
`consensus_apply_patch` plus `PATCH_NOT_AUTHORIZED` shape described by the
protocol, after repository and SQLite no-write checks.
Version 0.1.26 adds one equally narrow interruption case for an App Server
residue that has the same exact failed tool item and final blocker but remains
`inProgress + waitingOnApproval`. The daemon revalidates the complete 0.1.25
identity and repository proof before interrupting only that turn. Participant
turns use a 30-minute canonical-inactivity timeout; changed canonical status or
turn history renews the deadline, while unchanged active state remains bounded.
Version 0.1.27 accepts the same residue without a final assistant JSON only
when the one exact patch item is already canonically `failed`, every other item
is complete and allowlisted, no successful patch is recorded, and the clean
authorized merge SHA is identical before and after interruption. If any agent
message exists, it must still validate as the exact 0.1.25 blocker. Unknown
items, possible writes, target movement, and source drift remain terminal.
Version 0.1.28 keeps every machine-checkable blocker field mandatory but no
longer requires the redundant `payload.role` label or free-form
`blocking_condition` prose. The pending send already binds the exact Primary
task, and a paused Run rejects `consensus_apply_patch` before Git access. A
missing request, plan, source, target, or result-SHA identity still fails closed.
Version 0.2.0 moves machine identity out of participant-authored responses.
One action marker controls each turn; contract tests remain structured, while
plans and feedback stay as complete Markdown. The coordinator computes plan
identity and derives integration and verification facts. It can also resume the
same Run after one recorded successful controlled patch followed by an invalid
legacy integration response, but only after matching the stored patch hash,
canonical turn, authoritative Git result, and unchanged frozen refs. The retry
is read-only and cannot repeat the patch, branch creation, or merge.
Version 0.2.1 strengthens the Primary verification prompt and permits one
same-Run retry only when the exact completed verification turn contains no
command execution at all. Partial execution, a second empty response, unknown
items, and identity or repository drift still fail closed.
Version 0.2.2 treats `VERIFICATION_READY` as proof that all frozen command
items completed, not as a participant-authored pass verdict. The coordinator
derives exit codes and bounded failure output from canonical App Server items;
nonzero commands route the same Run to another controlled integration round.
It also permits one same-Run retry of an exact completed, side-effect-free
`CARGO_UNAVAILABLE` verification blocker after the local toolchain is repaired.
Both paths preserve the integration branch and frozen source refs.
Version 0.2.3 no longer assumes that `thread/read` replays completed command or
MCP items. While a turn is active, the daemon writes every `item/started` and
`item/completed` lifecycle item to private SQLite and records `turn/completed`
as the ordered completion barrier. It accepts event-derived evidence only when
the run, task, turn, item identity, and lifecycle are complete, then merges it
with persisted user/final-agent history. App Server versions that still return
full turn items continue to use that history as a fallback. Existing Runs with
the exact archived sequence of one empty verification attempt and one
side-effect-free `CARGO_UNAVAILABLE` recovery may replace one subsequent
verification turn whose command evidence is absent from persisted history.
That compatibility recovery is recorded atomically and is allowed only once;
it cannot repeat a controlled patch, branch creation, merge, or source update.
Version 0.2.5 requires `command/exec` for coordinator-owned verification. The
request supplies a direct argv array, exact absolute detached-clone cwd,
`sandboxPolicy.type: "dangerFullAccess"`, timeout, and output-byte cap. The
typed response must contain an exit code, stdout, and stderr; a method or shape
mismatch fails closed. SQLite records the exact request identity before
dispatch and reuses only a completed exact response. An execution left STARTED
after uncertain delivery returns
`VERIFICATION_EXECUTION_UNCERTAIN` rather than being dispatched again.

Version 0.2.6 deletes the exact archived turn's event rows in the same
transaction that resets its reusable turn record. On daemon startup it may
repair the single known v0.2.5 completion collision only when the blocked
diagnostic, Primary VERIFY request, four archived statuses, stale completion
identity, marker-only active event evidence, one successful patch, zero test
executions, and absent repository lock all match exactly. Canonical task
history and the unchanged Git result are revalidated before mutation. The
repair removes only stale archived event rows, reacquires the existing Run's
lock, and restores its already-started verification action; it does not archive
another turn, dispatch another resume, or execute a verification command.

Version 0.2.7 establishes a participant binding before the first Primary
action. The selected frozen task is the **Source Primary**. A `notLoaded`
Source Primary is resumed with the task-scoped
`worktreeMergeConsensusParticipant` configuration and becomes the **Effective
Primary** directly. A preloaded Source Primary with the exact participant
inventory also binds directly. A preloaded Source Primary without that tool is
not reconfigured in place: the coordinator first calls `thread/goal/get` on the
Source Primary and requires null, then calls `thread/fork` with `threadId`,
`config`, `ephemeral: true`, and `excludeTurns: false`, without carrying or
continuing a goal. It accepts the ephemeral full-history mirror only when every
canonical turn ID matches the Source, the mirror is idle, and the participant
inventory is exact. The mirror represents the Source Primary; it is not an
additional source or reviewer and no active Source goal is carried. Because
supported Codex runtimes may reject goal queries for ephemeral tasks, the
mirror itself is never passed to `thread/goal/get`.

Before every Primary action, the coordinator resumes the Effective Primary and
fully paginates `mcpServerStatus/list` using `threadId`, `detail:
"toolsAndAuthOnly"`, `limit: 100`, and an opaque nullable `cursor` before
`turn/start`. The participant server must expose exactly
`consensus_apply_patch`; missing, malformed, duplicate, or additional
participant tools fail closed. The operator plugin's eight tools are not
participant visibility evidence. Reviewer routing is unchanged, and the
selected task IDs, source refs, and source worktrees remain frozen. A missing
ephemeral mirror can normally be recreated only between completed actions with
no pending or uncertain send. Version 0.2.12 adds one atomic exception for a
pending request proven unsent by the absence of an effective task ID, turn ID,
and turn-start intent. Uncertain turns are never reforked or resent.
`thread/fork` is non-idempotent and is never automatically repeated after an
uncertain response. The App Server contract remains Codex CLI `>=0.144.1`.

Version 0.2.7 also permits explicit recovery of only the exact production
blocker left after the 0.2.6 recovery: the same blocked Run, correction round,
integration branch, prior integration SHA, and failed frozen verification
evidence, with `CONTROLLED_PATCH_TOOL_UNAVAILABLE` and an otherwise empty,
side-effect-free correction turn. After a matching 0.2.8 deployment, explicit
`consensus_resume` archives only that correction turn, atomically reacquires
the Run lock, preflights the participant server, and retries the same request.
At most one request-bound corrective patch and commit are allowed; the
integration SHA must advance and every frozen verification command runs again.
Installing or enabling the operator plugin alone never mutates a blocked Run.

Version 0.2.8 distinguishes persisted tasks from ephemeral participant forks.
For an ephemeral Effective Primary, the coordinator uses
`thread/read(includeTurns: false)` only, never calls `thread/turns/list` or
`thread/resume`, and reconstructs terminal turns from SQLite-journaled
`item/started`, `item/completed`, and `turn/completed` events. It persists the
Source Primary turn-ID-sequence hash with every ephemeral binding and writes a
turn-start intent before dispatch. Missing terminal evidence and uncertain
delivery therefore fail closed without a second `turn/start` or replacement
fork. Stored Source, Reviewer, and direct Primary tasks keep the canonical
full-history path.

Version 0.2.9 accepts canonical nonzero terminal results only for retry-safe
read-only integration inspections. Approved writes still require
`completed`/exit-zero evidence. This permits explicit same-Run recovery of the
exact completed integration response that was blocked only after its
request-bound patch and commit succeeded. Recovery reuses the existing branch
and SHA after checking patch provenance, frozen refs, ancestry, and
cleanliness; it archives one response attempt and sends one read-only
confirmation. The live command gate does not approve `git diff --no-index`;
only the exact historical `/dev/null` to normalized repository-relative path
shape is recovery-safe. An explicit null `pluginId` is compatible only for the
exact injected participant server and controlled patch tool.

Version 0.2.10 replaces the completed-integration recovery's frozen-worktree
HEAD preflight with the existing integration-in-progress check. This permits
the Primary worktree to be attached to the exact authorized target after its
successful commit, while continuing to require unchanged source refs, a frozen
Reviewer worktree, and the same repository. All later authoritative integration
result checks remain unchanged.

Version 0.2.11 treats the App Server's canonical `unifiedExecStartup` source as
agent-initiated command provenance during recovery. Codex 0.145.0 emits that
source for commands started through unified exec. Recovery continues to accept
an omitted legacy source as the schema default and rejects `userShell`,
`unifiedExecInteraction`, null, malformed, and unknown sources. Command, cwd,
terminal-result, side-effect, frozen-state, and authoritative target checks are
unchanged.

Version 0.2.12 recovers that read-only confirmation when the active ephemeral
Effective Primary is no longer loaded before dispatch. The store atomically
deactivates the old binding, inserts a replacement with the same frozen Source
history fingerprint, and rebinds the one pending request only when no task ID,
turn ID, or turn-start intent exists. The successful controlled patch remains
attributed to the archived completed old generation. Cross-generation
validation requires two ephemeral bindings with the same frozen Source,
participant server, nonempty history hash, an exact archived completed attempt,
and the replacement as the active binding. Any sent, intent-recorded,
uncertain, divergent, or mixed-provenance state remains terminal.

Version 0.2.13 loads the persisted Source Primary before that replacement when
App Server reports the Source as `notLoaded`. It resumes the exact frozen
Source with task-scoped participant configuration, verifies the returned task
identity, waits for idle when necessary, and only then forks a replacement
ephemeral mirror. It never resumes the ephemeral mirror itself.

The same release adds one explicit migration for the exact 0.2.12 terminal
state: `BLOCKED / HISTORY_UNAVAILABLE`, no operation, Primary integration
action, the matching ephemeral binding identity, and detail
`Source Primary before safe mirror recreation is not idle`. Explicit resume
atomically reacquires the repository lock only when the approved plan and
integration-in-progress repository state are unchanged, there is one pending
Primary integration request with no task ID, turn ID, or start intent, its
generation is still active, and an exact archived completed patch attempt
exists for the same request and Effective Primary. The pending row and binding
are not rewritten by the migration; normal proven-unsent rotation follows.
Every near-match remains terminal without Git mutation.

Version 0.2.14 adds one exact read-only command to the Primary integration
policy: `git symbolic-ref --short HEAD` in the frozen Primary worktree.
The command may appear directly or inside one canonical App Server
`/bin/bash -lc` wrapper. No other `symbolic-ref` option, reference operand,
delete form, or two-argument write is accepted.

The same release adds one explicit migration for the exact 0.2.13 terminal
state: `BLOCKED / FORBIDDEN_OPERATION`, no operation, Primary integration
action, matching ephemeral binding identity, and detail
`patch-success confirmation executed a non-read-only command: /bin/bash -lc 'git symbolic-ref --short HEAD'`.
Explicit resume requires an approved frozen plan, no recorded integration
result or test evidence, the integration-in-progress repository state, the
exact completed pending request turn, matching request hash and binding
generation, one successful request-bound controlled patch, and canonical turn
items that are all retry-safe. Every command must be terminal, agent-initiated,
in the frozen Primary cwd, and read-only under the current policy. The
authoritative target must remain clean, contain both frozen source commits, and
match its changed-file result. The store then reacquires the repository lock
and archives only that completed confirmation atomically on the same Run. The
replacement prompt forbids another controlled patch, merge, staging, or
commit. Changed identity, side effects, uncertain execution, a near-match
diagnostic, or an existing accepted result remains terminal.

Before every `turn/start`, the coordinator also calls `thread/resume` with the
fixed task ID for persisted direct and Reviewer tasks. Ephemeral Effective
Primary tasks are already loaded and must not be resumed. `thread/read` can
return persisted history for a `notLoaded` task, but it does not load that task
for model execution; starting a turn after only reading history can produce a
completed user-message-only turn.

Every `turn/start` also carries the pinned role-specific cwd, runtime workspace
roots, approval policy, and a same-host `local` environment selection with that
cwd. Release 0.2.4 used separate offline read-only, primary-integration, and
isolated-verification sandbox profiles. Release 0.2.5 instead sends
`approvalPolicy: "never"` and `sandboxPolicy.type: "dangerFullAccess"` for
every participant turn. Participant execution is therefore fully unattended
and not OS-sandboxed. This mode is only for trusted tasks and trusted repository
contents. The coordinator still verifies canonical item history, request
identity, exact Git state, command evidence, and frozen refs before acceptance,
but those checks cannot undo a participant side effect already performed.
App Server reports a
unified-exec command as a shell-joined argument vector, normally one
known shell followed by `-c` or `-lc` and the model's script. The coordinator
removes exactly that one wrapper before applying its existing allowlist to the
inner command evidence. Nested launchers, non-null subcommand `approvalId`
values, and non-`local` execution environments reject the Run. Target-existence preflight is
limited to `git show-ref --verify` with the exact frozen integration ref or
`git branch --list` with the exact frozen integration branch; no other form of
either subcommand is accepted. The
Primary verification participant turn is marker-only; any command, file,
MCP, patch, or other side-effect-capable item rejects it before coordinator
test execution. Frozen tests then run only through the typed `command/exec`
adapter using the exact verification cwd. Command evidence and frozen-ref
checks determine whether the result may be accepted.
These fields are part of the checked-in `supported-methods` fixture and are
process-tested. An adapter change must revalidate their semantics and the
`commandExecution` item fields (`id`, `command`, `cwd`, `status`, `exitCode`,
and optional `source`) before changing the runtime contract.

The task's cwd returned by `thread/list` or `thread/read` is display metadata,
not source identity. New runs independently supply two task IDs and two
registered worktree paths through CLI or MCP. The daemon still verifies the
returned task IDs, but every turn uses the frozen explicit worktree even when
both tasks report the same cwd or a non-Git directory.

The plugin contract exposes eight MCP tools. Seven are operator tools,
including `consensus_list_worktrees`; `consensus_start` requires both task IDs
and both worktree paths. The eighth, `consensus_apply_patch`, is available only
to an exact active Primary integration request and has no public CLI
equivalent. Plugin and binary versions must come from the same release.
After installing or updating the plugin, restart Codex or open a new task. A
conflicting manually installed `$CODEX_HOME/skills/worktree-merge-consensus`
is reported as `LEGACY_SKILL_CONFLICT` and is never deleted automatically.
The same-account installation must also run `codex-consensus configure` once;
a broad or global auto-approval configuration is neither required nor accepted
as a substitute for the exact tool key.

`doctor` validates a fresh App Server protocol connection and asks the
coordinator daemon to probe its own connection, but deliberately does not spend
a model turn. If the managed App Server restarts after the coordinator daemon,
the production adapter reconnects before retrying idempotent reads and reaps
the closed proxy process. It only reconnects before `turn/start` when closure
is already known; an uncertain non-idempotent delivery is left to persisted
pending-send recovery instead of being duplicated. If a run reports `completed
turn has no final assistant JSON`, verify `codex login status` and outbound
ChatGPT connectivity from the persistent App Server process. Proxy variables
must be present when that daemon starts; after correcting them, restart both
the App Server daemon and `codex-consensus` daemon, then start a new run because
the user-only turn is retained for idempotent recovery.

The managed identity check accepts the exact App Server identities emitted for
`codex-cli`, Codex Desktop, and the fixed `worktree-merge-consensus` client
name. In every case, the embedded Codex version must exactly match
`codex --version`; unrelated client prefixes remain rejected.

## Persisted-state compatibility

The run-state schema is explicitly versioned. This prerelease does not
silently migrate state written by earlier development snapshots: missing or
unknown schema versions return `INCOMPATIBLE_STATE`. Preserve the old state
directory for audit and start the released binary with a fresh `--state-dir`
instead of editing SQLite by hand.

## Operating-system policy

Release artifacts are built natively on GitHub-hosted Linux x86_64 and ARM64
runners for the musl targets and are rejected by the release workflow if they
contain a dynamic-library dependency or program interpreter. They therefore do
not impose a host GLIBC version floor. The daemon depends on Unix socket
permissions, so Windows is unsupported. macOS is used for development tests but
is not a release target or compatibility promise.

All participants must satisfy the `same-host` constraint. Cross-host App Server
connections, Git transfer, SSH relays, and shared-network SQLite files are not
supported.

## Adapter maintenance procedure

The version gate admits new Codex versions automatically. If a new version
changes an App Server method, shape, or behavior used by this project, adapting
to that change requires all of the following:

1. Update the checked-in method/shape fixture in a reviewed change.
2. Add boundary, malformed-version, handshake-identity, turn-policy-shape, and
   operational-method regression tests for the observed behavior.
3. Run the complete fake-App-Server E2E suite, including recovery, duplicated
   notifications, cancellation, plan revision, result revision, Git drift,
   exact-SHA isolated verification, and authoritative command-item evidence.
4. Complete and record
   [the real Codex smoke test](real-codex-smoke-test.md) on both release
   architectures where practical.
5. Publish the adapter change only after its runtime checks fail closed on the
   incompatible shape.

## Version support

The project follows pre-1.0 semantic versioning: a minor release may change
unstable CLI or plugin details, but published protocol identifiers remain
explicit. The minimum supported Codex version remains an explicit release
contract even though there is no maximum-version gate.
