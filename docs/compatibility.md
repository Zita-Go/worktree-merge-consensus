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
   boundary, malformed version output, and missing methods.
3. Run the complete fake-App-Server E2E suite, including recovery, duplicated
   notifications, cancellation, plan revision, result revision, and Git drift.
4. Complete and record
   [the real Codex smoke test](real-codex-smoke-test.md) on both release
   architectures where practical.
5. Widen or add the adapter gate in a reviewed release.

Unknown future versions continue to fail closed until this process is complete.

## Version support

The project follows pre-1.0 semantic versioning: a minor release may change
unstable CLI or plugin details, but published protocol identifiers remain
explicit. Patch releases do not silently widen the Codex adapter range.
