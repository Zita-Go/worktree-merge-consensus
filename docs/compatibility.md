# Compatibility Policy

## Supported surface

Version 0.1 has one checked Codex adapter:

| Component | Supported value |
| --- | --- |
| Codex CLI | `>=0.144.1` |
| App Server family | `codex-app-server/experimental-v2` |
| Consensus protocol | `worktree-merge-consensus/v1` |
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
- `turn/start`

It consumes `thread/status/changed`, `turn/started`, and `turn/completed`
notifications. Task reads include turns. Coordinator prompts require exactly
one JSON object, then the daemon validates the final assistant text locally
against the checked-in protocol JSON Schema and state-machine invariants.

The adapter intentionally omits App Server `outputSchema`. Codex 0.144.6 can
accept the repository's full Draft 2020-12 schema at `turn/start` yet complete
the turn with only a user message and no assistant output. Local validation is
therefore authoritative and fails closed without relying on the provider's
smaller structured-output schema subset.

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
Before every `turn/start`, the coordinator also calls `thread/resume` with the
fixed task ID. `thread/read` can return persisted history for a `notLoaded`
task, but it does not load that task for model execution; starting a turn after
only reading history can produce a completed user-message-only turn.

Every `turn/start` also carries the pinned role-specific cwd, runtime workspace
roots, approval policy, a same-host `local` environment selection with that
cwd, and one of three sandbox profiles: offline read-only
review, offline primary integration with source worktree/Git-common writes, or
offline primary verification with only the isolated clone writable. The
App Server may include a `proposedExecpolicyAmendment` in an approval request;
the coordinator's plain one-time `accept` does not apply that proposal. The
proposal therefore does not by itself fail the command gate, while actual
additional filesystem or network permissions still fail closed. The
integration profile disables temporary-directory writes; the verification
profile permits temporary build artifacts but has no source Git-common root.
These fields are part of the checked-in `supported-methods` fixture and are
process-tested. An adapter change must revalidate their semantics and the
`commandExecution` item fields (`id`, `command`, `cwd`, `status`, `exitCode`,
and optional `source`) before changing the runtime contract.

The task's cwd returned by `thread/list` or `thread/read` is display metadata,
not source identity. New runs independently supply two task IDs and two
registered worktree paths through CLI or MCP. The daemon still verifies the
returned task IDs, but every turn uses the frozen explicit worktree even when
both tasks report the same cwd or a non-Git directory.

The v0.1 plugin contract exposes seven MCP tools, including
`consensus_list_worktrees`; `consensus_start` requires both task IDs and both
worktree paths. Plugin and binary versions must come from the same release.
After installing or updating the plugin, restart Codex or open a new task. A
conflicting manually installed `$CODEX_HOME/skills/worktree-merge-consensus`
is reported as `LEGACY_SKILL_CONFLICT` and is never deleted automatically.

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

The v0.1 run-state schema is explicitly versioned. This prerelease does not
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
is not a v0.1 release target or compatibility promise.

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
