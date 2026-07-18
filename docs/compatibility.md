# Compatibility Policy

## Supported surface

Version 0.1 has one checked Codex adapter:

| Component | Supported value |
| --- | --- |
| Codex CLI | `>=0.144.5, <0.145.0` |
| App Server family | `codex-app-server/experimental-v2` |
| Consensus protocol | `worktree-merge-consensus/v1` |
| Release OS/architecture | Linux x86_64 and Linux ARM64 |
| Local transport | Unix domain socket |
| Rust MSRV | 1.85 |

The executable parses an exact semantic version from `codex --version` before
starting `codex app-server daemon start` and `codex app-server proxy`. An
unparseable version, a version below `0.144.5`, or a version at or above
`0.145.0` returns `INCOMPATIBLE_CODEX`. There is no optimistic fallback.

## Required App Server contract

The adapter is based on
[`schemas/app-server/0.144.5-methods.json`](../schemas/app-server/0.144.5-methods.json)
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
be absolute, the platform family must be Unix, and the App Server user-agent
version must exactly match `codex --version`. Codex 0.144.5 does not advertise
a method inventory in `initialize`; therefore each required operational method
is invoked through a typed adapter, and JSON-RPC `Method not found` or a shape
mismatch fails closed when reached. The version fixture is an adapter contract,
not a claim that the server echoed the client's own method list.

Every `turn/start` also carries the pinned role-specific cwd, runtime workspace
roots, approval policy, an empty `environments` array that disables inherited
sticky environments, and one of three sandbox profiles: offline read-only
review, offline primary integration with source worktree/Git-common writes, or
offline primary verification with only the isolated clone writable. The
integration profile disables temporary-directory writes; the verification
profile permits temporary build artifacts but has no source Git-common root.
These fields are part of the 0.144.5 shape fixture and are process-tested; an
adapter upgrade must revalidate their semantics and the `commandExecution`
item fields (`id`, `command`, `cwd`, `status`, `exitCode`, and optional `source`)
before widening the supported range.

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

## Upgrade procedure

Supporting a new Codex minor line requires all of the following:

1. Capture a new checked-in method/shape fixture; do not widen the old maximum
   before verifying the protocol.
2. Add compatibility tests for the first accepted version, the previous
   boundary, malformed version output, handshake identity, turn policy shapes,
   and operational method failures.
3. Run the complete fake-App-Server E2E suite, including recovery, duplicated
   notifications, cancellation, plan revision, result revision, Git drift,
   exact-SHA isolated verification, and authoritative command-item evidence.
4. Complete and record
   [the real Codex smoke test](real-codex-smoke-test.md) on both release
   architectures where practical.
5. Widen or add the adapter gate in a reviewed release.

Unknown future versions continue to fail closed until this process is complete.

## Version support

The project follows pre-1.0 semantic versioning: a minor release may change
unstable CLI or plugin details, but published protocol identifiers remain
explicit. Patch releases do not silently widen the Codex adapter range.
