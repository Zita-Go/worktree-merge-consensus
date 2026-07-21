---
name: worktree-merge-consensus
description: Use when two existing Codex tasks on the same host have committed changes in separate worktrees of one Git repository and need a reviewed integration result that preserves both source refs.
---

# Worktree Merge Consensus

## Overview

Launch the persistent local coordinator for two-task worktree integration. Keep this skill limited to choosing the tasks and starting the run; the daemon owns the consensus workflow.

## Tool surface

`consensus_doctor` is an MCP tool, not a shell command. The same applies to every `consensus_*` name below. Call these names through the Codex tool interface. Never run `consensus_doctor` as an executable.

Codex starts the bundled MCP server with `codex-consensus mcp-server`. Do not start that foreground process manually during a normal plugin run. The CLI equivalents are:

- `consensus_doctor` → `codex-consensus doctor`
- `consensus_list_threads` → `codex-consensus threads list`
- `consensus_list_worktrees` → `codex-consensus worktrees list --repository <absolute-path>`
- `consensus_start` → `codex-consensus run` with both task IDs and both worktree paths
- `consensus_status` → `codex-consensus status <run-id>`
- `consensus_resume` → `codex-consensus resume <run-id>`
- `consensus_cancel` → `codex-consensus cancel <run-id>`

Use those CLI commands only for diagnostics or when the user explicitly requests the CLI surface. If no `consensus_*` MCP tools are exposed, run `codex mcp list --json`, `command -v codex-consensus`, and `codex-consensus doctor` when shell access is available. Report whether `worktreeMergeConsensus` is absent, disabled, or unable to start, then stop. A successful CLI doctor does not prove that the plugin MCP tools were loaded. Do not search for a `consensus_doctor` binary or substitute ordinary task/thread tools.

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
- If `consensus_start` returns `COMMUNICATION_FAILURE` before a `run_id` exists,
  call `consensus_doctor` once; v0.1.7 and later probe and repair the
  daemon-owned App Server proxy. Verify that no run was created, then retry the
  exact confirmed mapping once. If a run ID exists, inspect that run instead of
  creating a replacement.
- Call `consensus_resume` only after the user resolves the reported pause reason.
  For `COMMUNICATION_FAILURE`, explicit user authorization permits the
  coordinator to inspect the exact pending turn. It replaces a terminal
  `failed` or `interrupted` attempt only when canonical history proves that the
  attempt has no side-effectful items; otherwise it remains paused and fails
  closed.
- For `INVALID_TEST_COMMAND` from a contract or plan, explain that model-declared
  tests cannot invoke Git. After explicit user authorization, call
  `consensus_resume` on the same run. The coordinator revalidates both frozen
  sources and replaces the exact completed pre-integration read-only turn only
  when canonical history has no file change, incomplete command, mutating or
  external MCP call, or unknown item. Completed calls to this plugin's exact
  `consensus_list_threads`, `consensus_list_worktrees`, and `consensus_status`
  queries are retry-safe. Version 0.1.10 and later may also recover this reason
  from the legacy `BLOCKED` state created by 0.1.9; do not treat any other
  `BLOCKED` state as resumable.
- For a pre-integration `BLOCKED / INVALID_RESPONSE`, report the exact validation
  diagnostic. After explicit user authorization, call `consensus_resume` on the
  same run. Version 0.1.12 reactivates only contract, primary-plan, or
  reviewer-plan-verdict actions whose exact completed canonical turn passes the
  same read-only history checks. Never resume a post-integration, side-effectful,
  incomplete, external, or unknown invalid response, and never create a
  replacement run implicitly. Version 0.1.13 supplies concrete top-level payload
  templates for both approval message types and rejects approval identities that
  exist only under a nested object.
- For `BLOCKED / EXECUTION_TOOL_UNAVAILABLE` before an integration branch or SHA
  exists, report that the selected task lacked its same-host execution tools.
  After explicit user authorization, call `consensus_resume` on the same run.
  Version 0.1.14 retries only the exact accepted primary integration turn when
  canonical history and its response hash match, the response explicitly
  reports no writes, no command or file-change item exists, both frozen sources
  are unchanged and clean, and the target branch is absent. Any mismatch,
  integration identity, side effect, or later-phase blocker remains terminal.
- Call `consensus_cancel` only when the user requests cancellation. Cancellation preserves existing Git state.

Read [references/protocol.md](references/protocol.md) when explaining lifecycle states, acceptance evidence, or recovery behavior.
