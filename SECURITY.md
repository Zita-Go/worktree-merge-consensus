# Security Policy

## Supported versions

Only the latest published 0.1.x release receives security fixes while the
project remains pre-1.0. The supported Codex adapter range is narrower and is
listed in [the compatibility policy](docs/compatibility.md).

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
- The primary task is the sole Git writer. The reviewer and daemon perform
  read-only Git inspection.
- Both source refs and SHAs are frozen and revalidated. Integration may occur
  only on a unique new local branch.
- There is no remote push, PR creation, credential management, source-ref
  update, reset, rebase, deletion, or cleanup capability.
- Structured task responses are schema- and invariant-validated. Unknown Codex
  protocol versions fail closed.
- Sensitive App Server diagnostics containing authorization, key, secret, or
  token markers are redacted; home-directory prefixes are shortened.

## Stored data

SQLite stores task IDs, local worktree paths, Git refs and SHAs, protocol
messages, state transitions, and pending-send metadata. It does not store
credentials. Codex task histories retain coordinator prompts and task replies
under the user's normal Codex data controls. The managed daemon writes no
persistent application log by default.

Local task IDs, paths, commit metadata, and protocol payloads may still be
sensitive. Protect the host account and state directory, do not place the SQLite
database on a shared filesystem, and do not publish diagnostic output without
review.

## Out of scope

Security claims do not extend to a compromised Codex binary, Git binary, host
account, repository hooks, task model, or operating system. The coordinator
reduces accidental integration loss; it is not a sandbox for untrusted code.
Configured test commands and repository code run with the primary task's normal
permissions.
