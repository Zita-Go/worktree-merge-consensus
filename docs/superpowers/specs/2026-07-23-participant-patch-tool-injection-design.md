# Participant Patch Tool Injection Design

## Goal

Guarantee that the effective Primary participant can call the request-bound
controlled patch capability in every coordinator-created Primary turn,
including when the user-selected Primary task was already loaded before the
plugin was installed or selected.

The coordinator must establish and verify the Primary participant route before
the first Primary turn. It must detect a missing or malformed patch capability
before `turn/start`. It must never rely on prompt text, global plugin
selection, or a successful operator-side `doctor` result as proof that a
participant task can call the tool.

## Terms

- **Source Primary**: the exact task selected by the user as Primary. Its
  history, repository binding, worktree, and frozen commit define the Primary
  side of the Run.
- **Reviewer**: the exact task selected by the user as Reviewer.
- **Effective Primary**: the task to which coordinator-created Primary turns
  are sent. It is either the Source Primary or an ephemeral full-history fork
  of the Source Primary.
- **Primary participant binding**: the persisted Run-scoped mapping from the
  Source Primary to the Effective Primary, including its mode and generation.
- **Primary mirror**: an ephemeral Effective Primary created only when an
  already-loaded Source Primary cannot accept task-scoped MCP configuration.
  It is not a third participant and never replaces the Source Primary as a
  source of repository or contract facts.

## Constraints

- Support Codex versions greater than or equal to `0.144.1` with no upper
  version ceiling.
- Preserve the two exact user-selected Source Primary and Reviewer task IDs as
  immutable Run identities.
- Preserve their completed conversation histories. A mirror must fork the
  complete Source Primary history without truncation.
- Preserve both frozen source refs and source worktrees.
- Keep the integration result on a new local-only branch; never push, open a
  pull request, or merge into an existing branch.
- Keep `dangerFullAccess` and `approvalPolicy: "never"` for unattended
  coordinator turns.
- Expose no operator or launcher capability from the participant-only MCP
  server.
- Continue to bind every successful patch to one exact Run, request hash,
  source and effective Primary identities, integration round, clean target
  branch, and frozen source pair.
- Never carry or automatically continue a Source Primary goal into a mirror.
- Fail closed before a model turn when the participant route or patch
  capability cannot be initialized or verified.
- Do not require an operator to close a task, restart Codex, approve a command,
  register global MCP configuration, or manually relay messages.

## Proven Root Cause

The production adapter previously resumed a selected task using only
`{"threadId": ...}`. The subsequent Primary integration prompt nevertheless
required a `consensus_apply_patch` call.

The first proposed fix added a task-scoped MCP override to `thread/resume`.
Real App Server testing then established two distinct behaviors:

1. A task whose App Server state was `notLoaded` accepted the resume override
   and exposed the participant server and `consensus_apply_patch`.
2. An already-loaded Primary accepted the same valid `thread/resume` request
   but its thread-scoped MCP inventory did not change.

The status inventory was requested with an explicit large page and therefore
was not a first-page artifact. Generated Codex `0.144.1` and deployed Codex
schemas confirm that `thread/resume.config`, `thread/fork.config`,
`thread/fork.ephemeral`, `thread/goal/get`, and paginated
`mcpServerStatus/list` are supported.

The root cause is therefore loaded-runtime reuse: resuming an already-loaded
task rejoins its current runtime and does not rebuild that runtime from the
new MCP override. Plugin registration and `doctor` only prove that the
operator-side server is healthy; they do not change a different task's loaded
tool surface.

## Considered Approaches

### Selected: eager task-scoped injection with an ephemeral mirror fallback

The coordinator establishes the Primary participant binding before the first
Primary consensus action:

- If the Source Primary can expose the exact participant server, it remains
  the Effective Primary.
- If it is already loaded, idle, and missing that server, the coordinator
  creates an ephemeral full-history fork with the task-scoped participant MCP
  configuration. The fork becomes the Effective Primary for the Run.

Every subsequent Primary prompt goes through the binding. The Reviewer remains
the exact selected Reviewer task. Coordinator prompts are self-contained from
canonical Run state, so a safely recreated mirror does not depend on hidden
memory from a lost ephemeral runtime.

This preserves the selected task identities and full prior context while
remaining fully automatic. The fallback is private execution plumbing rather
than an additional decision-maker.

### Rejected: require the operator to unload or reopen the Primary

Unloading the task would allow a later resume to apply the override, but Codex
does not provide the coordinator with a reliable, portable unload-and-reload
contract for this workflow. It would also make unattended operation depend on
manual UI state.

### Rejected: register the participant server globally and restart Codex

Global registration would expose a sensitive patch entry point outside the
one Run that needs it. Restarting the App Server could disrupt unrelated tasks
and still requires external lifecycle coordination.

### Rejected: App Server dynamic tools

`dynamicTools` is attached when a thread is created. The product starts from
two already-existing tasks, and the minimum `0.144.1` schema does not provide
a turn-scoped dynamic-tool override for this use case. Dynamic tools also
would require a second patch execution protocol instead of reusing the
existing request-bound MCP backend.

### Rejected: parse a patch from the final assistant response

Moving the patch into the final response would require splitting the existing
atomic integration turn into prepare, daemon-write, and finalize turns. It
would weaken canonical tool-item evidence and duplicate the controlled patch
protocol.

### Rejected: allow the built-in file-change tool

The built-in file-change tool is not bound to the Run and request hash. Under
`dangerFullAccess`, relying only on post-hoc Git inspection would be weaker
than the controlled patch boundary and would not prove that no out-of-scope
write was attempted.

## Architecture

### Participant-only MCP server

Add a hidden CLI mode named `participant-mcp-server`. It uses the existing MCP
stdio transport and daemon backend but advertises and accepts only
`consensus_apply_patch`.

The normal plugin `mcp-server` remains unchanged and continues to advertise
all operator tools plus the participant patch tool for compatibility.

The injected participant server:

- has a stable coordinator-owned server name;
- launches the absolute current `codex-consensus` executable with
  `participant-mcp-server`;
- is marked required;
- enables only `consensus_apply_patch`;
- uses automatic approval for that tool;
- has bounded startup and tool timeouts.

Other task-level or globally configured MCP servers may exist. The
participant-only server itself must expose exactly one tool.

### Binding establishment

The coordinator establishes a binding once repository facts and task roles
are frozen but before the first Primary `turn/start`.

1. Read the Source Primary state and require it to be idle. An active or
   transitioning task is not forked.
2. If the Source Primary is not loaded, resume it with the participant MCP
   override and perform capability preflight.
3. If the Source Primary is loaded, inspect its current thread-scoped MCP
   inventory:
   - if the exact participant capability is present, bind directly;
   - if it is absent, create a mirror instead of retrying an ineffective
     resume override.
4. Call `thread/goal/get` for the Source Primary and require no current goal.
   Supported Codex runtimes may reject goal operations for ephemeral tasks, so
   this check must occur before the fork.
5. Create the mirror with `thread/fork` using:
   - the Source Primary task ID;
   - the complete history, with no last-turn cutoff and no excluded turns;
   - `ephemeral: true`;
   - the exact participant MCP override;
   - no goal-carry or automatic-continuation option.
6. Require the returned mirror ID to be nonempty and different from both
   selected source task IDs.
7. Require the mirror to be idle and pass capability preflight.
8. Atomically persist the binding before starting any Primary turn.

If a not-loaded Source Primary fails to expose the server after a configured
resume, the coordinator treats that as an incompatible or malformed runtime;
it does not hide the failure by creating a fork.

The binding record contains at least:

- immutable Source Primary task ID;
- Effective Primary task ID;
- mode: `DIRECT` or `EPHEMERAL_FORK`;
- mirror generation;
- participant server identity;
- creation and verification timestamps.

Repository discovery, source commit selection, source-worktree validation, and
all contract ownership remain attached to the Source Primary. Turn provenance
and controlled patch evidence are attached to the Effective Primary and
cross-checked against the same binding.

### Primary and Reviewer routing

All Primary actions, not only the final integration action, route through the
Effective Primary. This ensures that one task accumulates the complete
coordinator-side negotiation and has the controlled patch capability before
it becomes necessary.

Reviewer actions always route to the selected Reviewer. The Reviewer never
receives the participant patch server.

Every Primary prompt identifies both immutable source identity and effective
execution identity. The prompt makes clear that a mirror represents the Source
Primary, must preserve its implementation contract, and must operate only on
the coordinator-created integration worktree.

### Capability preflight

Call `mcpServerStatus/list` with the Effective Primary task ID and consume all
pages before accepting the inventory. Pagination must:

- use a bounded page size;
- follow opaque `nextCursor` values;
- reject cursor cycles and duplicate participant server entries;
- stop at a deterministic maximum page and item count;
- fail closed if the complete inventory cannot be established.

Before every Primary `turn/start`, require:

- exactly one matching participant server in the complete inventory;
- a successfully initialized status;
- exactly one exposed tool on that server;
- the tool name `consensus_apply_patch`;
- a present and valid input schema.

Missing, duplicate, additional, or malformed participant capability state
blocks before a model turn. Other unrelated servers do not invalidate the
preflight.

Diagnostics record the App Server operation, source task ID, effective task
ID, binding mode, and mirror generation without storing prompts, patches, or
credentials.

### Canonical patch evidence

The participant turn continues to produce a canonical `mcpToolCall` item. A
new controlled patch request stores both Source Primary and Effective Primary
identities from the persisted binding.

Integration history validation requires:

- the turn belongs to the Effective Primary in the active binding;
- the request belongs to the immutable Source Primary and Run;
- the participant server identity is exact;
- the existing request hash, patch hash, single-success-per-request, target
  branch, source ancestry, frozen-ref, and clean-worktree checks all pass.

Legacy plugin server evidence is accepted only for recovery cases already
defined by prior releases. New requests require the participant server
identity.

### Ephemeral mirror recovery

The coordinator reuses one live mirror for the Run whenever possible.
Ephemeral tasks may disappear when the App Server restarts, so each Primary
prompt must contain all authoritative contracts, the current proposal or
feedback, the accepted canonical evidence, and the exact requested action.

If the mirror is unavailable:

- before any `turn/start` was persisted, it may be recreated safely;
- after a prior turn completed and its canonical response was persisted, a
  new generation may be created before the next action;
- while a non-idempotent turn is pending or its outcome is uncertain, the
  coordinator must reconcile that exact turn and must not create a replacement
  mirror or resend;
- if the missing runtime makes reconciliation impossible, the Run blocks with
  a communication failure.

Every recreation repeats the full-history fork, no-goal check, capability
preflight, and atomic binding update. A generation change never changes the
Source Primary, source commits, branch, round, request hashes, or Reviewer.

At a terminal Run state, the coordinator releases its App Server subscription
or local handle. It does not delete, archive, or mutate either selected source
task. The ephemeral mirror is not installed as a persistent user task.

## Existing Run Recovery

Version `0.2.7` may recover the exact production shape created after the
`0.2.6` verification recovery:

- the Run is `BLOCKED` with
  `CONTROLLED_PATCH_TOOL_UNAVAILABLE`;
- the blocked action is the post-verification Primary integration correction
  round;
- a previous integration branch and integration SHA exist but are not
  accepted;
- authoritative verification evidence exists and contains at least one
  failed frozen command;
- the blocked turn is completed and contains the deterministic request marker,
  the exact blocker response, and no command, file-change, MCP, dynamic-tool,
  or other side-effect-capable item;
- no successful patch is recorded for the blocked correction request;
- the target branch remains clean at the recorded integration SHA, contains
  both frozen source commits, and both source refs and source worktrees remain
  unchanged;
- no repository lock is held by another active Run.

Explicit same-Run resume archives only that empty blocked attempt, reacquires
the repository lock atomically, and establishes a Primary participant binding.
If the loaded Source Primary still lacks the participant server, recovery uses
the mirror fallback. It retries the same correction round and never recreates
the branch or repeats the merge.

The correction round may apply one successful patch for its own request and
create one new integration commit. The integration SHA must therefore advance.
The coordinator then runs the complete frozen verification command set again.

Any mismatch, extra tool item, possible side effect, source drift, target
movement, successful patch residue for the blocked request, missing failed
verification evidence, or uncertain pending turn remains terminal.

## Error Handling

- Source Primary is active or transitioning:
  retain the existing task-busy behavior; do not fork.
- Configured resume of a not-loaded Source Primary does not expose the exact
  server: `PATCH_TOOL_UNAVAILABLE`, before `turn/start`.
- Loaded Source Primary lacks the exact server: attempt one mirror binding for
  that generation.
- Source Primary has an active goal: `HISTORY_UNAVAILABLE` before
  `thread/fork`.
- Mirror creation, identity, history, or idle-state validation fails:
  the corresponding identity, history, or communication error before
  `turn/start`.
- MCP inventory is incomplete, cyclic, duplicated, malformed, or lacks the
  exact participant tool: `PATCH_TOOL_UNAVAILABLE`, before `turn/start`.
- App Server lacks `thread/fork`, `thread/goal/get`, or
  `mcpServerStatus/list`: `INCOMPATIBLE_CODEX`.
- Participant server advertises additional tools:
  `PATCH_TOOL_UNAVAILABLE`.
- Tool call identity, arguments, binding generation, or canonical status
  mismatch: retain the existing fail-closed integration-history error.
- Communication loss after a non-idempotent `turn/start`: retain existing
  persisted pending-send recovery; do not resend or refork blindly.

No error path modifies a frozen source ref or source worktree.

## Compatibility

The compatibility fixture and required-method list add:

- `thread/fork` with task-scoped `config` and `ephemeral`;
- `thread/goal/get`;
- paginated `mcpServerStatus/list` with a thread ID.

The checked minimum and deployed Codex versions must also expose:

- `thread/resume.config`;
- required MCP startup semantics;
- canonical `mcpToolCall` items.

The implementation does not require `deferGoalContinuation`, which is absent
from Codex `0.144.1`. There remains no maximum Codex version gate.

## Testing

### App Server client

- A not-loaded Source Primary resume contains the exact task-scoped MCP
  configuration and retains the same task ID.
- A loaded Source Primary with the exact capability binds directly.
- A loaded, idle Source Primary without it creates one ephemeral full-history
  fork with the exact configuration.
- An active Source Primary is never forked.
- A mirror with an empty, colliding, or Reviewer task ID is rejected.
- A Source Primary with a current goal is rejected before `thread/fork`.
- No goal operation is attempted on an ephemeral mirror.
- No request uses `deferGoalContinuation`.
- Multi-page inventory succeeds only after all pages are validated.
- Cursor cycles, duplicate participant entries, malformed pages, and bounds
  exhaustion fail before `turn/start`.

### MCP server and CLI

- The normal MCP mode still lists all public tools.
- The participant MCP mode lists only `consensus_apply_patch`.
- The participant mode rejects every operator tool.
- Help keeps both internal modes hidden.

### Coordinator

- Binding establishment occurs before the first Primary action.
- Every Primary action uses the Effective Primary; every Reviewer action uses
  the selected Reviewer.
- Every Primary turn performs capability preflight before `turn/start`.
- A failed binding or preflight creates no participant turn and performs no
  Git write.
- Patch requests and canonical evidence contain consistent source/effective
  identities and binding generation.
- Existing legacy patch evidence remains readable.
- New patch evidence requires the participant server identity.
- A disappeared mirror is recreated only at a persisted safe boundary.
- A pending or uncertain turn is never resent through a new mirror.
- The exact post-verification tool-unavailable Run can resume once.
- Variants with side effects, drift, changed SHA, missing failed tests, a
  prior correction patch, or a second unsafe recovery attempt are rejected.

### End to end

- Two pre-existing task fixtures complete a conflict-free integration when the
  Source Primary is initially not loaded.
- The same workflow completes when the Source Primary is preloaded without the
  plugin; the Effective Primary is an ephemeral full-history mirror.
- The mirror receives the complete pre-existing Primary context and all
  subsequent Primary negotiation turns.
- Verification failure enters a corrective integration round, applies one new
  patch, advances the integration SHA, reruns every frozen command, and reaches
  Reviewer result approval.
- Source refs and source worktrees remain unchanged, source task IDs remain
  authoritative, and no remote publication occurs.

## Release and Deployment

Prepare release `0.2.7` only after:

- workspace tests pass;
- formatting and Clippy pass;
- documentation and release gates pass;
- minimum supported Rust and Codex compatibility checks pass;
- a real App Server smoke test confirms direct injection for a not-loaded
  existing task;
- a real App Server smoke test confirms automatic ephemeral fallback for a
  preloaded existing task, with no carried goal and no turn started during
  preflight;
- a full disposable-repository workflow completes through Reviewer approval
  while both source branches and worktrees remain unchanged.

Deployment replaces the binary and plugin on each host, restarts the consensus
daemon, confirms `doctor`, and then explicitly resumes the exact blocked Run
once. Installation itself does not mutate production Run state.
