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
| Primary verification | `VERIFICATION_READY`, `BLOCKED` | Optional Markdown summary. Test evidence comes from App Server command items. |
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
- test evidence from exact completed `commandExecution` items in the isolated
  verification clone, including authoritative exit codes and bounded diagnostic
  output for failures;
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

Malformed, missing, duplicate, unknown, or action-incompatible markers fail
closed with `INVALID_RESPONSE`. A v1 response remains governed by the
[legacy v1 protocol](protocol-v1.md).
