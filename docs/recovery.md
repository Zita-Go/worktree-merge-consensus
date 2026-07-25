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
| `APPROVAL_CONFIGURATION_REQUIRED` | Run `codex-consensus configure` as the same Codex account and `CODEX_HOME`, then retry the original operation. |
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

If the managed App Server restarts while the daemon is alive, `doctor` probes
both a fresh direct connection and the daemon-owned proxy. It repairs a closed
proxy before an idempotent task read. If repair fails, preserve the Run and fix
the App Server connection before explicitly resuming.

## Installation troubleshooting

### Plugin tools are absent

```bash
command -v codex-consensus
codex-consensus --version
codex-consensus doctor
codex mcp list --json
```

`doctor` proving that the binary and daemon are healthy does not prove that an
already-open Codex task loaded the plugin. Confirm `worktreeMergeConsensus` is
enabled and points to the matching plugin version, then open a new launcher
task. MCP names such as `consensus_doctor` are not shell executables.

### `LEGACY_SKILL_CONFLICT`

An older manually installed `$CODEX_HOME/skills/worktree-merge-consensus`
shadows the plugin workflow. Back it up or remove it manually, reinstall
matching binary/plugin artifacts, and open a new task. Diagnostics never delete
the legacy directory.

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
