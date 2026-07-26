# Installation and MCP Diagnostics

Use this reference only when operator `consensus_*` tools are missing or
`consensus_doctor` reports installation, startup, compatibility, or scoped
configuration failure.

## Normal startup

The plugin registers `worktreeMergeConsensus` as a stdio MCP server. Its
`scripts/start-mcp.sh` launcher resolves the exact `codex-consensus` version and
then starts `codex-consensus mcp-server`.

On Linux x86_64 or ARM64, the resolver downloads the matching static musl
archive from the same GitHub Release as the plugin version, verifies its exact
entry in `SHA256SUMS`, installs it atomically under private `PLUGIN_DATA`, and
reuses the versioned cache. First startup may take up to 300 seconds.

`CODEX_CONSENSUS_BIN` is an optional exact-version override for offline or
centrally managed deployments. A missing, non-executable, or mismatched override
fails closed.

## Diagnostic order

1. Run `codex mcp list --json`.
2. Find `worktreeMergeConsensus` and inspect whether it is absent, disabled, or
   unable to start.
3. Check the reported cwd, launcher, `startup_timeout_sec: 300`, and startup
   diagnostic.
4. Open a new Codex task after installing or upgrading the plugin; an already
   open task does not gain a newly loaded MCP surface.
5. Use `codex-consensus doctor` only when a direct or centrally managed binary
   is actually installed. A successful CLI doctor does not prove that the
   current task loaded the plugin MCP tools.

`consensus_doctor` is an MCP tool, not a shell command. Never search for or run
a `consensus_doctor` executable.

## Common failures

### Runtime download

The first launch requires `curl` or `wget`, `tar`, and `sha256sum` or `shasum`.
The downloader accepts HTTPS only. It honors standard proxy environment
variables such as `HTTPS_PROXY`; ensure the Codex/App Server process inherits
them when a host cannot reach GitHub directly.

Report the exact URL, architecture, or timeout diagnostic. Do not copy an
unverified binary into the managed cache.

### Checksum or version mismatch

Stop. The resolver never accepts an archive whose selected SHA-256 entry or
`codex-consensus --version` output differs from the plugin's base SemVer. Remove
only a cache that the user explicitly authorizes removing, then reinstall from a
trusted matching release.

### Unsupported host

Automatic runtime installation supports Linux `x86_64` and `aarch64`. Other
hosts require an exact compatible binary supplied through
`CODEX_CONSENSUS_BIN`.

### `LEGACY_SKILL_CONFLICT`

An older manually installed `$CODEX_HOME/skills/worktree-merge-consensus`
shadows the plugin. Report the exact path. Do not delete or move it without user
authorization. After the user backs it up or removes it, reinstall the plugin
and open a new task.

### `APPROVAL_CONFIGURATION_REQUIRED`

The plugin may configure and verify only this request-bound key:

```text
plugins.worktree-merge-consensus.mcp_servers.worktreeMergeConsensus.tools.consensus_apply_patch.approval_mode = "approve"
```

It must not enable global auto-approval. If managed policy blocks the scoped
write, tell the user to run `codex-consensus configure` under the same account
and `CODEX_HOME`, or ask the administrator to permit only that key.

### `INCOMPATIBLE_CODEX`

Require Codex CLI and the managed App Server to satisfy the supported method
contract beginning at `>=0.144.1`. Report the exact missing method or identity
mismatch; do not bypass compatibility checks.

## Stop boundary

When the plugin tool surface remains unavailable, report the actionable error
and stop. Do not substitute ordinary task tools, manually relay participant
messages, or perform the integration with ad hoc Git commands.
