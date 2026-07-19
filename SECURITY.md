# Security Policy

## Supported versions

Only the latest published 0.1.x release receives security fixes while the
project remains pre-1.0. The supported Codex version floor and runtime adapter
checks are listed in [the compatibility policy](docs/compatibility.md).

## Report a vulnerability privately

Do not open a public issue for a suspected vulnerability. Use the repository's
GitHub **Security** tab to create a private vulnerability report/private
security advisory. If private vulnerability reporting is unavailable, contact
the repository owner through a private channel shown on their GitHub profile;
do not include exploit details in a public discussion. GitHub documents the
private reporting flow in its
[security advisory guidance](https://docs.github.com/en/code-security/security-advisories/working-with-repository-security-advisories/privately-reporting-a-security-vulnerability).

Include affected versions, platform, impact, a minimal reproduction, and any
suggested mitigation. Redact Codex account data, task transcripts, repository
secrets, and local filesystem identities.

## Security boundaries

- The coordinator is local-only and supports exactly two tasks on one host.
- It connects to the local Codex App Server through Codex CLI and exposes only a
  private Unix socket. The state directory is mode `0700`; the socket and state
  files are restricted to the current user.
- The primary task is the sole writer to the source repository. The reviewer
  performs read-only inspection. The daemon performs authoritative read-only
  Git checks and materializes only coordinator-owned verification clones under
  the private state directory.
- Review App Server turns are read-only and offline. The integration turn is
  offline, can write only the primary worktree and source Git common directory,
  and is guarded by deterministic command approvals. It cannot run tests. The
  separate verification turn is offline and can write only a clean, detached,
  remote-free clone of the exact integration SHA; its Git common directory is
  independent of the source repository. It may run only exact frozen test
  commands. Forbidden publication, destructive Git, shell chaining, dynamic
  command launchers, and permission escalation are cancelled. Every turn
  explicitly disables inherited sticky execution environments.
- Both source refs and SHAs are frozen and revalidated. Integration may occur
  only on a unique new local branch.
- There is no remote push, PR creation, credential management, source-ref
  update, reset, rebase, deletion, or cleanup capability.
- Structured task responses are schema- and invariant-validated. Plan approval
  is bound to a canonical payload hash. Verification evidence is derived from
  successful App Server `commandExecution` items, including the exact turn,
  item, command, cwd, and exit code; model-reported evidence cannot replace it.
  Malformed or missing required App Server responses and unknown persisted-state
  versions fail closed.
- Sensitive App Server diagnostics containing authorization, key, secret, or
  token markers are redacted; home-directory prefixes are shortened.

## Stored data

SQLite stores task IDs, local worktree and verification-clone paths, Git refs
and SHAs, canonical protocol payloads, state transitions, and pending-send
metadata. It does not store credentials or full task transcripts. Codex task
histories retain coordinator prompts and task replies under the user's normal
Codex data controls. The managed daemon writes no persistent application log by
default.

Local task IDs, paths, commit metadata, and protocol payloads may still be
sensitive. Protect the host account and state directory, do not place the SQLite
database on a shared filesystem, and do not publish diagnostic output without
review.

## Out of scope

Security claims do not extend to a compromised Codex binary, Git binary, host
account, task model, operating system, or hostile code already committed to the
repository. Configured tests execute repository code with the user's local
identity inside the App Server sandbox; while their writable repository root is
an isolated clone with no remote, users must still review tests before running
them. The coordinator reduces accidental integration loss; it is not a sandbox
for adversarial project code.
