# Participant Patch Tool Injection Design

## Goal

Guarantee that every coordinator-created Primary integration turn can call the
request-bound controlled patch capability, including turns sent to existing
Codex tasks that were created before the plugin was installed or selected.

The coordinator must detect a missing patch capability before `turn/start`.
It must never rely on prompt text, global plugin selection, or a successful
operator-side `doctor` result as proof that a participant task can call the
tool.

## Constraints

- Support Codex versions greater than or equal to `0.144.1` with no upper
  version ceiling.
- Reuse the two exact existing Primary and Reviewer task IDs and preserve their
  completed conversation histories.
- Preserve both frozen source refs and source worktrees.
- Keep the integration result on a new local-only branch; never push, open a
  pull request, or merge into an existing branch.
- Keep `dangerFullAccess` and `approvalPolicy: "never"` for unattended
  coordinator turns.
- Expose no operator or launcher capability to a participant task.
- Continue to bind every successful patch to one exact Run, request hash,
  Primary task, integration round, clean target branch, and frozen source pair.
- Fail closed before a model turn when the participant patch capability cannot
  be initialized or verified.

## Root Cause

The production adapter currently resumes a selected task using only
`{"threadId": ...}`. The subsequent Primary integration prompt nevertheless
requires a `consensus_apply_patch` call.

Installing and enabling the plugin proves that the operator task can discover
the plugin MCP server. It does not prove that an unrelated existing Primary
task has selected the same plugin capability root. The coordinator therefore
created a valid integration prompt containing an unavailable tool name.

## Considered Approaches

### Selected: task-scoped required MCP configuration

On every Primary integration `thread/resume`, inject a direct task-scoped MCP
configuration whose executable is the exact running `codex-consensus` binary.
The server uses a participant-only mode that advertises only
`consensus_apply_patch`.

Mark the server as required, restrict its enabled tools to the one patch
capability, and configure that tool for automatic approval. After resume,
query the thread-scoped MCP inventory and require the exact server and exact
tool before sending `turn/start`.

This approach preserves the selected task ID and its existing history, uses
the App Server configuration override already supported by the minimum Codex
version, and keeps the current request-bound patch protocol.

### Rejected: App Server dynamic tools

`dynamicTools` is attached when a thread is created. The product accepts two
already-existing tasks, so replacing either task with a newly created thread
would violate the identity and full-history requirements. The minimum
supported `0.144.1` schema does not provide a turn-scoped dynamic-tool override
for this use case.

### Rejected: parse a patch from the final assistant response

Moving the patch into the final response would require splitting the existing
atomic integration turn into prepare, daemon-write, and finalize turns. It
would add a second protocol for the same patch while weakening canonical tool
item evidence. The existing controlled patch backend already provides the
required request binding, Git preflight, single-use record, and audit trail.

### Rejected: allow the built-in file-change tool

The built-in file-change tool is not bound to the Run and request hash. Under
`dangerFullAccess`, relying only on post-hoc Git inspection would be weaker
than the current controlled patch boundary and would not prove that no
out-of-scope write was attempted.

## Architecture

### Participant-only MCP server

Add a hidden CLI mode named `participant-mcp-server`. It uses the existing MCP
stdio transport and daemon backend but advertises and accepts only
`consensus_apply_patch`.

The normal plugin `mcp-server` remains unchanged and continues to advertise
all operator tools plus the participant patch tool for compatibility.

### Resume policy

Extend the App Server abstraction so `thread/resume` receives a typed resume
policy:

- ordinary Primary and Reviewer actions resume without participant MCP
  injection;
- `REQUEST_PRIMARY_INTEGRATION` resumes with the participant-only MCP
  configuration.

The injected server:

- has a stable coordinator-owned server name;
- launches the absolute current executable with `participant-mcp-server`;
- is marked `required = true`;
- advertises only `consensus_apply_patch`;
- uses `approval_mode = "approve"` for that tool;
- has bounded startup and tool timeouts.

The coordinator passes the same injection again before every integration turn
so behavior does not depend on previous task session state.

### Capability preflight

After the integration task resumes, call
`mcpServerStatus/list` with that exact `threadId`. Before `turn/start`, require:

- one matching participant server;
- a successfully initialized inventory response;
- exactly one exposed tool on that server;
- the tool name is `consensus_apply_patch`;
- its input schema is present.

Missing, additional, or malformed capability state blocks with
`PATCH_TOOL_UNAVAILABLE` before creating a model turn. The diagnostic records
the App Server operation and task ID without storing prompts, patches, or
credentials.

### Canonical evidence

The participant turn continues to produce an `mcpToolCall` item. Integration
history validation accepts the participant server identity and the existing
legacy plugin server identity only where required for recovery of older Runs.
New Runs require the participant server identity.

All existing request hash, patch hash, single-success-per-request, target
branch, source ancestry, and frozen-ref checks remain mandatory.

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
the repository lock atomically, and retries the same correction round after
participant MCP preflight. It never recreates the branch or repeats the merge.

The correction round may apply one successful patch for its own request and
create one new integration commit. The integration SHA must therefore advance.
The coordinator then runs the complete frozen verification command set again.

Any mismatch, extra tool item, possible side effect, source drift, target
movement, successful patch residue for the blocked request, or missing failed
verification evidence remains terminal.

## Error Handling

- Required MCP initialization failure:
  `PATCH_TOOL_UNAVAILABLE`, before `turn/start`.
- MCP inventory does not contain the exact participant tool:
  `PATCH_TOOL_UNAVAILABLE`, before `turn/start`.
- App Server lacks `mcpServerStatus/list`:
  `INCOMPATIBLE_CODEX`.
- Participant server advertises additional tools:
  `PATCH_TOOL_UNAVAILABLE`.
- Tool call identity, arguments, or canonical status mismatch:
  retain the existing fail-closed integration-history error.
- Communication loss after a non-idempotent `turn/start`:
  retain existing persisted pending-send recovery; do not resend blindly.

## Compatibility

The compatibility fixture and required-method list add
`mcpServerStatus/list`. The checked minimum and deployed Codex versions must
both expose:

- `thread/resume.config`;
- required MCP startup semantics;
- `mcpServerStatus/list` with a thread ID;
- canonical `mcpToolCall` items.

There remains no maximum Codex version gate.

## Testing

### App Server client

- A Primary integration resume request contains the exact task-scoped MCP
  configuration.
- Ordinary resume requests do not inject the participant server.
- MCP status parsing accepts only the exact participant server and tool.
- Missing, malformed, or additional tool inventory fails before `turn/start`.
- Reconnecting resume preserves the same typed policy.

### MCP server and CLI

- The normal MCP mode still lists all public tools.
- The participant MCP mode lists only `consensus_apply_patch`.
- The participant mode rejects every operator tool.
- Help keeps both internal modes hidden.

### Coordinator

- Every Primary integration action performs resume, MCP preflight, then
  `turn/start` in that order.
- A failed preflight creates no participant turn and performs no Git write.
- Existing legacy patch evidence remains readable.
- New patch evidence requires the participant server identity.
- The exact post-verification tool-unavailable Run can resume once.
- Variants with side effects, drift, changed SHA, missing failed tests, a
  prior correction patch, or a second recovery attempt are rejected.

### End to end

- Two pre-existing task fixtures with no selected plugin capability complete a
  conflict-free integration using the injected participant server.
- Verification failure enters a corrective integration round, applies one new
  patch, advances the integration SHA, reruns every frozen command, and reaches
  reviewer result approval.
- Source refs remain unchanged and no remote publication occurs.

## Release and Deployment

Prepare release `0.2.7` only after:

- workspace tests pass;
- formatting and Clippy pass;
- documentation and release gates pass;
- minimum supported Rust and Codex compatibility checks pass;
- a real App Server smoke test confirms the participant MCP appears on an
  existing resumed task before any integration turn is sent.

Deployment replaces the binary and plugin on each host, restarts the consensus
daemon, confirms `doctor`, and then explicitly resumes the exact blocked Run
once. No automatic production-state mutation occurs during installation.
