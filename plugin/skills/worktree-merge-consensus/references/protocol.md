# Consensus Protocol Reference

## Preconditions

- Exactly two existing Codex tasks are selected on one host.
- Their committed heads are in different worktrees of the same Git common directory.
- The primary task is the only integration writer.
- The reviewer task protects the intent and implementation details of its frozen commit.

The start operation freezes both task IDs, worktree paths, commit SHAs, and source refs. A mismatch fails closed before integration.

## Lifecycle

| Phase | Required outcome |
| --- | --- |
| `CONTRACT` | Both tasks independently describe behavior, constraints, tests, and protected details. |
| `PLAN_REVIEW` | The primary proposes coverage; the reviewer either identifies concrete gaps or approves the exact plan revision. |
| `INTEGRATE` | Only after exact plan approval, the primary creates a new local branch and integrates both frozen commits. |
| `VERIFY` | Configured tests run and Git safety checks confirm both frozen source refs are unchanged. |
| `RESULT_REVIEW` | The reviewer audits the exact integration SHA and evidence, then requests changes or approves that SHA. |
| `ACCEPTED` | The daemon revalidates the approved SHA and source refs, records the result, and stops. |

Review rounds are bounded. Repeated non-progress, malformed envelopes, incompatible Codex versions, communication failures, permission requests, or safety violations stop or pause the run instead of guessing.

## Statuses

- `RUNNING`: the daemon can dispatch the next deterministic action.
- `WAITING_THREAD`: one selected task has an active turn.
- `PAUSED_USER_ACTION`: explicit user action is required; inspect the reason before resuming.
- `ACCEPTED`: the exact integration SHA passed verification and reviewer approval.
- `BLOCKED`: a terminal protocol or safety condition prevented acceptance.
- `CANCELLED`: cancellation was requested; existing Git state remains intact.
- `INCOMPATIBLE_CODEX`: the local Codex version is outside the supported adapter set.

## Accepted result

An accepted status includes the run ID, new local integration branch, integration SHA, both frozen source SHAs, test evidence, and `source_refs_unchanged: true`. The coordinator does not publish the branch or merge it into an existing branch.

## Recovery

Run state and pending sends are persisted in SQLite before dispatch. Restarting the daemon resumes runnable work idempotently. Use status to inspect a pause, resolve the reported external condition, then resume the same run ID. Cancellation never deletes the integration branch or worktree state.
