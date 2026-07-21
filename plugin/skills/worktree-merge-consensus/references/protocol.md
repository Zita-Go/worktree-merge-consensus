# Consensus Protocol Reference

## Preconditions

- Exactly two existing Codex tasks are selected on one host.
- Their committed heads are in different registered worktrees of the same Git common directory.
- The primary task is the only integration writer.
- The reviewer task protects the intent and implementation details of its frozen commit.

Task IDs and source worktrees are selected independently. A task's App Server cwd is orientation metadata only and may be identical or outside Git. The confirmed start operation freezes both task IDs, canonical registered worktree paths, commit SHAs, and source refs. A mismatch fails closed before integration.

Discovery uses `consensus_list_worktrees` with `repository_path`; start requires `primary_thread`, `reviewer_thread`, `primary_worktree`, and `reviewer_worktree`. `UNREGISTERED_WORKTREE`, `DUPLICATE_WORKTREE`, `REPOSITORY_MISMATCH`, `DIRTY_WORKTREE`, or `WORKTREE_UNAVAILABLE` stops preflight. A task that finds its explicitly bound source inconsistent with its conversation history returns `SOURCE_BINDING_MISMATCH`.

## Lifecycle

| Phase | Required outcome |
| --- | --- |
| `CONTRACT` | Both tasks independently describe behavior, constraints, tests, and protected details. |
| `PLAN_REVIEW` | The primary proposes coverage; the reviewer either identifies concrete gaps or approves the exact plan revision. |
| `INTEGRATE` | Only after exact plan approval, the primary creates a new local branch and integrates both frozen commits. |
| `VERIFY` | The coordinator creates a detached, remote-free clone of the exact result SHA. A separate primary turn runs each frozen test there exactly once; the daemon derives evidence from successful App Server command items and confirms both source refs are unchanged. |
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

An accepted status includes the run ID, new local integration branch, integration SHA, both frozen source SHAs, authoritative test evidence (`turn_id`, `item_id`, command, cwd, and exit code), and `source_refs_unchanged: true`. The coordinator does not publish the branch or merge it into an existing branch. A task's self-reported test result is never sufficient evidence.

## Recovery

Run state and pending sends are persisted in SQLite before dispatch. Restarting the daemon resumes runnable work idempotently. Use status to inspect a pause, resolve the reported external condition, then resume the same run ID. A contract or plan that declares a Git test pauses with `INVALID_TEST_COMMAND`; explicit resume may archive and replace only the exact completed pre-integration read-only turn after source revalidation and canonical item checks. Completed calls to this plugin's exact `consensus_list_threads`, `consensus_list_worktrees`, and `consensus_status` queries are retry-safe; mutating, external, and unknown MCP calls fail closed. Version 0.1.10 and later can recover the equivalent legacy 0.1.9 `BLOCKED` state while atomically reacquiring its repository lock. Version 0.1.12 applies the same safeguards to malformed model output in a pre-integration contract, primary-plan, or reviewer-plan-verdict turn. Version 0.1.13 gives each approval request a concrete top-level payload template and rejects identity values provided only under a nested object. Post-integration and side-effectful `INVALID_RESPONSE` states remain terminal. Version 0.1.14 explicitly selects the same-host `local` execution environment with each turn's pinned cwd; an empty environment selection would disable the task's command and file tools. It adds one narrow recovery: an exact pre-integration `BLOCKED / EXECUTION_TOOL_UNAVAILABLE` accepted from the primary may be replaced only when canonical history and the accepted response hash match, the blocker payload proves no writes, no command or file-change item is present, both frozen worktrees and refs remain unchanged, and the target branch is absent. Version 0.1.15 treats an App Server `proposedExecpolicyAmendment` as ignored metadata when returning one-time `accept`, never applies the proposal, and adds recovery for an exact first-integration `BLOCKED / FORBIDDEN_OPERATION` only when the pending turn is canonically failed or interrupted, contains no side-effect-capable item, both frozen sources remain unchanged and clean, and the target branch is absent. Any later-phase, side-effectful, incomplete, or mismatched case stays terminal. Other `BLOCKED` states remain terminal. Cancellation never deletes the integration branch or worktree state.
