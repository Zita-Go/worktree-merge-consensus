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
notifications. Task reads include turns, and every coordinator turn supplies
the checked-in protocol JSON Schema as `outputSchema`.

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

Every `turn/start` also carries the pinned role-specific cwd, runtime workspace
roots, approval policy, an empty `environments` array that disables inherited
sticky environments, and one of three sandbox profiles: offline read-only
review, offline primary integration with source worktree/Git-common writes, or
offline primary verification with only the isolated clone writable. The
integration profile disables temporary-directory writes; the verification
profile permits temporary build artifacts but has no source Git-common root.
These fields are part of the checked-in `supported-methods` fixture and are
process-tested. An adapter change must revalidate their semantics and the
`commandExecution` item fields (`id`, `command`, `cwd`, `status`, `exitCode`,
and optional `source`) before changing the runtime contract.

## Persisted-state compatibility

The v0.1 run-state schema is explicitly versioned. This prerelease does not
silently migrate state written by earlier development snapshots: missing or
unknown schema versions return `INCOMPATIBLE_STATE`. Preserve the old state
directory for audit and start the released binary with a fresh `--state-dir`
instead of editing SQLite by hand.

## Operating-system policy

Release artifacts are built natively on GitHub-hosted Linux x86_64 and ARM64
runners for the GNU targets. The daemon depends on Unix socket permissions, so
Windows is unsupported. macOS is used for development tests but is not a v0.1
release target or compatibility promise.

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
