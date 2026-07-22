# Unattended Danger-Full-Access Verification Design

## Goal

Make every coordinator-owned participant turn and every frozen verification
command run without human approval or Bubblewrap, while preserving the exact
two source refs, source worktrees, local-only integration branch, and
machine-derived verification evidence.

## Confirmed failure

Release 0.2.4 sends `approvalPolicy: "never"`, but its `readOnly` and
`workspaceWrite` policies still ask Codex to create a Linux sandbox. The target
container cannot perform Bubblewrap's mount propagation setup and returns:

```text
bwrap: Failed to make / slave: Permission denied
```

The six frozen commands therefore never reach Cargo. Independently, the
existing task exposes those Shell calls as raw `custom_tool_call` response
items, while the coordinator persists and accepts only canonical
`commandExecution` items. The completed task consequently appears to contain
no commands even though its rollout contains six failed attempts.

## Selected approach

Use two separate mechanisms:

1. Start every coordinator-owned Primary and Reviewer turn with
   `approvalPolicy: "never"` and `sandboxPolicy: {"type":"dangerFullAccess"}`.
   This applies per automated turn and does not rewrite either task's global
   configuration.
2. Keep the Primary verification response as a marker-only acknowledgement,
   but execute the frozen commands in the coordinator through App Server
   `command/exec`. Do not derive acceptance evidence from model-generated
   Shell calls.

`command/exec` accepts an argv vector, cwd, timeout, output cap, and an explicit
sandbox policy and returns structured `exitCode`, `stdout`, and `stderr`.
OpenAI Codex `rust-v0.144.1` already contains this API; the generated schemas
on Basestream 0.145.0 and Huoshan 0.144.6 expose the same required fields.

## Rejected approaches

### Parse `rawResponseItem/completed`

Raw `custom_tool_call.input` contains model-generated JavaScript rather than a
stable command schema. Parsing it would either accept an unsafe subset by
mistake or reject harmless syntactic variation. It is unsuitable as an
authorization or acceptance boundary.

### Add another public MCP test tool

A dedicated participant-callable test tool could be made deterministic, but
it would add another approval/configuration surface, another long-running RPC,
and another model action that can be omitted or malformed. The coordinator
already owns the frozen command list and can execute it without model routing.

### Keep only `workspaceWrite`

This retains stronger OS isolation but cannot operate in the deployed
container. Retrying it does not address the mount-namespace failure.

## Turn policy

All coordinator-created turn classes use the same unattended policy:

```json
{
  "approvalPolicy": "never",
  "sandboxPolicy": {"type": "dangerFullAccess"}
}
```

The coordinator still pins `cwd`, `runtimeWorkspaceRoots`, and the local
environment as task orientation metadata. Those fields are not claimed as an
OS security boundary under `dangerFullAccess`.

The controlled `consensus_apply_patch` tool remains independently configured
with `approval_mode = "approve"`. The protocol authorizes only that
request-bound tool to apply the text patch recorded for an integration request;
`dangerFullAccess` is explicitly not an OS-level enforcement of that rule.

## Verification flow

1. Revalidate both frozen source refs and the current integration SHA.
2. Materialize or recover the detached, remote-free verification clone for
   that exact SHA.
3. Send the Primary a marker-only verification turn. Its prompt says not to
   execute Shell commands and to return exactly the existing
   `VERIFICATION_READY` marker plus optional Markdown.
4. Parse each previously validated frozen command with `shell_words` into one
   argv vector. Reject an empty vector or any command that no longer passes the
   frozen command policy.
5. Execute every command exactly once in declared order through
   `command/exec`, with:
   - cwd equal to the canonical verification clone;
   - `sandboxPolicy.type = dangerFullAccess`;
   - no PTY or stdin streaming;
   - a bounded per-command timeout;
   - bounded stdout and stderr capture.
6. Continue after nonzero exits so the evidence set always covers every frozen
   command.
7. Persist the structured result before advancing state. Derive test evidence
   and bounded failure diagnostics from the persisted command, cwd, exit code,
   stdout, and stderr. Participant prose is never evidence.
8. If all commands pass, request Reviewer result review. If any command fails,
   return the same Run to a controlled Primary integration correction round.

## Persistence and restart behavior

Store coordinator command executions under the deterministic verification
request identity and command index. A completed exact record is reused after a
daemon restart and must not be executed again.

Record `STARTED` before dispatch and `COMPLETED` only after receiving the
structured App Server response. If a restart finds `STARTED` without
`COMPLETED`, fail closed with an explicit uncertain-execution diagnostic. Do
not silently claim success or rerun an execution whose completion is unknown.
This preserves the existing no-duplicate side-effect invariant.

The evidence identity contains the Primary verification turn ID and a
deterministic coordinator command item ID. The latter is explicitly identified
as coordinator-owned rather than an App Server `ThreadItem`.

## Existing Run migration

Permit one release-bounded migration for a blocked 0.2.4 verification request
only when all of these are true:

- status is `BLOCKED / TEST_FAILURE`;
- the diagnostic is the exact missing-command-evidence diagnostic for
  `REQUEST_PRIMARY_VERIFICATION`;
- the same Run, pending request, Primary task, verification clone,
  integration branch, and integration SHA match;
- no authoritative test evidence has been accepted;
- the integration branch is clean and contains both frozen commits;
- both frozen source refs are unchanged, the Reviewer source worktree remains
  clean at its frozen commit, and the Primary integration worktree remains
  clean at the exact integration SHA;
- the exact legacy turn is completed and contains no canonical
  side-effect-capable item;
- this migration has not already been recorded.

Archive only that legacy verification turn, clear only its pending delivery,
and restore `REQUEST_PRIMARY_VERIFICATION`. Never recreate the Run, branch,
merge, controlled patch, or integration commit.

## Security boundary

`dangerFullAccess` executes with the App Server process user's container and
mounted-filesystem permissions. This release is therefore for trusted tasks,
trusted repositories, and trusted frozen test commands. It does not claim that
`runtimeWorkspaceRoots` prevents access outside the declared paths.

Acceptance still fails unless:

- the source refs retain their frozen SHAs;
- the Reviewer source worktree remains clean at its frozen commit and the
  Primary integration worktree is clean at the exact integration SHA;
- the integration result remains local-only, clean, and contains both source
  commits;
- every frozen test has one exact completed coordinator result;
- Reviewer approval names the exact verified integration SHA.

No branch is pushed, no pull request is created, and no existing branch is
updated.

## Tests

Add regression coverage proving:

- every turn class emits `approvalPolicy: never` plus
  `sandboxPolicy.type: dangerFullAccess`;
- `command/exec` emits an argv vector, exact cwd, timeout, output cap, and
  `dangerFullAccess`, and parses its structured response;
- marker-only verification triggers every frozen command in order and derives
  passing evidence without command items in task history;
- all commands still run after an earlier nonzero exit, and failures route to
  corrective integration;
- completed journal records are reused after restart;
- uncertain `STARTED` records fail closed;
- the 0.2.4 migration is exact, one-time, and preserves the Run, branch,
  integration SHA, controlled patch record, and both frozen refs;
- malformed commands, wrong cwd, mismatched journal identity, source drift,
  and integration drift fail closed;
- the plugin skill and smoke-test instructions describe the unattended trusted
  execution boundary accurately.

## Acceptance criteria

1. Basestream no longer invokes bwrap for coordinator-owned turns or frozen
   verification commands.
2. The original Run `f83cd777-9ed1-4369-8270-0fedd282f912` is resumed exactly
   once after deployment and is never replaced.
3. No successful branch creation, merge, patch, or integration commit is
   repeated.
4. All six frozen commands produce structured evidence from the exact
   verification clone.
5. Reviewer approval and final acceptance refer to the exact verified SHA.
6. `master` and `codex/feature-expansion` remain at their frozen SHAs and no
   remote state changes.
