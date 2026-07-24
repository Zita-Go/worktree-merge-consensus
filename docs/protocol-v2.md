# Participant Response Protocol v2

`worktree-merge-consensus/v2` is the participant-facing response protocol used
by release 0.2.0 and later. It separates a very small machine control signal
from the tasks' human-readable reasoning.

The coordinator still persists canonical structured state internally. Tasks no
longer copy run IDs, rounds, plan revisions, hashes, branches, or SHAs into
their responses. Those values are bound to the exact App Server task and turn,
then supplied by deterministic coordinator code.

## Result marker

Every participant response contains exactly one marker:

```text
<consensus-result>VALUE</consensus-result>
```

The marker may appear before or after ordinary prose. The parser requires one
opening tag, one closing tag, and one value allowed for the pending action. A
blocked response may include one optional stable reason:

```text
<consensus-result>BLOCKED:SOURCE_BINDING_MISMATCH</consensus-result>
```

The optional reason contains only uppercase ASCII letters, digits, and
underscores. Everything outside the marker is preserved as Markdown and is not
parsed into fields.

## Action values

| Pending action | Allowed marker values | Body handling |
| --- | --- | --- |
| Primary or Reviewer contract | `CONTRACT_READY`, `BLOCKED` | `CONTRACT_READY` body is one contract JSON object; blocked evidence is Markdown. |
| Primary plan | `PLAN_READY`, `BLOCKED` | Nonempty complete plan in free-form Markdown. |
| Reviewer plan review | `APPROVED`, `CHANGES_REQUIRED`, `BLOCKED` | Feedback is free-form Markdown; it is required only for `CHANGES_REQUIRED`. |
| Primary integration | `INTEGRATION_READY`, `BLOCKED` | Optional Markdown summary. Branch, SHA, and changed files come from Git. |
| Primary verification | `VERIFICATION_READY`, `BLOCKED` | Marker-only handoff. The turn must not run tools; test evidence comes from coordinator-owned App Server `command/exec` calls. |
| Reviewer result review | `APPROVED`, `CHANGES_REQUIRED`, `BLOCKED` | Feedback is free-form Markdown; approval is bound to the exact current SHA. |

## Contract JSON

The contract is the only v2 body with a structured representation. It is one
JSON object, optionally enclosed in a single `json` code fence. The object must
contain a nonempty `tests` array of exact direct non-Git commands. Other fields
may express goals, behavior, decisions and rationale, invariants, interfaces,
edge cases, rejected alternatives, and relevant files using the structure best
suited to the implementation.

Example:

```text
<consensus-result>CONTRACT_READY</consensus-result>
{"goals":["preserve retry behavior"],"tests":["cargo test --workspace"]}
```

## Code-side binding

The coordinator associates each response with its persisted pending send and
canonical App Server turn. It computes and supplies all machine identity:

- contract role from the selected task;
- plan revision and hash from the exact complete plan Markdown;
- plan approval from the exact Reviewer turn that received that plan;
- integration branch, HEAD SHA, changed files, ancestry, source-ref stability,
  cleanliness, and conflict-marker checks from Git;
- test evidence from exact coordinator-journaled `command/exec` results in the
  isolated verification clone, including authoritative exit codes and bounded
  diagnostic output for failures;
- final approval from the exact Reviewer turn that received the current result
  SHA.

Free-form prose is never treated as machine evidence. It is stored and relayed
so the other task can reason about it, and its complete content participates in
progress fingerprints and plan hashes.

## Recovery and compatibility

Release 0.2.0 can still read a valid v1 JSON envelope from an already-running
or migrated Run, but all newly generated prompts request v2 markers. This
compatibility path does not weaken v1 validation.

If a controlled integration patch was already recorded successfully before an
invalid legacy final response, explicit same-Run resume audits the exact
canonical turn, matching successful patch hash, Git result, and frozen refs.
It then archives only that response attempt and requests a read-only
`INTEGRATION_READY` marker. It cannot apply a second patch, recreate the branch,
or repeat the merge.

Release 0.2.1 also permits one narrow same-Run verification retry. It applies
only when the exact completed Primary verification turn returned a result but
contains zero `commandExecution` items. Resume revalidates the unchanged
integration result and isolated clone, rejects every side-effect-capable or
unknown item, archives the empty turn atomically, and reissues the same frozen
verification request once. A partial test run, second empty turn, or changed
integration remains terminal.

Release 0.2.2 requires the Primary to complete every frozen verification
command even after a nonzero exit. `VERIFICATION_READY` signals only that the
complete command evidence set exists. The coordinator derives every exit code
and bounded diagnostic output itself. Any failed command routes the same Run
back to a new controlled Primary integration round with that machine evidence;
verification then runs against the new integration SHA, and Reviewer approval
is requested only after all frozen commands pass. Explicit resume may also
replace one exact, completed, side-effect-free `CARGO_UNAVAILABLE` verification
blocker after the environment is repaired. The integration branch and Run ID
remain unchanged, and a second such recovery is forbidden.

Release 0.2.3 derives side-effect evidence primarily from the live App Server
item lifecycle. The daemon persists `item/started`, `item/completed`, and the
ordered `turn/completed` barrier before it accepts a response, then combines
those exact-turn items with the stored user and final-agent messages. This
supports App Server stores that omit command and MCP items from `thread/read`
without asking participants to serialize evidence in JSON. Full historical
turn items remain a backwards-compatible fallback. For pre-0.2.3 Runs only,
one exact empty-verification plus `CARGO_UNAVAILABLE` recovery sequence may be
followed by one side-effect-free evidence-compatibility retry; SQLite records
that allowance so it cannot repeat.

Release 0.2.4 makes coordinator-started participant turns fully unattended.
Every `turn/start` sends approval policy `never`, including integration and
isolated verification, while retaining the pinned offline sandbox, writable
roots, exact event evidence, and frozen-source checks. No participant command
or file operation should wait for interactive user approval.

Release 0.2.5 sends `sandboxPolicy.type: dangerFullAccess` as well as approval
policy `never` for every participant turn. This requires trusted tasks and
trusted repository contents; coordinator evidence checks fail closed but are
not an OS sandbox and cannot undo an already executed action. Primary
verification is now a marker-only turn that must not run Shell, Git, file, MCP,
or patch tools. After the marker, the coordinator executes every frozen direct
command itself through App Server `command/exec`, in order, against the exact
detached clone. SQLite records STARTED before dispatch and COMPLETED with the
structured result. Exact completed results are reusable after restart; an
uncertain STARTED execution is never repeated automatically.

The same release permits one migration only for the exact legacy 0.2.4 history
and unchanged Run, task, request, branch, integration SHA, verification clone,
and frozen refs. It requires the archived signal sequence
`VERIFICATION_READY`, `BLOCKED:CARGO_UNAVAILABLE`, `VERIFICATION_READY`, one
prior evidence-compatibility archive, and a final completed side-effect-free
marker turn. Resume archives only that final turn and restores the same frozen
verification request. It cannot repeat a patch, branch creation, merge, commit,
or source-ref update, and a second migration is forbidden.

Release 0.2.6 fixes archived event cleanup for that reusable request record.
If v0.2.5 already restored the request but then blocked on the exact SQLite
`turn_event_completions.turn_record_id` collision, daemon startup validates the
unchanged Git result and completed marker-only active turn before removing only
the stale archived event rows and continuing the same action. This is not a
second resume or migration and cannot create another patch, merge, commit, or
verification execution during repair. Any near-match remains blocked.

Before the first Primary action, release 0.2.7 establishes a durable
participant binding. The selected frozen task is the **Source Primary**. When
it is `notLoaded`, the coordinator loads it with the task-scoped
`worktreeMergeConsensusParticipant` configuration and uses it directly as the
**Effective Primary**. A preloaded Source Primary with exactly
`consensus_apply_patch` also binds directly. A preloaded Source Primary without
that tool is represented by a `thread/fork` created with `ephemeral: true` and
`excludeTurns: false`, producing an ephemeral full-history mirror. Before the
fork, `thread/goal/get` on the Source Primary
must return null, and the fork request must not carry or continue a goal. The
fork is accepted only if it is an idle, ephemeral full-history mirror with the
same canonical turn-ID sequence and an exact complete MCP inventory. It
represents the Source Primary rather than becoming a third source or reviewer,
and it carries no active Source goal. The coordinator does not query
`thread/goal/get` on the ephemeral mirror because supported Codex runtimes may
reject goal operations for ephemeral tasks.

Before every Primary turn, the coordinator resumes the Effective Primary and
fully paginates `mcpServerStatus/list` before `turn/start`. The only accepted
participant tool inventory is exactly `consensus_apply_patch`; the operator
plugin's eight tools do not prove participant visibility. Reviewer routing is
unchanged, while the selected source task IDs, refs, worktrees, and SHAs remain
frozen. A lost mirror may be recreated only between completed actions with no
pending or uncertain send. Pending or uncertain turns are never reforked or
resent, and an uncertain non-idempotent `thread/fork` is never automatically
repeated. This protocol depends on the experimental Codex CLI `>=0.144.1` App
Server surface.

After a matching 0.2.8 deployment, explicit resume may recover only the exact
post-0.2.6 `CONTROLLED_PATCH_TOOL_UNAVAILABLE` correction blocker: the same
Run, round, branch, old integration SHA, and failed frozen verification
evidence, with one otherwise empty side-effect-free correction turn. Recovery
archives only that turn, reacquires the Run lock, repeats participant preflight,
and retries the same request. It permits one request-bound corrective patch and
commit only; the integration SHA must advance and all frozen verification is
rerun. Installing or enabling the operator plugin alone never mutates or
recovers a blocked Run.

Release 0.2.8 routes an ephemeral Effective Primary through the App Server
surface that ephemeral tasks actually support. The coordinator checks identity
and liveness with `thread/read(includeTurns: false)`, never calls
`thread/read(includeTurns: true)`, `thread/turns/list`, or `thread/resume` for
that binding, and accepts a terminal turn only from durable matching
`item/started`, `item/completed`, and `turn/completed` events. The binding
stores a hash of the frozen Source Primary turn-ID sequence. Every send stores
turn-start intent before dispatch; if delivery becomes uncertain without a
turn ID, automatic resend and refork are forbidden. Stored Source, Reviewer,
and direct Primary histories continue to use canonical full-history reads.

Release 0.2.9 makes post-turn integration command auditing side-effect-aware.
Approved write commands remain canonical only when completed with exit code
zero. Retry-safe read-only commands may have a numeric nonzero terminal result;
this is archival safety, not evidence that the check succeeded. Explicit
same-Run resume is available only when the exact request-bound controlled patch
and integration commit already succeeded and the legacy audit then blocked the
completed turn. Recovery verifies the successful patch record, frozen refs,
clean target result, both source ancestors, and final SHA, archives only that
response attempt, and starts one read-only confirmation turn without repeating
the branch, merge, patch, staging, or commit. The historical
`git diff --no-index -- /dev/null <normalized-relative-path>` form is accepted
only by this recovery audit and remains denied by live approval. An explicit
null App Server `pluginId` is accepted only for the exact injected participant
server and patch tool.

Release 0.2.10 corrects the repository preflight for this recovery. After the
successful commit, the Primary worktree may be attached to the exact authorized
target branch instead of the frozen source HEAD. The
integration-in-progress check still requires an unchanged Reviewer worktree,
unchanged frozen source refs, and the same repository. Authoritative target,
patch provenance, source ancestry, cleanliness, changed files, and final SHA
are revalidated before archival.

Malformed, missing, duplicate, unknown, or action-incompatible markers fail
closed with `INVALID_RESPONSE`. A v1 response remains governed by the
[legacy v1 protocol](protocol-v1.md).
