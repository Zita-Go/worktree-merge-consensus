---
name: worktree-merge-consensus
description: Use when two existing Codex tasks on the same host have committed changes in separate worktrees of one Git repository and need a reviewed integration result that preserves both source refs.
---

# Worktree Merge Consensus

## Overview

Launch the persistent local coordinator for two-task worktree integration. Keep this skill limited to choosing the tasks and starting the run; the daemon owns the consensus workflow.

## Launch

1. Call `consensus_doctor`. Stop and report its exact error if the local binary, Codex App Server, Git, state directory, or daemon is unavailable or incompatible.
2. Call `consensus_list_threads`. Use only two existing Codex tasks returned by this host. They must have different task IDs and different worktree paths.
3. Assign one task as primary and the other as reviewer. If the user has not made that choice, present the eligible candidates and ask for the two roles before continuing.
4. Call `consensus_start` with `primary_thread` and `reviewer_thread`. Include `integration_branch` only when the user supplied a unique new branch name. Include `test_commands` only when the user supplied additional verification commands.
5. Report the returned `run_id` and initial status. State that the result will remain on a new local integration branch and both frozen source refs remain protected. End the launch turn.

The launcher does not conduct or relay review rounds. The persistent coordinator handles contracts, plan revisions, integration, verification, final approval, recovery, and fail-closed pauses.

## Follow-up controls

- Call `consensus_status` when the user asks for progress or the accepted result.
- Call `consensus_resume` only after the user resolves the reported pause reason.
- Call `consensus_cancel` only when the user requests cancellation. Cancellation preserves existing Git state.

Read [references/protocol.md](references/protocol.md) when explaining lifecycle states, acceptance evidence, or recovery behavior.
