# Security Policy

## Supported versions

Only the latest published pre-1.0 release receives security fixes. The
supported Codex version floor and runtime adapter checks are listed in
[the compatibility policy](docs/compatibility.md).

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
- Every coordinator-started participant turn uses approval policy `never` and
  sandbox policy `dangerFullAccess`, so no participant action waits for
  interactive user confirmation and no App Server OS sandbox contains it. This
  mode is only for trusted tasks and trusted repository contents. Prompts,
  canonical history, request identity, Git invariants, and final evidence checks
  fail closed, but they cannot undo a participant action already performed.
  Forbidden publication, destructive Git, shell chaining, dynamic command
  launchers, and unexpected side effects reject acceptance when observed. The
  command gate
  removes at most one App Server-generated known-shell wrapper before checking
  the exact inner command; nested shells, subcommand callbacks, and non-local
  environments fail the Run. Every turn explicitly disables inherited sticky
  execution environments.
- The Primary verification participant turn is marker-only and rejects Shell,
  Git, file, MCP, patch, or other side-effect-capable items. The coordinator
  then runs each exact frozen direct command through App Server `command/exec`
  in a clean, detached, remote-free clone of the integration SHA whose Git
  common directory is independent of the source repository. Each command is
  journaled STARTED before dispatch and COMPLETED with its structured result.
  An exact completed result may be reused after restart; uncertain STARTED
  execution fails closed as `VERIFICATION_EXECUTION_UNCERTAIN` and is not
  automatically repeated.
- Both source refs and SHAs are frozen and revalidated. Integration may occur
  only on a unique new local branch.
- Task IDs and source worktrees are selected independently. App Server task cwd
  metadata is never trusted as source identity; preflight accepts only two
  different clean paths registered in one Git common directory, and every turn
  receives its frozen explicit path and workspace root.
- There is no remote push, PR creation, credential management, source-ref
  update, reset, rebase, deletion, or cleanup capability.
- Structured task responses are schema- and invariant-validated. Plan approval
  is bound to a canonical payload hash. Verification evidence is derived from
  coordinator-journaled App Server `command/exec` results, including the exact
  turn, deterministic item identity, command, cwd, and exit code;
  model-reported evidence cannot replace it.
  Malformed or missing required App Server responses and unknown persisted-state
  versions fail closed.
- Sensitive App Server diagnostics containing authorization, key, secret, or
  token markers are redacted; home-directory prefixes are shortened.
- The public observation stream is a bounded allowlist of declared contracts,
  plans, review verdicts, integration identity, test command/exit-code evidence,
  and final acceptance. It excludes hidden reasoning, participant prompts, raw
  task history, and command stdout/stderr. Each artifact is capped at 48 KiB
  and each response at six events before it crosses the local MCP boundary.
- A manually installed legacy Skill with the plugin's public name is reported
  as `LEGACY_SKILL_CONFLICT`. Diagnostics never delete or overwrite it; users
  must migrate it manually and restart Codex or open a new task.

## Stored data

SQLite stores task IDs, local worktree and verification-clone paths, Git refs
and SHAs, canonical protocol payloads, state transitions, bounded public
progress events, and pending-send metadata. It does not store credentials or
full task transcripts. Codex task histories retain coordinator prompts, task
replies, and public progress rendered by the launcher under the user's normal
Codex data controls. The managed daemon writes no persistent application log by
default.

Local task IDs, paths, commit metadata, and protocol payloads may still be
sensitive. Protect the host account and state directory, do not place the SQLite
database on a shared filesystem, and do not publish diagnostic output without
review.

## Out of scope

Security claims do not extend to a compromised Codex binary, Git binary, host
account, task model, operating system, or hostile code already committed to the
repository. Participant turns and configured tests execute with the App Server
process user's local identity and `dangerFullAccess`; the test cwd is an
isolated clone with no remote, but that cwd is not an OS containment boundary.
Users must trust the selected tasks and review repository code and tests before
running them. The coordinator reduces accidental integration loss; it is not a
sandbox for adversarial project code.
