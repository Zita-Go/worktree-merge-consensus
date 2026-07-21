---
name: worktree-merge-consensus
description: Use when the user asks to launch or control a reviewed integration of two existing Codex tasks on the same host with committed changes in separate worktrees of one Git repository. Do not use for an internal Primary or Reviewer participant turn whose coordinator prompt says the run is already active.
---

# Worktree Merge Consensus

## Overview

Launch the persistent local coordinator for two-task worktree integration. Keep this skill limited to choosing the tasks and starting the run; the daemon owns the consensus workflow.

This is a launcher and operator skill only. If a coordinator-authored prompt identifies the current task as the Primary or Reviewer participant inside an already-running automated consensus run, this skill is not applicable: do not read it recursively, call its tools, or start another run. Follow that self-contained participant prompt instead.

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

`codex-consensus configure` is a one-time installation command, not an MCP
tool. It writes and verifies only
`plugins.worktree-merge-consensus.mcp_servers.worktreeMergeConsensus.tools.consensus_apply_patch.approval_mode = "approve"`.
Do not replace it with a global approval change.

The eighth tool, `consensus_apply_patch`, intentionally has no public CLI
equivalent. It is not an operator or launcher tool. Only a coordinator-authored
Primary integration prompt may call it with the exact active run ID and request
hash; its daemon-side policy is described under recovery below. Never call it
manually from this launcher skill.

Use those CLI commands only for diagnostics or when the user explicitly requests the CLI surface. If no `consensus_*` MCP tools are exposed, run `codex mcp list --json`, `command -v codex-consensus`, and `codex-consensus doctor` when shell access is available. Report whether `worktreeMergeConsensus` is absent, disabled, or unable to start, then stop. A successful CLI doctor does not prove that the plugin MCP tools were loaded. Do not search for a `consensus_doctor` binary or substitute ordinary task/thread tools.

## Launch

1. Call `consensus_doctor`. Stop and report its exact error if the binary, plugin surface, Codex App Server, Git, private state, or daemon is unavailable or incompatible. For `LEGACY_SKILL_CONFLICT`, do not delete anything; give the returned migration guidance. For `APPROVAL_CONFIGURATION_REQUIRED`, tell the user to run the one-time `codex-consensus configure` installation command as the same account and `CODEX_HOME` used by Codex, then stop. Never relax global or command approvals.
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
- For a first-integration `BLOCKED / FORBIDDEN_OPERATION`, report the denied
  execution boundary and do not create a replacement run. After explicit user
  authorization, call `consensus_resume` on the same run. Version 0.1.15
  retries only an exact canonically `failed` or `interrupted` primary turn with
  no side-effect-capable item, no integration identity, both frozen sources
  unchanged and clean, and the target branch absent. An App Server
  `proposedExecpolicyAmendment` does not itself request more access when the
  coordinator returns one-time `accept`; the amendment is never applied.
  Version 0.1.16 also recognizes the App Server's one known-shell `-c` or `-lc`
  wrapper, removes it exactly once, and applies the existing command policy to
  the inner script. Nested shell launchers, non-null subcommand approval IDs,
  non-local environments, and added permissions remain forbidden.
  Version 0.1.17 permits only the exact target-branch existence preflight
  `git show-ref --verify refs/heads/<target-integration-branch>`. Version 0.1.19
  also permits only the equivalent exact
  `git branch --list <target-integration-branch>` query; every other `git branch`
  form remains forbidden. The same-run
  recovery may retain canonically terminal read-only Git queries only when
  every query used the frozen primary cwd and still passes the integration
  allowlist. Version 0.1.20 marks coordinator-authored Primary and Reviewer
  prompts as internal participant turns for which this launcher is inapplicable.
  Recovery may discard the exact denied legacy `sed -n 1,240p` read of this
  plugin's semver-versioned `SKILL.md`; that read never enters the live command
  allowlist. Version 0.1.21 treats App Server's internal `contextCompaction`
  lifecycle marker as retry-safe only when it contains exactly a nonempty `id`
  and the fixed `type`. Extra fields, writes, wrong cwd, and unknown items
  remain terminal; `inProgress` remains terminal except for the exact
  v0.1.24 controlled-patch approval recovery below.
  Version 0.1.22 additionally permits only `rg --files -g AGENTS.md` in the
  frozen primary cwd for repository-instruction discovery. Other `rg` forms
  remain denied; subsequent tracked-file reads use the read-only Git allowlist.
  Version 0.1.23 adds a bwrap-independent controlled patch path for the exact
  Primary integration request. `consensus_apply_patch` accepts one text-only
  patch of at most 512 KiB only when the run, request hash, pending Primary
  turn, clean authorized target branch, both frozen ancestors, and unchanged
  source refs all match. Git validates the patch without unsafe paths before
  applying it, and SQLite permits only one successful patch for that request.
  A `COMMUNICATION_FAILURE` pause whose exact completed Primary response reports
  `FILE_CHANGE_TOOL_UNAVAILABLE` may retry the same Run only when canonical
  history, approved identity, bwrap permission evidence, reported merge SHA,
  clean target branch, both source ancestors, and frozen source refs all match.
  The existing merge is retained; no replacement Run, branch recreation, or
  second merge is allowed.
  Version 0.1.24 requires the exact per-tool `approval_mode = "approve"`
  setting above before starting a Run or resuming controlled patch work. After
  explicit same-Run resume, a canonically `waitingOnApproval` Primary turn may
  be interrupted and retried only when it contains exactly one request-bound
  `inProgress` `consensus_apply_patch` call, all command items completed
  successfully and still pass the allowlist, no patch was recorded, the
  authorized target remains clean with both source ancestors, and frozen refs
  are unchanged. Unknown, multiple, or mismatched calls, other incomplete
  items, drift, or possible writes fail closed. If the turn completed during
  the interrupt race, its completed result is reused instead of duplicated.
  Version 0.1.25 also handles the exact completed rejection produced when App
  Server continues the old approval after configuration hot reload but before
  the paused Run is reactivated. Explicit same-Run resume may archive and retry
  only one canonically `failed`, request-bound `consensus_apply_patch` call with
  an exact `BLOCKED / PATCH_NOT_AUTHORIZED` response, no successful patch
  record, a clean authorized target at the reported merge SHA, both frozen
  ancestors, and unchanged source refs. It reuses the existing merge; unknown
  or additional tools, ambiguous writes, mismatched evidence, or drift remain
  terminal.
  Version 0.1.26 also handles only that exact failed call and blocker when App
  Server has persisted the final assistant JSON but leaves the turn
  `inProgress` with `waitingOnApproval`. Same-Run resume revalidates every
  0.1.25 condition, interrupts only that stale turn, and atomically archives it
  before retry. Participant waits use a 30-minute canonical-inactivity window;
  canonical status or turn-history changes renew it, while unchanged active
  state remains bounded.
  Network, added-permission, later-phase, mismatched, or side-effectful cases
  remain terminal.
- Call `consensus_cancel` only when the user requests cancellation. Cancellation preserves existing Git state.

Read [references/protocol.md](references/protocol.md) when explaining lifecycle states, acceptance evidence, or recovery behavior.
