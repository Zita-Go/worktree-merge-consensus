# Recovery and Troubleshooting

[简体中文](recovery.zh-CN.md)

Recovery is conservative, explicit, and always tied to the same durable Run.
Installing a new binary/plugin version alone does not mutate or recover a
blocked Run. Resolve the reported condition, inspect the retained Git state,
and call `codex-consensus resume RUN_ID` only when the reason below is marked as
recoverable.

## Operator commands

```bash
codex-consensus doctor
codex-consensus status RUN_ID --json
codex-consensus watch RUN_ID
codex-consensus resume RUN_ID
codex-consensus cancel RUN_ID
```

`cancel` preserves existing Git state, including an integration branch that was
already created. It does not make the Run resumable.

## Recovery invariants

Before a permitted retry, the coordinator revalidates the exact Run, pending
request, participant binding, round, source task IDs, worktree paths, source
refs and SHAs, authorized target branch, and any existing integration SHA. It
also audits canonical App Server history for completed commands, file changes,
MCP calls, unknown items, and uncertain side effects.

Recovery never silently:

- creates a replacement Run;
- selects different tasks or worktrees;
- moves or repairs a source ref;
- repeats an uncertain patch, merge, staging operation, commit, or test command;
- accepts a result whose exact SHA was not tested and reviewed.

## Common reasons

| Reason | Action |
| --- | --- |
| `COMMUNICATION_FAILURE` | Repair connectivity and explicitly resume. A terminal failed/interrupted turn is replaced only when canonical history proves no side-effect-capable item exists. |
| `INVALID_TEST_COMMAND` | Correct the declared direct test command, then explicitly resume if the Run is still pre-integration and history is read-only. |
| `INVALID_RESPONSE` | A pre-integration contract or plan response may be retried only when the completed turn is read-only and identity checks match. |
| `EXECUTION_TOOL_UNAVAILABLE` | After restoring the task's tools, a first integration request may be retried only when no target branch or write exists. |
| `CONTROLLED_PATCH_TOOL_UNAVAILABLE` | Use the exact version-gated path below; near-matches remain terminal. |
| `FORBIDDEN_OPERATION` | Some historical exact read-only command misclassifications have narrow same-Run migrations. Inspect the version-specific section below. |
| `INCOMPATIBLE_STATE` | Usually terminal. Version 0.3.13 recognizes only the exact stale-Reviewer-reasoning lifecycle case described below. |
| `APPROVAL_CONFIGURATION_REQUIRED` | Plugin setup normally writes the one scoped key automatically. If managed policy blocks it, run `codex-consensus configure` as the same Codex account and `CODEX_HOME`, then retry the original operation. |
| `WAITING_THREAD` | Finish or interrupt the unrelated active task turn, then resume. |
| `SOURCE_DRIFT` | Inspect the source changes and start a new Run from newly confirmed commits. Do not resume with different source identity. |
| `INTEGRATION_BRANCH_EXISTS` | Choose a different new branch and start a new Run; the coordinator never reuses or deletes it. |
| `NO_PROGRESS`, `ROUND_LIMIT` | Terminal. Revise the work or contracts and start a new Run. |
| `VERIFICATION_EXECUTION_UNCERTAIN` | Terminal by design. The coordinator cannot prove whether a frozen command completed and will not rerun it automatically. |
| `CANCELLED` | Terminal. Existing Git state is retained. |

## Controlled-patch correction boundary

After a matching 0.2.8-or-newer deployment, explicit `consensus_resume` may
recover only the exact `CONTROLLED_PATCH_TOOL_UNAVAILABLE` correction blocker.
The same Run, round, branch, old integration SHA, and failed frozen verification
evidence must still match. Recovery archives only the empty, side-effect-free
correction attempt, atomically reacquires the repository lock, and permits at
most one request-bound corrective patch and one correction commit. The new SHA
must advance, and every frozen verification command reruns against that new
SHA. Installing or enabling the matching version alone never mutates or
recovers the Run.

Before every retried Primary turn, the coordinator repeats participant binding
preflight. The participant inventory must contain exactly
`consensus_apply_patch`; the selected Source Primary, Effective Primary lineage,
Reviewer, source refs, and worktrees remain frozen. Any sent or uncertain turn
is never reforked or resent.

## Completed integration recovery

When a request-bound patch and integration commit already succeeded but a later
read-only confirmation was rejected, recovery may preserve that exact result
instead of repeating the write. The coordinator requires:

- a successful stored patch record matching the request hash;
- a clean authorized target at the authoritative integration SHA;
- both frozen commits as ancestors;
- unchanged source refs;
- canonical terminal history containing only allowed read-only inspection after
  the successful write.

It archives only the rejected confirmation and requests one read-only
`INTEGRATION_READY` response. It never repeats the patch, branch creation,
merge, staging, or commit.

The production layout may store the successful patch on an archived ephemeral
Primary attempt while the current attempt contains only result confirmation.
Those histories are validated separately and must share the exact frozen Source
lineage and request identity.

### Historical commentary/final-answer audit migration

Version 0.3.12 can resume the same Run after the exact 0.3.11
`FORBIDDEN_OPERATION` detail
`controlled patch call appears after the final agent response`. Eligibility is
not inferred from that text alone. Canonical App Server history must prove an
`agentMessage` with `phase: commentary` came before exactly one successful,
request-bound controlled patch and that `phase: final_answer` came afterward.
The successful patch record, clean authoritative target, source ancestry,
frozen refs, request identity, and participant binding must all revalidate.

Recovery archives only that completed response attempt and requests one
read-only `INTEGRATION_READY` confirmation. Missing, malformed, or unknown
message phases remain terminal for audit purposes. A patch or command that
actually occurs after `final_answer`, a second patch, partial history, or any
repository drift remains non-recoverable.

### Historical Reviewer reasoning-lifecycle migration

Version 0.3.13 can resume the same Run after the exact 0.3.12 final-verdict
diagnostic `turn <id> completed before all item lifecycle events were
persisted` only when the pending action is the bound Reviewer's
`REQUEST_REVIEWER_RESULT_VERDICT`. A durable successful `turn/completed` event
must exist, and every incomplete item must be exactly `type: reasoning` with
state `STARTED`. An incomplete command, MCP call, file change, unknown item,
other lifecycle state, or unsuccessful turn remains terminal.

The coordinator revalidates the unchanged frozen refs, clean tested
integration SHA, complete successful frozen-test evidence, request marker,
Reviewer identity, canonical final response, and the completed read-only Git
trace in the Reviewer's frozen worktree. It then consumes the already
completed response; it does not send another Reviewer turn and does not repeat
verification, patching, branch creation, merge, staging, or commit. Both the
marker response and the legacy protocol JSON response are validated before the
state is advanced.

Version 0.3.14 additionally handles the same completed turn when App Server
history and durable item events contain identical request-bound user messages
under different item IDs. Only the ID is ignored; canonical content and the
request hash must match exactly, otherwise recovery remains fail-closed.

## Historical read-only command migrations

Version 0.2.14 added the exact read-only query
`git symbolic-ref --short HEAD`. Version 0.3.1 added
`git branch --show-current`. Both may be direct or wrapped once by the canonical
App Server `/bin/bash -lc` form.

An older `FORBIDDEN_OPERATION` caused only by one of those exact queries may be
resumed on the same Run after matching artifacts are installed. Recovery
requires terminal canonical history, unchanged frozen refs, a clean
authoritative target, and successful patch provenance when integration already
occurred. Argument-bearing or write-capable variants, unknown commands, file
changes, MCP calls, uncertain results, and commands after the final response
remain terminal.

## Daemon and App Server restarts

The daemon persists intended sends before App Server dispatch. On restart it
recovers only idempotent reads and actions whose completion is proven. A lost
non-idempotent `turn/start` response is not blindly resent. Completed
coordinator-owned verification commands are reused from their exact journal;
STARTED commands without proven completion fail closed.

Version 0.3.7 recognizes one additional exact boundary: an ephemeral Primary
is idle after its request-bound patch and clean commit succeeded, while the
coordinator persisted zero events for that turn. It revalidates patch
provenance, target ancestry, cleanliness, and frozen refs, refreshes the App
Server connection, and retries only the read-only result marker once. It does
not infer success from a partial event trace and never repeats the patch. If
the first connection refresh itself fails and pauses the Run, explicit
same-Run resume repeats these checks and may perform that one confirmation-only
recovery after connectivity is restored.

Version 0.3.8 extends only that same boundary when App Server returns the exact
`-32600 / thread not loaded: <requested ephemeral id>` identity after a hot
reload or lifecycle transition. Explicit same-Run resume performs the 0.3.7
checks, archives the eventless attempt, and rotates the ephemeral binding only
after the deterministic pending request is provably unsent and the frozen
Source-history fingerprint is unchanged. The replacement receives only the
final confirmation request; a different identity, uncertain dispatch, partial
events, or changed Source history remains fail-closed.

If the managed App Server restarts while the daemon is alive, `doctor` probes
both a fresh direct connection and the daemon-owned proxy. It repairs a closed
proxy before an idempotent task read. If repair fails, preserve the Run and fix
the App Server connection before explicitly resuming.

## Installation troubleshooting

### Plugin tools are absent

```bash
codex plugin list
codex mcp list --json
```

Version 0.3.9 resolves the exact static runtime before the MCP handshake. On a
fresh install, allow the first task up to five minutes to download and verify
the matching release. Confirm `worktreeMergeConsensus` is enabled and inspect
its startup diagnostic for download, checksum, architecture, or managed-policy
errors, then open a new launcher task. MCP names such as `consensus_doctor` are
not shell executables.

The verified runtime is versioned under the plugin's private `PLUGIN_DATA`; it
is intentionally not required to appear in `PATH`. If a direct
`codex-consensus` executable is present, these optional checks still apply:

```bash
command -v codex-consensus
codex-consensus --version
codex-consensus doctor
```

For offline or centrally managed installations, set `CODEX_CONSENSUS_BIN` to
an executable whose output is exactly the plugin's
`codex-consensus <version>`. A mismatch is rejected rather than silently
mixing binary/plugin versions.

### `LEGACY_SKILL_CONFLICT`

An older manually installed `$CODEX_HOME/skills/worktree-merge-consensus`
shadows the plugin workflow. Back it up or remove it manually, reinstall the
plugin from its matching marketplace release, and open a new task. Diagnostics
never delete the legacy directory.

### `INCOMPATIBLE_CODEX`

Confirm `codex --version` is `>=0.144.1`, then read
[Compatibility](compatibility.md). The adapter also checks App Server identity,
required methods, and response shapes; passing the numeric version floor alone
is not sufficient.

### Repository preflight failures

- `DIRTY_WORKTREE`: commit or intentionally resolve local changes first.
- `UNREGISTERED_WORKTREE` / `DUPLICATE_WORKTREE` /
  `REPOSITORY_MISMATCH`: choose two different entries from the same
  `codex-consensus worktrees list` result.
- `WORKTREE_UNAVAILABLE`: restore the frozen path or start a new Run.
- `SOURCE_BINDING_MISMATCH`: correct the task-to-worktree mapping and start a new
  Run; resume cannot replace frozen identity.

For version-by-version migrations and exact canonical-history shapes, read
[Compatibility](compatibility.md) and [CHANGELOG](../CHANGELOG.md).
