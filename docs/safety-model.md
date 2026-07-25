# Safety Model

[简体中文](safety-model.zh-CN.md)

Worktree Merge Consensus reduces accidental loss of behavior and implementation
intent when two trusted Codex tasks integrate committed work. It is not an OS
sandbox, a security boundary for hostile code, or a replacement for human review
before production deployment.

## Trust assumptions

Every coordinator-started participant turn uses App Server approval policy
`never` and sandbox policy `dangerFullAccess`. The workflow is deliberately
unattended: Primary and Reviewer turns do not stop for per-command confirmation,
and the App Server does not provide OS containment for those turns.

Use the tool only when all of the following are trusted:

- the local host account and Codex installation;
- both selected tasks and their existing histories;
- both source commits and repository instructions;
- every frozen test command and the code it executes.

The coordinator can reject acceptance after detecting forbidden evidence or
state drift. It cannot undo an action that a participant or test process already
performed.

## Frozen identities

Before review, the coordinator freezes and persists:

- Primary and Reviewer task IDs;
- two canonical, registered worktree paths;
- the shared Git common directory;
- source refs, detached state where applicable, and full commit SHAs;
- the unique target integration branch;
- declared test commands.

Task selection and worktree selection are independent. App Server task cwd is
orientation metadata only and is never accepted as source identity. Both paths
must be different, clean registered worktrees from one Git common directory.
Source refs and SHAs are revalidated throughout the Run.

## Write ownership

Only the Primary may write the integration result, and only on the authorized
new local branch. The Reviewer is read-only. The coordinator performs
authoritative Git inspection and creates private verification clones, but it has
no capability to push, open a PR, update a source ref, rebase, reset, delete a
branch, manage credentials, or clean up a worktree.

The participant-only `consensus_apply_patch` capability is narrower than normal
repository access. It is bound to one active Run and request hash, accepts at
most one successful text-only patch of at most 512 KiB, rejects unsafe paths,
preflights with Git, revalidates frozen refs, and persists single-use provenance
in SQLite. It cannot select a repository, create a Run, choose a branch, or
publish a result. Its required approval setting is:

```text
plugins.worktree-merge-consensus.mcp_servers.worktreeMergeConsensus.tools.consensus_apply_patch.approval_mode = "approve"
```

`codex-consensus configure` writes and verifies only that key. A missing or
overridden value fails closed as `APPROVAL_CONFIGURATION_REQUIRED`.

## Verification evidence

The Primary verification turn is marker-only and must not run Shell, Git, file,
MCP, patch, or other tools. After the marker, the daemon invokes App Server
`command/exec` for every frozen direct non-Git command in a clean detached clone
of the exact integration SHA.

The verification clone:

- has a Git common directory independent from both source worktrees;
- has no remote;
- is pinned to the exact result SHA;
- is retained under the private state directory for audit and recovery.

Each command is journaled as STARTED before dispatch and COMPLETED with its exact
turn identity, deterministic item identity, command, cwd, exit code, and bounded
diagnostics. A completed exact result may be reused after restart. A command
whose completion cannot be proven stops as `VERIFICATION_EXECUTION_UNCERTAIN`
and is never automatically repeated. A model's self-reported success is not test
evidence.

## Participant binding

Before the first Primary action, the selected **Source Primary** is bound to an
**Effective Primary**. A `notLoaded` Source Primary binds directly after
task-scoped participant configuration is loaded. A preloaded Source Primary
without the exact participant tool uses an `ephemeral: true`, full-history
`thread/fork` after a null goal check. The mirror carries no active Source goal
and is inspected with `thread/read(includeTurns: false)`.

Before every Primary turn, the coordinator consumes every
`mcpServerStatus/list` page before `turn/start` and requires the participant
inventory to contain exactly `consensus_apply_patch`. Reviewer routing and both
frozen source identities remain unchanged. Pending or uncertain turns are never
reforked or resent, and an uncertain fork response is not automatically retried.
This binding contract requires Codex CLI `>=0.144.1` and the checked App Server
method and response shapes.

## Protocol and history checks

`worktree-merge-consensus/v2` uses one
`<consensus-result>...</consensus-result>` marker per participant response.
Contracts include one machine-readable JSON body; later prose is free-form
Markdown. The daemon binds each response to the exact pending task turn and
derives branch, SHA, changed files, ancestry, and test evidence independently.

Canonical history is rejected when it contains unexpected writes, publication,
destructive Git, shell chaining, wrong-directory operations, unknown items, or
ambiguous side effects. A single known App Server shell wrapper may be normalized
before the exact inner command policy is checked; nested wrappers and dynamic
launchers remain forbidden.

## State, events, and privacy

The default state directory is `$XDG_STATE_HOME/codex-consensus`, or
`~/.local/state/codex-consensus` when `XDG_STATE_HOME` is unset. It is mode
`0700`; its Unix socket and state files are restricted to the current user.

SQLite stores task IDs, local paths, refs, SHAs, protocol payloads, pending-send
metadata, structured evidence, and bounded public progress events. It does not
store credentials or complete task transcripts. Codex itself still retains
coordinator messages and participant replies under the user's normal Codex data
controls.

The public event stream may expose declared contracts, plans, Reviewer feedback,
integration identity, command names and exit codes, and final acceptance. It
excludes hidden reasoning, participant prompts, raw task history, and command
stdout/stderr.

## Out of scope

Safety claims do not extend to a compromised Codex binary, Git binary, host
account, operating system, task model, or hostile code already committed to the
repository. Tests execute with the App Server process user's local identity and
`dangerFullAccess`; a remote-free clone is Git isolation, not OS containment.

For vulnerability reporting and supported security-fix versions, read
[SECURITY.md](../SECURITY.md). For exact adapter and recovery boundaries, read
[Compatibility](compatibility.md) and [Recovery](recovery.md).
