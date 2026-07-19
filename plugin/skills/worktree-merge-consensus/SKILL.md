---
name: worktree-merge-consensus
description: Use when two existing Codex tasks on the same host have committed changes in separate worktrees of one Git repository and need a reviewed integration result that preserves both source refs.
---

# Worktree Merge Consensus

## Overview

Launch the persistent local coordinator for two-task worktree integration. Keep this skill limited to choosing the tasks and starting the run; the daemon owns the consensus workflow.

## Launch

1. Call `consensus_doctor`. Stop and report its exact error if the binary, plugin surface, Codex App Server, Git, private state, or daemon is unavailable or incompatible. For `LEGACY_SKILL_CONFLICT`, do not delete anything; give the returned migration guidance.
2. Call `consensus_list_threads`. Present all visible tasks and assign two different task IDs as primary and reviewer. A task cwd is display metadata only: do not filter tasks by cwd or infer a source worktree from it.
3. Obtain an absolute `repository_path` to any worktree in the intended repository. Call `consensus_list_worktrees` with that path.
4. Present the registered entries with path, source ref or detached state, full HEAD SHA, and clean state. Assign two different, available, clean worktrees as primary and reviewer sources.
5. Show one complete mapping: `primary_thread` → `primary_worktree`/ref/SHA and `reviewer_thread` → `reviewer_worktree`/ref/SHA. Ask the user to confirm this exact mapping. Do not continue without confirmation.
6. Call `consensus_start` with all four required fields: `primary_thread`, `reviewer_thread`, `primary_worktree`, and `reviewer_worktree`. Include `integration_branch` only when the user supplied a unique new branch name. Include `test_commands` only when the user supplied additional verification commands.
7. Report the returned `run_id` and initial status. State that the result will remain on a new local integration branch and both frozen source refs remain protected. End the launch turn.

The launcher does not conduct or relay review rounds. The persistent coordinator handles contracts, plan revisions, integration, verification, final approval, recovery, and fail-closed pauses.

## Follow-up controls

- Call `consensus_status` when the user asks for progress or the accepted result.
- Call `consensus_resume` only after the user resolves the reported pause reason.
- Call `consensus_cancel` only when the user requests cancellation. Cancellation preserves existing Git state.

Read [references/protocol.md](references/protocol.md) when explaining lifecycle states, acceptance evidence, or recovery behavior.
