---
name: worktree-merge-consensus
description: Use when the user asks to launch or control a reviewed integration of two existing Codex tasks on the same host with committed changes in separate worktrees of one Git repository. Do not use for an internal Primary or Reviewer participant turn whose coordinator prompt says the run is already active.
---

# Worktree Merge Consensus

## Overview

Launch the persistent local coordinator for two-task worktree integration. Keep this skill limited to choosing the tasks and starting the run; the daemon owns the consensus workflow.

This is a launcher and operator skill only. If a coordinator-authored prompt identifies the current task as the Primary or Reviewer participant inside an already-running automated consensus run, this skill is not applicable: do not read it recursively, call its tools, or start another run. Follow that self-contained participant prompt instead.

Participant protocol v2 uses exactly one
`<consensus-result>...</consensus-result>` marker per response. Only the initial
contract body is JSON; plans, review feedback, integration summaries,
verification summaries, and final reviews are free-form Markdown. The daemon,
not either task, binds machine identity and derives Git and test evidence. This
launcher never parses or relays those participant responses.

Release 0.2.5 starts every coordinator-owned participant turn with App Server
approval policy `never` and sandbox policy `dangerFullAccess`. Contract, plan,
integration, verification, and review therefore proceed without per-command
user confirmation or an App Server OS sandbox. Use unattended runs only with
trusted tasks and trusted repository contents. The daemon still binds exact
tasks, worktrees, refs, requests, turns, Git results, and acceptance evidence,
but those fail-closed checks do not undo an action already performed by a
participant. Do not request or apply a global Codex permission change.

Primary verification is a marker-only handoff to coordinator-owned verification:
do not run Shell in the verification marker turn. After the
exact side-effect-free marker completes, the daemon invokes App Server
`command/exec` itself for every frozen direct command, in order, with the exact
detached verification cwd and `sandboxPolicy.type: "dangerFullAccess"`. The
participant marker turn retains `approvalPolicy: "never"`. SQLite journals each
command before dispatch and reuses only a completed exact result; an uncertain
started command fails closed instead of being run again automatically.

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

Release 0.2.7 makes that participant capability coordinator-owned rather than
an inference from the operator plugin. For every Primary integration turn, the
coordinator injects the task-scoped `worktreeMergeConsensusParticipant` server
through `participant-mcp-server` and preflights `mcpServerStatus/list` with
`detail: "toolsAndAuthOnly"` before `turn/start`. The participant server must
expose exactly `consensus_apply_patch`; the operator plugin's eight tools do
not prove that the participant task can see it. Only the Primary integration
resume variant carries `config`; default, ordinary, and non-integration task
resumes remain `threadId`-only.

Use those CLI commands only for diagnostics or when the user explicitly requests the CLI surface. If no `consensus_*` MCP tools are exposed, run `codex mcp list --json`, `command -v codex-consensus`, and `codex-consensus doctor` when shell access is available. Report whether `worktreeMergeConsensus` is absent, disabled, or unable to start, then stop. A successful CLI doctor does not prove that the plugin MCP tools were loaded. Do not search for a `consensus_doctor` binary or substitute ordinary task/thread tools.

## Launch

1. Call `consensus_doctor`. Stop and report its exact error if the binary, plugin surface, Codex App Server, Git, private state, or daemon is unavailable or incompatible. For `LEGACY_SKILL_CONFLICT`, do not delete anything; give the returned migration guidance. For `APPROVAL_CONFIGURATION_REQUIRED`, tell the user to run the one-time `codex-consensus configure` installation command as the same account and `CODEX_HOME` used by Codex, then stop. Never relax global or command approvals.
2. Call `consensus_list_threads`. Present all visible tasks and assign two different task IDs as primary and reviewer. A task cwd is display metadata only: do not filter tasks by cwd or infer a source worktree from it.
3. Obtain an absolute `repository_path` to any worktree in the intended repository. Call `consensus_list_worktrees` with that path.
4. Present the registered entries with path, source ref or detached state, full HEAD SHA, and clean state. Assign two different, available, clean worktrees as primary and reviewer sources.
5. Show one complete mapping: `primary_thread` → `primary_worktree`/ref/SHA and `reviewer_thread` → `reviewer_worktree`/ref/SHA. Ask the user to confirm this exact mapping. Do not continue without confirmation.
6. Call `consensus_start` with all four required fields: `primary_thread`, `reviewer_thread`, `primary_worktree`, and `reviewer_worktree`. Include `integration_branch` only when the user supplied a unique new branch name. Include `test_commands` only when the user supplied additional verification commands.
7. Report the returned `run_id` and initial status. State that the coordinator's participant turns are unattended and unsandboxed, so the selected tasks and repository must be trusted; the result will remain on a new local integration branch, and both frozen source refs remain protected. End the launch turn.

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
  Version 0.1.27 also handles that exact `inProgress + waitingOnApproval` turn
  before a final assistant JSON exists, but only when its one request-bound
  patch item is canonically `failed`, all other items are complete and
  allowlisted, no successful patch is recorded, and the clean authorized merge
  SHA remains identical across interruption. Unknown items, ambiguous writes,
  target movement, or source drift remain terminal.
  Version 0.1.28 keeps the blocker's direct request, plan, source, target, and
  merge-SHA identity mandatory while treating `payload.role` and free-form
  `blocking_condition` as redundant diagnostics. The persisted pending send
  already binds the Primary task, and the paused Run rejects the patch before
  Git access. Missing machine identity still fails closed.
  Version 0.2.0 can also recover the exact case where one request-bound
  controlled patch and integration commit succeeded but the legacy final
  integration response was invalid. Same-Run resume must match the stored patch
  hash, canonical completed turn, authoritative clean target result, both
  ancestors, and frozen refs. It archives only that response attempt and asks
  the Primary for one read-only `INTEGRATION_READY` marker; a second patch,
  branch creation, or merge is forbidden.
  Version 0.2.1 may also recover one exact completed Primary verification turn
  that returned a result marker without executing any command. Explicit
  same-Run resume revalidates the unchanged integration result and isolated
  verification clone, requires zero command items and no side-effect-capable
  item, archives that empty turn, and retries the frozen verification request
  once. Partial execution, a second empty attempt, or drift remain terminal.
  Version 0.2.2 requires every frozen verification command to complete even
  after a nonzero exit. The marker means only that the evidence set is complete;
  the coordinator derives exit codes and bounded diagnostics. Failed commands
  return the same Run to a new controlled Primary integration round, and only a
  fully passing new SHA proceeds to Reviewer result review. After repairing the
  local toolchain, explicit resume may also replace one exact completed,
  side-effect-free `CARGO_UNAVAILABLE` verification blocker. That recovery is
  limited to one attempt and preserves the Run, integration branch, and frozen
  source refs.
  Version 0.2.3 persists App Server item lifecycle events and the exact
  `turn/completed` barrier in private SQLite before accepting a participant
  response. This is the authoritative command and controlled-tool evidence
  path when `thread/read` omits those items; full historical items remain a
  compatible fallback. For a pre-0.2.3 Run with the exact archived sequence of
  one empty verification and one side-effect-free `CARGO_UNAVAILABLE` recovery,
  explicit resume may replace one subsequent verification turn whose persisted
  command evidence is absent. The compatibility retry is atomic, one-time, and
  cannot repeat any patch, branch creation, merge, or source-ref update.
  Network, added-permission, later-phase, mismatched, or side-effectful cases
  remain terminal.
  Version 0.2.4 sends approval policy `never` for every coordinator-started
  turn, including integration and isolated verification. Do not ask the user to
  approve individual participant commands; inspect `consensus_status` if the
  Run pauses because event evidence or a sandbox boundary rejected an action.
  Version 0.2.5 additionally sends `dangerFullAccess` for every participant
  turn. The Primary verification turn is marker-only and must contain no Shell,
  Git, file, MCP, or patch item; the coordinator then executes the frozen
  commands itself through `command/exec` in the exact detached clone. Completed
  command results are journaled and reused, while a persisted STARTED result is
  `VERIFICATION_EXECUTION_UNCERTAIN` and is never retried automatically. One
  migration is available only for the exact blocked 0.2.4 history already
  recorded by the daemon; it archives only the final side-effect-free legacy
  verification turn and preserves the same Run, integration branch, SHA,
  successful patch record, merge, commit, and frozen refs. Any different
  history, side effect, drift, prior migration, or changed identity remains
  terminal.
  If a v0.2.5 Run instead blocks with the exact SQLite diagnostic
  `UNIQUE constraint failed: turn_event_completions.turn_record_id` after that
  migration, install matching v0.2.6 binary and plugin artifacts, then restart
  the daemon once. Do not call `consensus_resume` again. v0.2.6 revalidates and
  continues the same Run during startup; observe it with `consensus_status`.
  Every near-match remains blocked without mutation.
- If the exact post-v0.2.6 production Run is `BLOCKED` with
  `CONTROLLED_PATCH_TOOL_UNAVAILABLE`, install matching v0.2.7 binary and
  plugin artifacts, then explicitly call `consensus_resume`. The Run, round,
  branch, old SHA, and failed verification evidence must match; the coordinator
  archives only the empty correction turn, reacquires the lock, preflights the
  participant server, and retries one request-bound correction patch. The new
  SHA must advance and all frozen verification reruns. Installation alone does
  not mutate the blocked Run; every near-match remains terminal.
- Call `consensus_cancel` only when the user requests cancellation. Cancellation preserves existing Git state.

Read [references/protocol.md](references/protocol.md) when explaining lifecycle states, acceptance evidence, or recovery behavior.
