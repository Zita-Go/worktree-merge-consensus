---
name: worktree-merge-consensus
description: Launch, observe, resume, or cancel a reviewed same-host integration between two existing Codex tasks whose committed changes live in separate worktrees of one Git repository. Use when both original development tasks should retain and apply their own conversation context while reaching consensus on a new local integration branch. Do not use inside coordinator-authored Primary or Reviewer turns for an already-active run.
---

# Worktree Merge Consensus

## Purpose

Git preserves code differences; the original Codex tasks preserve why those
changes were made.

Use this skill to keep both original development tasks involved in integration:

- Primary proposes how both implementations should be preserved.
- Reviewer checks the proposal using its own original task history.
- They revise the plan until Reviewer approves.
- Primary integrates only on a new local branch.
- The coordinator verifies the exact result SHA.
- Reviewer approves or rejects that tested SHA.

The tasks do not exchange complete transcripts or hidden reasoning. Each task
uses its own context while the coordinator exchanges bounded contracts, plans,
feedback, summaries, evidence, and verdicts.

## Role boundary

This is a launcher and operator skill. It is not a third review participant.

If a coordinator-authored prompt identifies the current task as an internal
Primary or Reviewer participant in an active Run, do not invoke this skill,
start another Run, or call operator tools. Follow that self-contained
participant prompt instead.

The launcher must not:

- manually relay messages between participants;
- write or approve either participant's proposal;
- edit the integration branch;
- call participant-only `consensus_apply_patch`;
- replace the coordinator with ordinary Git or task tools;
- push, publish, or modify either frozen source ref.

Participant replies follow `worktree-merge-consensus/v2`; the daemon, not this
launcher, validates their markers, identity, Git result, and test evidence.

## Trust and execution boundary

Participant turns run unattended with `approvalPolicy: never` and
`sandboxPolicy: dangerFullAccess`. Use only trusted tasks and trusted repository
contents. These permissions avoid per-command approval but are not an OS
security boundary. Do not request a global Codex permission change.

The accepted result remains on a new local integration branch. The coordinator
does not push it, merge it into an existing branch, or move either frozen source
ref. Verification is coordinator-owned and runs against the exact result SHA in
an isolated, remote-free clone.

## Tool usage

All `consensus_*` names are MCP tools, not shell executables. In particular,
never run `consensus_doctor` as an executable.

On first startup, the MCP server may take up to five minutes while it downloads,
verifies, and caches the matching native runtime.

Use only these operator tools:

- `consensus_doctor`
- `consensus_list_threads`
- `consensus_list_worktrees`
- `consensus_start`
- `consensus_status`
- `consensus_wait`
- `consensus_resume`
- `consensus_cancel`

Never call `consensus_apply_patch` from this launcher. Its injection, preflight,
authorization, and single-use enforcement belong to the coordinator.

## Start a Run

1. Call `consensus_doctor`. Stop and report its exact error if the plugin,
   runtime, Codex App Server, daemon, Git state, or scoped approval configuration
   is unavailable.
2. Call `consensus_list_threads`. Present all visible tasks and select two
   different task IDs as Primary and Reviewer. A task cwd is display metadata
   only; never infer a source worktree from it.
3. Obtain an absolute `repository_path` to any worktree in the intended
   repository and call `consensus_list_worktrees`.
4. Present every registered worktree with path, source ref or detached state,
   full HEAD SHA, availability, and cleanliness. Select two different,
   available, clean worktrees in one Git common directory.
5. Show one complete mapping:

   - `primary_thread` -> `primary_worktree`, source ref, and SHA
   - `reviewer_thread` -> `reviewer_worktree`, source ref, and SHA

   Task selection and worktree selection are independent. Ask the user to
   confirm the exact mapping and do not continue without confirmation.
6. Call `consensus_start` with all four task/worktree fields. Include
   `integration_branch` only when the user supplied a unique new branch name.
   Include `test_commands` only when the user supplied extra verification
   commands.
7. Report the returned `run_id` and initial status. Remind the user that the
   selected tasks run unattended and that the result remains on a new local
   branch with both source refs protected.

## Observe progress

Keep the launcher task active until the Run terminates or needs user action.

1. Start with `after_cursor: 0`.
2. Call `consensus_wait` with the Run ID and `timeout_ms: 25000`.
3. Advance `after_cursor` only to the returned `next_cursor`.
4. If `has_more` is true, call again immediately.
5. Continue after an empty timeout batch without treating it as a state change.

For each nonempty event batch, send one concise commentary update containing:

- `[stage/6 NAME]`;
- the current review round;
- the active role when available;
- the material public artifact or decision.

Explain contract goals, protected details, tests, plan changes, Reviewer
objections, integration branch/SHA, frozen verification exit codes, and final
approval. Preserve concrete rejection details. Do not dump raw JSON unless
asked, and never expose hidden reasoning, participant prompts, complete task
history, or raw command output.

After two consecutive empty waits, show at most one short liveness update using
the last known stage. Reset the timeout counter when a new event arrives.

## Terminal and paused states

For `ACCEPTED`, report the Run ID, local integration branch, exact integration
SHA, frozen test evidence, unchanged source refs, and no-push boundary.

For `PAUSED_USER_ACTION`, report the exact reason and required user action. Do
not call `consensus_resume` until the user resolves the condition and explicitly
authorizes continuation.

For `BLOCKED`, `CANCELLED`, or `INCOMPATIBLE_CODEX`, report the exact reason and
retained Git state. Do not create a replacement Run.

Call `consensus_cancel` only when the user explicitly requests cancellation.

The final response must be self-contained even when progress was already shown
in commentary.

## Recovery invariants

- If a Run ID exists, inspect and recover that same Run.
- Never create a replacement Run to bypass a pause or blocker.
- Never repeat a command or patch whose side effects are uncertain.
- Let the daemon decide whether an exact recovery is safe.
- If `consensus_resume` rejects recovery, report the result without bypassing it
  manually.
- If `consensus_start` returns `COMMUNICATION_FAILURE` before any Run ID exists,
  call `consensus_doctor` once, verify that no Run was created, and retry the
  exact confirmed mapping at most once.
- If observation was interrupted, call `consensus_status` once and continue
  `consensus_wait` from the last known cursor. If no cursor is available, start
  at `0` and summarize replayed events without presenting them as new.

Read [references/recovery.md](references/recovery.md) only when a Run is paused
or blocked and the user wants to recover it.

## Diagnostics

If no operator `consensus_*` tools are exposed:

1. Run `codex mcp list --json`.
2. Report whether `worktreeMergeConsensus` is absent, disabled, or failed to
   start.
3. Do not search for a `consensus_doctor` binary or substitute ordinary Git or
   task tools.
4. Use `codex-consensus doctor` only when a direct or centrally managed CLI
   binary is actually installed and the user requested CLI diagnostics.
5. Stop after reporting the actionable installation error.

Read [references/installation.md](references/installation.md) only for missing
tools, runtime download, checksum, architecture, timeout, legacy-skill, or
scoped-configuration failures.

## References

Load detailed references only when the current situation requires them:

- [protocol.md](references/protocol.md): lifecycle, participant response format,
  statuses, verification, and acceptance evidence.
- [installation.md](references/installation.md): plugin and runtime startup
  diagnostics.
- [participant-binding.md](references/participant-binding.md): direct versus
  ephemeral Primary binding and participant-tool preflight.
- [recovery.md](references/recovery.md): reason-oriented same-Run recovery and
  legacy migration eligibility.
