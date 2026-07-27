# Same-Run Recovery Reference

Use this reference only after `consensus_status` reports a paused or blocked Run
and the user wants to continue it.

## Contents

- [Global invariants](#global-invariants)
- [Communication and task availability](#communication-and-task-availability)
- [Invalid declarations or responses](#invalid-declarations-or-responses)
- [Execution and command policy](#execution-and-command-policy)
- [Controlled patch failures](#controlled-patch-failures)
- [Verification failures](#verification-failures)
- [Legacy migration qualifiers](#legacy-migration-qualifiers)

## Global invariants

- Recover only the existing Run ID; never create a replacement Run.
- Require explicit user authorization after the reported external condition is
  resolved.
- Let `consensus_resume` and the daemon decide whether the persisted exact state
  is eligible.
- Never repeat an uncertain command, fork, patch, merge, stage, or commit.
- Revalidate frozen refs, worktree cleanliness, target identity, ancestry,
  request identity, participant binding, and durable evidence before mutation.
- Installation or enablement alone never mutates or recovers a paused or blocked
  Run.
- If resume rejects a near-match, report it and stop; do not bypass the check
  with ordinary task or Git tools.

## Communication and task availability

### `COMMUNICATION_FAILURE`

If no Run ID was created, call `consensus_doctor` once, verify the run list did
not change, and retry the exact confirmed mapping at most once.

If a Run ID exists, inspect that Run. Explicit resume may replace a failed or
interrupted participant attempt only when canonical history proves it has no
side-effectful or uncertain item. Otherwise the Run remains paused.

If a patch already succeeded and only its final read-only confirmation was lost,
the daemon may refresh the connection and retry only that confirmation after
revalidating the exact request, clean target, ancestry, frozen refs, participant
identity, and empty or canonical event history. It never repeats the patch.

### `HISTORY_UNAVAILABLE` or `thread not loaded`

Do not resume an ephemeral task or create a replacement mirror manually. The
daemon may rotate an ephemeral binding only when the pending request is proven
unsent, has no turn-start intent, and the frozen Source-history fingerprint is
unchanged. The replacement receives only the still-pending read-only action.

## Invalid declarations or responses

### `INVALID_TEST_COMMAND`

Model-declared test commands cannot invoke Git. After the declaration is fixed,
explicit resume may replace only the exact completed pre-integration read-only
contract or plan turn. Any file change, incomplete command, mutating or external
MCP call, unknown item, or source drift fails closed.

### `INVALID_RESPONSE`

Before integration, explicit resume may replace an exact completed contract,
plan, or plan-verdict turn whose canonical history is side-effect free. After
integration, resume is allowed only for a daemon-recognized response-only
recovery that preserves the authoritative existing Git result. Never infer
eligibility from prose alone.

## Execution and command policy

### `EXECUTION_TOOL_UNAVAILABLE`

Before any target branch or integration SHA exists, explicit resume may retry
the exact accepted Primary action only when the response and canonical history
prove no writes occurred, both frozen sources remain unchanged and clean, and
the target is absent.

### `FORBIDDEN_OPERATION`

Report the denied command and cwd. The daemon may retry a side-effect-free
pre-integration attempt, or archive a completed post-patch response and request
one read-only confirmation after revalidating the successful patch and target.
It never repeats a completed write.

Known safe recovery queries remain narrowly scoped to the frozen Primary cwd
and exact target identity. A near-match command, unknown item, non-agent source,
wrong cwd, incomplete result, or possible write remains terminal.

Matching 0.3.12 artifacts may also recover the exact 0.3.11 diagnostic
`controlled patch call appears after the final agent response`. Resume is
eligible only when canonical history proves a `phase: commentary` message came
before one successful request-bound patch and `phase: final_answer` came after
it, with matching patch provenance, a clean authoritative target, source
ancestry, unchanged frozen refs, and the same participant binding. The daemon
archives only that response attempt and requests a read-only confirmation. A
real post-final command or patch, missing or unknown phase, second patch,
partial history, or drift remains terminal.

## Controlled patch failures

### `PATCH_NOT_AUTHORIZED`

First confirm the exact scoped approval key is effective. Explicit same-Run
resume may retry one request-bound patch call only when every failed call has
the same request and patch, no successful patch record exists for that request,
the authorized target remains clean and unchanged, and canonical participant
history is complete.

If the patch already succeeded, recovery can request only a final read-only
result confirmation; a second patch is forbidden.

### `CONTROLLED_PATCH_TOOL_UNAVAILABLE`

For a legacy blocker, install matching `0.2.8`-or-newer binary and plugin
artifacts, then explicitly call `consensus_resume`; installation alone never
mutates or recovers this Run.

Preserve the same Run, round, integration branch, prior integration SHA, and
failed frozen verification evidence. The daemon may archive only the empty,
side-effect-free correction attempt, reacquire the repository lock, repeat the
participant-tool preflight, and permit at most one request-bound corrective
patch and correction commit. The new SHA must advance and every frozen
verification command runs again.

Do not inject the participant tool manually; an explicit same-Run resume is
still required.

## Verification failures

### Frozen command returned nonzero

This is ordinary machine feedback, not a resume condition. The same Run returns
to Primary for a controlled correction, creates a new SHA, reruns every frozen
command, and reaches Reviewer only after all pass.

### `CARGO_UNAVAILABLE` or another repaired local toolchain blocker

After repairing the environment, explicit resume may replace one exact
completed, side-effect-free verification blocker. The Run, branch, SHA, frozen
refs, and isolated verification worktree remain the same. A second environment
retry or any drift fails closed.

### `VERIFICATION_EXECUTION_UNCERTAIN`

A command was journaled as started without a durable completion. Do not resume
in a way that executes it again automatically. Report the uncertainty and retain
the integration result for manual investigation.

## Legacy migration qualifiers

Legacy recovery is organized by the current blocker, not by release chronology.
Use it only when the daemon recognizes the exact persisted diagnostic.

- An old `CONTROLLED_PATCH_TOOL_UNAVAILABLE` correction Run requires matching
  binary and plugin artifacts with recovery support at least equivalent to
  `0.2.8`, followed by explicit `consensus_resume`. The same Run, round, branch,
  prior SHA, failed verification evidence, empty correction attempt, and lock
  identity must match. At most one request-bound corrective patch and commit may
  advance the SHA, and all frozen verification reruns.
- A legacy blocked read-only current-branch query may be archived only when its
  exact command, cwd, terminal evidence, successful patch provenance when
  applicable, clean target, ancestry, and frozen refs revalidate.
- A legacy approval race may be retried only when one exact request-bound patch
  call failed or remained waiting, no successful patch was recorded, all other
  items are complete and allowlisted, and the target and frozen refs are
  unchanged.
- A legacy eventless ephemeral confirmation may rotate its binding only when
  dispatch is proven unsent and the frozen Source history is unchanged.

Every migration is atomic, bounded, and same-Run. A second attempt, changed
identity, partial history, unknown item, possible side effect, or drift remains
terminal.
