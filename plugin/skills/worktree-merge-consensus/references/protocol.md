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
| `VERIFY` | The coordinator creates a detached, remote-free clone of the exact result SHA. A separate Primary marker-only turn performs no tools. The daemon then runs each frozen command itself through App Server `command/exec`, in order and continuing after failures; it journals exact structured results, derives bounded diagnostics, and confirms both source refs are unchanged. A failed command returns the same Run to another controlled integration round. |
| `RESULT_REVIEW` | The reviewer audits the exact integration SHA and evidence, then requests changes or approves that SHA. |
| `ACCEPTED` | The daemon revalidates the approved SHA and source refs, records the result, and stops. |

Review rounds are bounded. Repeated non-progress, malformed envelopes, incompatible Codex versions, communication failures, permission requests, or safety violations stop or pause the run instead of guessing.

## Participant responses

Release 0.2.0 uses `worktree-merge-consensus/v2`. Every response has exactly
one `<consensus-result>...</consensus-result>` marker. Contracts pair the marker
with one JSON object so exact test commands remain machine-readable. Plans,
change requests, approvals, integration and verification summaries, and result
reviews use ordinary Markdown outside the marker. The coordinator binds the
response to its pending task turn, computes plan identity, and derives branch,
SHA, changed files, ancestry, source-ref stability, and test evidence itself.
Valid v1 JSON envelopes remain accepted only as a migration fallback.

## Primary participant binding and patch-tool preflight

Before the first Primary action, the coordinator establishes a durable
participant binding. The frozen selected task is the **Source Primary**. A
`notLoaded` Source Primary is loaded with task-scoped
`worktreeMergeConsensusParticipant` MCP configuration and binds directly as
the **Effective Primary**. A preloaded Source Primary with the exact tool also
binds directly. A preloaded Source Primary without that capability is not
mutated in place: `thread/fork` creates an `ephemeral: true`,
`excludeTurns: false` full-history mirror with the participant configuration.
Before forking, the coordinator requires `thread/goal/get: null` on the Source
Primary and omits goal carry or continuation from the fork request. It then
requires matching canonical turn IDs, idle mirror status, and an exact
inventory before using that mirror. The mirror represents the Source Primary;
it is not a third source or reviewer and does not carry an active Source goal.
The coordinator never queries a goal on the ephemeral mirror because supported
Codex runtimes may reject goal operations for ephemeral tasks.

Before every Primary turn, the coordinator resumes the Effective Primary and
fully paginates `mcpServerStatus/list` before `turn/start`. The only accepted
participant tool inventory is exactly `consensus_apply_patch`; the operator
plugin's separate tools do not prove participant visibility. Reviewer routing
is unchanged, and both selected source task IDs, refs, worktrees, and SHAs stay
frozen. A mirror may be recreated only between completed actions. A pending or
uncertain turn is never reforked or resent, and an uncertain `thread/fork`
response is never retried automatically. The required experimental App Server
surface begins at Codex CLI `>=0.144.1`.

## Statuses

- `RUNNING`: the daemon can dispatch the next deterministic action.
- `WAITING_THREAD`: one selected task has an active turn.
- `PAUSED_USER_ACTION`: explicit user action is required; inspect the reason before resuming.
- `ACCEPTED`: the exact integration SHA passed verification and reviewer approval.
- `BLOCKED`: a terminal protocol or safety condition prevented acceptance.
- `CANCELLED`: cancellation was requested; existing Git state remains intact.
- `INCOMPATIBLE_CODEX`: the local Codex version is outside the supported adapter set.

## Accepted result

An accepted status includes the run ID, new local integration branch,
integration SHA, both frozen source SHAs, coordinator-journaled test evidence
(`turn_id`, deterministic command item ID, command, cwd, and exit code), and
`source_refs_unchanged: true`. The coordinator does not publish the branch or
merge it into an existing branch. A task's self-reported test result is never
sufficient evidence.

## Recovery

Version 0.1.24 requires the effective per-tool setting
`plugins.worktree-merge-consensus.mcp_servers.worktreeMergeConsensus.tools.consensus_apply_patch.approval_mode = "approve"`
before Run start or controlled-patch resume. After explicit same-Run resume, a
canonically `waitingOnApproval` Primary integration turn may be interrupted and
retried only when it contains exactly one request-bound `inProgress`
`consensus_apply_patch` call, every command item completed successfully and
still passes the integration allowlist, no successful patch is recorded, the
authorized target is clean with both frozen ancestors, and frozen refs remain
unchanged. Unknown or multiple calls, other incomplete items, mismatched
arguments, drift, and possible writes fail closed. A turn that completes during
the interrupt race is reused, not duplicated. This is the sole exception to the
summary below that otherwise treats `inProgress` as terminal.

Version 0.1.25 covers the exact completed rejection race after configuration
hot reload. If App Server continues the old approval while the Run remains
paused, the daemon rejects the patch with `PATCH_NOT_AUTHORIZED`. Explicit
same-Run resume may archive and retry that Primary integration turn only when
canonical history contains exactly one request-bound `consensus_apply_patch`
item with status `failed`, the final blocker carries the exact approved
identity, no successful patch record exists, the authorized target is clean at
the reported merge SHA with both frozen ancestors, and source refs remain
unchanged. The existing merge is reused; any unknown item, additional tool,
possible write, mismatch, or drift fails closed.

Version 0.1.26 covers the equivalent App Server residue in which that exact
failed tool item and final blocker are canonical but the turn remains
`inProgress` with `waitingOnApproval`. Same-Run resume performs all 0.1.25
checks, interrupts only that stale turn, and atomically archives it before
retrying the same request. Participant waits use a 30-minute inactivity window
that renews on changes to canonical task status or turn history; unchanged
active state still times out and fails closed.

Version 0.1.27 covers the immediately earlier residue in which the exact patch
item is already canonically `failed` but no final assistant JSON exists. It is
retryable only when every other item is complete and allowlisted, no successful
patch is recorded, and the clean authorized merge SHA is unchanged across the
single-turn interruption. Unknown items, possible writes, target movement, or
source drift fail closed.

Version 0.1.28 retains all direct machine identity checks but permits the
redundant `payload.role` label and free-form `blocking_condition` prose to be
absent. The persisted pending send already binds the exact Primary task, and
the paused daemon state rejects the controlled patch before Git access. Missing
request, plan, source, target, or result-SHA identity remains terminal.

Version 0.2.0 additionally permits same-Run recovery after one controlled patch
was recorded successfully and the integration commit is already present, but
the legacy final integration response was invalid. The daemon must match the
exact completed turn, stored patch hash, authoritative clean Git result, both
frozen ancestors, and unchanged refs. It then requests one read-only
`INTEGRATION_READY` marker and forbids a second patch, branch creation, or
merge.

Version 0.2.1 permits one additional same-Run recovery only when the exact
completed Primary verification turn contains zero command items. Resume
revalidates the unchanged integration result and isolated clone, rejects
unknown or side-effect-capable items, archives the empty turn, and retries the
same frozen verification request once. Partial execution, a second empty
response, or drift remains terminal.

Version 0.2.2 makes `VERIFICATION_READY` a completeness marker rather than a
pass verdict. Every frozen command must reach a terminal state; the coordinator
derives its exit code and bounded failure output. Nonzero results become
machine feedback for a new controlled Primary integration round in the same
Run, followed by verification of the new SHA. Reviewer result review starts
only after all commands pass. Explicit same-Run resume may also replace one
exact completed, side-effect-free `CARGO_UNAVAILABLE` verification blocker
after the environment is repaired. A second environment retry, source drift,
or integration drift remains terminal.

Version 0.2.3 persists `item/started`, `item/completed`, and the ordered
`turn/completed` barrier while each participant turn is active. The daemon
combines those exact-turn lifecycle items with stored user and final-agent
messages when App Server history omits command or MCP items. Older full
`thread/read` histories remain a fallback. A pre-0.2.3 Run may use one atomic
evidence-compatibility retry only after the exact archived sequence of one
empty verification attempt and one side-effect-free `CARGO_UNAVAILABLE`
recovery. It cannot repeat a controlled patch, branch creation, merge, or
source-ref update, and a second evidence retry is terminal.

Version 0.2.4 starts every participant turn with App Server approval policy
`never`. No contract, plan, integration, verification, or result-review action
requires interactive command or file approval. The coordinator still pins the
offline sandbox and writable roots and rejects acceptance unless exact command,
Git, test, and frozen-ref evidence is valid.

Version 0.2.5 replaces those participant sandbox profiles with
`dangerFullAccess`. This is a trusted-tasks execution boundary, not OS-level
containment: every selected task and the repository contents must be trusted.
Prompts and canonical-history checks still constrain role behavior and fail
closed, but cannot undo an already executed participant action. The Primary
verification response is only a side-effect-free `VERIFICATION_READY` marker;
it must not run Shell, Git, file, MCP, or patch tools. The coordinator then
executes every frozen direct argv command through App Server `command/exec`
with the exact detached verification cwd, `sandboxPolicy.type:
"dangerFullAccess"`, a bounded timeout, and a 65,536-byte output cap. The
participant marker turn retains `approvalPolicy: "never"`. Each command is
journaled as STARTED before dispatch and COMPLETED after its structured result.
An exact COMPLETED row is reused after restart; a STARTED row produces
`VERIFICATION_EXECUTION_UNCERTAIN` and is never executed a second time
automatically.

Version 0.2.7 permits an explicit same-Run recovery only for the exact
post-0.2.6 `CONTROLLED_PATCH_TOOL_UNAVAILABLE` correction blocker. It preserves
the same Run, round, integration branch, old SHA, and failed frozen verification
evidence; archives only the empty side-effect-free correction turn; reacquires
the lock; and repeats participant preflight. One request-bound correction patch
and commit may advance the SHA, after which every frozen verification command
runs again. Matching 0.2.8 installation alone does not mutate the blocked Run;
explicit resume is required and every near-match remains terminal.

Release 0.2.8 makes ephemeral Primary execution summary- and event-backed.
The coordinator never requests full history, turn lists, or resume for an
ephemeral binding. It uses `thread/read(includeTurns: false)` for liveness,
persists all matching item and terminal-turn events, and reconstructs the
canonical terminal turn from that journal. The frozen Source-history hash and
pre-dispatch start intent prevent changed-history reforks and duplicate sends.
Missing terminal evidence fails closed. Stored Source, Reviewer, and direct
Primary tasks retain full-history recovery.

Release 0.2.9 separates canonical terminal shape from side-effect policy when
auditing a completed integration turn. Approved writes remain valid only as
`completed` with exit code zero. Retry-safe read-only inspections may terminate
with a numeric nonzero code, but that result is not considered a successful
check; it is only safe to archive before a fresh read-only confirmation.
Explicit resume is limited to the exact turn whose request-bound controlled
patch and integration commit already succeeded. The same Run revalidates the
patch record, frozen refs, clean target result, both source ancestors, and
final SHA, archives only the rejected response, and forbids all repeated
writes. One recovery-only `git diff --no-index -- /dev/null <relative-path>`
shape is recognized only in historical evidence and never enters the live
approval allowlist. An explicit null `pluginId` is compatible only with the
exact injected participant server and patch tool.

Release 0.2.10 uses the integration-in-progress repository check before that
completed-turn recovery. The Primary worktree may already be attached to the
exact authorized target branch, while the frozen source refs and Reviewer
worktree must remain unchanged. The existing authoritative target, patch
record, source ancestry, cleanliness, changed-file, and final-SHA checks still
run before the response attempt is archived.

Release 0.2.11 recognizes `unifiedExecStartup` as canonical agent-initiated
command provenance during recovery. It continues to reject `userShell`,
`unifiedExecInteraction`, null, malformed, and unknown sources, and does not
weaken the command, terminal-result, side-effect, frozen-state, or target-result
checks.

The same release contains one migration only for the exact legacy 0.2.4
blocked-verification history: the same Run, Primary task, request, round,
verification clone, integration branch and SHA, frozen refs, and three archived
signals `VERIFICATION_READY`, `BLOCKED:CARGO_UNAVAILABLE`,
`VERIFICATION_READY`, with exactly one prior evidence-compatibility archive and
one final completed side-effect-free marker turn. Resume atomically archives
only that final turn as `completed-unattended-verification-migration` and
restores `REQUEST_PRIMARY_VERIFICATION`. It cannot repeat a patch, branch
creation, merge, commit, or source update, and cannot be applied twice.

Run state and pending sends are persisted in SQLite before dispatch. Restarting the daemon resumes runnable work idempotently. Use status to inspect a pause, resolve the reported external condition, then resume the same run ID. A contract or plan that declares a Git test pauses with `INVALID_TEST_COMMAND`; explicit resume may archive and replace only the exact completed pre-integration read-only turn after source revalidation and canonical item checks. Completed calls to this plugin's exact `consensus_list_threads`, `consensus_list_worktrees`, and `consensus_status` queries are retry-safe; mutating, external, and unknown MCP calls fail closed. Version 0.1.10 and later can recover the equivalent legacy 0.1.9 `BLOCKED` state while atomically reacquiring its repository lock. Version 0.1.12 applies the same safeguards to malformed model output in a pre-integration contract, primary-plan, or reviewer-plan-verdict turn. Version 0.1.13 gives each approval request a concrete top-level payload template and rejects identity values provided only under a nested object. Post-integration and side-effectful `INVALID_RESPONSE` states remain terminal. Version 0.1.14 explicitly selects the same-host `local` execution environment with each turn's pinned cwd; an empty environment selection would disable the task's command and file tools. It adds one narrow recovery: an exact pre-integration `BLOCKED / EXECUTION_TOOL_UNAVAILABLE` accepted from the primary may be replaced only when canonical history and the accepted response hash match, the blocker payload proves no writes, no command or file-change item is present, both frozen worktrees and refs remain unchanged, and the target branch is absent. Version 0.1.15 treats an App Server `proposedExecpolicyAmendment` as ignored metadata when returning one-time `accept`, never applies the proposal, and adds recovery for an exact first-integration `BLOCKED / FORBIDDEN_OPERATION` only when the pending turn is canonically failed or interrupted, contains no side-effect-capable item, both frozen sources remain unchanged and clean, and the target branch is absent. Version 0.1.16 removes exactly one App Server-generated known-shell `-c` or `-lc` wrapper before applying the unchanged inner-command allowlist; nested shells, subcommand approval callbacks, non-local environments, and added permissions remain denied. Version 0.1.17 adds only the exact target-ref `git show-ref --verify refs/heads/<target-integration-branch>` preflight. Version 0.1.19 additionally permits only the equivalent exact `git branch --list <target-integration-branch>` query; every other `git branch` form remains forbidden. Same-run forbidden-operation recovery may retain canonically terminal read-only Git queries only when every item used the frozen primary cwd and still passes that allowlist. Version 0.1.20 marks coordinator-authored Primary and Reviewer prompts as internal participant turns for which the launcher skill is inapplicable. Recovery may discard the exact denied legacy `sed -n 1,240p` read of this plugin's semver-versioned `SKILL.md`; that read remains outside the live command allowlist. Version 0.1.21 recognizes App Server's internal `contextCompaction` lifecycle marker only when it has exactly a nonempty `id` and the fixed `type`; extra fields remain terminal. Version 0.1.22 permits exactly `rg --files -g AGENTS.md` in the frozen primary cwd for repository-instruction discovery, keeps all other `rg` forms denied, and directs later tracked-file reads through the read-only Git policy. Version 0.1.23 adds the request-bound `consensus_apply_patch` capability for one successful text-only patch of at most 512 KiB on the exact clean authorized integration branch after both frozen commits are ancestors. Git preflights the patch without unsafe paths, source refs are revalidated, and SQLite prevents a second successful patch for the same request. The same version may archive and replace an exact completed `FILE_CHANGE_TOOL_UNAVAILABLE` Primary turn after a communication pause only when the approved identity, bwrap permission failure, reported merge SHA, clean target, source ancestry, and frozen refs all match; it reuses that existing merge and never creates a replacement Run. Version 0.1.25 adds only the exact completed `PATCH_NOT_AUTHORIZED` recovery described above. Version 0.1.26 adds only the exact stale `inProgress + waitingOnApproval` variant described above. Every other `inProgress` shape, writes outside that controlled patch, wrong cwd, unknown items, later phases, incomplete or mismatched evidence, and other side effects remain terminal. Other `BLOCKED` states remain terminal. Cancellation never deletes the integration branch or worktree state.
