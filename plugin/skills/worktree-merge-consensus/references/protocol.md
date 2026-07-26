# Consensus Protocol Reference

Use this reference when explaining lifecycle states, participant output,
verification evidence, or the accepted result. The launcher procedure remains
in `../SKILL.md`.

## Contents

- [Preconditions](#preconditions)
- [Lifecycle](#lifecycle)
- [Participant responses](#participant-responses)
- [Verification](#verification)
- [Statuses](#statuses)
- [Public observation](#public-observation)
- [Accepted result](#accepted-result)

## Preconditions

- Select exactly two existing Codex tasks on one host.
- Bind their committed heads to two different registered worktrees in one Git
  common directory.
- Require both source worktrees to be available, clean, and committed.
- Let only the Primary write the integration result.
- Let the Reviewer protect the intent and implementation details of its frozen
  source.

Task IDs and source worktrees are selected independently. A task cwd is display
metadata and may be identical for both tasks or outside Git. The confirmed start
freezes both task IDs, canonical worktree paths, source refs, and commit SHAs.
A mismatch fails before integration.

## Lifecycle

| Phase | Required outcome |
| --- | --- |
| `SOURCE_FREEZE` | Freeze the selected tasks, registered worktrees, refs, and SHAs. |
| `CONTRACT` | Both tasks independently declare behavior, constraints, tests, and protected details. |
| `PLAN_REVIEW` | Primary proposes complete coverage; Reviewer identifies concrete gaps or approves the exact revision. |
| `INTEGRATE` | After plan approval, Primary integrates both frozen commits on one unique new local branch. |
| `VERIFY` | The coordinator tests the exact result SHA in a detached, remote-free clone. |
| `RESULT_REVIEW` | Reviewer audits the exact SHA, result summary, and frozen test evidence. |
| `ACCEPTED` | The daemon revalidates the approved SHA and unchanged source refs, records the result, and stops. |

Review rounds are bounded. Repeated non-progress, malformed participant output,
incompatible Codex behavior, communication uncertainty, or a safety violation
pauses or blocks the Run instead of guessing.

## Participant responses

The participant protocol is `worktree-merge-consensus/v2`.

Every response contains exactly one
`<consensus-result>...</consensus-result>` marker. The initial contract pairs
that marker with one JSON object so test commands remain machine-readable.
Plans, change requests, approvals, integration summaries, verification
summaries, and final reviews use ordinary Markdown outside the marker.

The coordinator binds every response to the exact pending task turn and derives
plan identity, branch, SHA, changed files, ancestry, source-ref stability, and
test evidence itself. A participant's self-report is not authoritative evidence.

Read [participant-binding.md](participant-binding.md) only when a binding,
ephemeral task, or patch-tool preflight diagnostic needs explanation.

## Verification

Verification is coordinator-owned:

1. Create a detached, remote-free clone at the exact integration SHA.
2. Ask Primary for a marker-only readiness handoff; that participant turn must
   not run Shell, Git, file, MCP, or patch tools.
3. Journal each frozen direct command before dispatch.
4. Execute every command through App Server `command/exec` in order, continuing
   after failures so the evidence set is complete.
5. Record exact cwd, command identity, structured exit code, and bounded failure
   diagnostics.
6. Recheck both frozen source refs.

A completed exact result can be reused after restart. A command journaled as
started without a durable terminal result becomes
`VERIFICATION_EXECUTION_UNCERTAIN` and is never executed again automatically.
A failed command returns the same Run to a controlled correction round; only a
new SHA that passes every frozen command reaches final review.

## Statuses

- `RUNNING`: the daemon can dispatch the next deterministic action.
- `WAITING_THREAD`: one selected task has an active turn.
- `PAUSED_USER_ACTION`: an external condition or explicit decision is required.
- `ACCEPTED`: the exact result SHA passed frozen verification and Reviewer
  approval.
- `BLOCKED`: a terminal protocol or safety condition prevented acceptance.
- `CANCELLED`: cancellation was requested; existing Git state remains.
- `INCOMPATIBLE_CODEX`: the required App Server behavior is unavailable.

## Public observation

`consensus_wait` returns bounded, cursor-ordered public events. Resume with the
returned `after_cursor`; an empty timeout is not a state change.

The stream may include source identities, contracts, plans, review feedback,
integration summaries, frozen test exit codes, and acceptance evidence. It
excludes hidden reasoning, participant prompts, complete task history, private
SQLite data, and raw command stdout/stderr.

## Accepted result

An accepted result includes:

- Run ID;
- new local integration branch;
- exact integration SHA;
- both frozen source SHAs;
- coordinator-journaled test commands, cwd values, and exit codes;
- `source_refs_unchanged: true`;
- confirmation that nothing was pushed or merged into an existing branch.

Acceptance is bound to the exact tested SHA. Moving the result, changing a
source ref, or relying only on a participant's reported tests cannot satisfy it.

For a paused or blocked Run, read [recovery.md](recovery.md) instead of inferring
a recovery from this lifecycle summary.
