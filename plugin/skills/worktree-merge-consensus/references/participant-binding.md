# Primary Participant Binding

Use this reference only when explaining a Primary binding, ephemeral task,
participant MCP inventory, or `consensus_apply_patch` preflight failure.

## Identities

- **Source Primary**: the user-selected, frozen original development task.
- **Effective Primary**: the task that receives coordinator Primary turns.

The Source Primary's selected task ID, worktree, source ref, commit SHA, and
canonical history fingerprint remain frozen. An Effective Primary mirror does
not create a third source and does not change Reviewer routing.

## Binding decision

Binding happens before the first Primary action:

| Source Primary state | Binding |
| --- | --- |
| `notLoaded` | Resume it with task-scoped participant configuration and bind directly. |
| Preloaded with the exact participant tool | Bind directly. |
| Preloaded without the participant tool | Create an ephemeral full-history mirror with participant configuration. |

In other words, a `notLoaded` Source Primary binds directly after task-scoped
resume. A preloaded Source Primary that lacks the exact participant tool uses an
ephemeral full-history `thread/fork` mirror instead.

Before that ephemeral fork, require the Source Primary to be idle and have no
active goal. The full-history fork must match the frozen canonical turn IDs and
history fingerprint. The mirror does not inherit an active goal.

## Ephemeral constraints

An ephemeral Effective Primary is not treated as a stored task:

- read only summary state with `thread/read(includeTurns: false)`;
- never call `thread/read(includeTurns: true)`, `thread/turns/list`,
  `thread/resume`, or goal operations on the ephemeral task;
- derive completion from durable matching `item/*` and `turn/completed` events;
- recreate a mirror only between completed actions when the Source history is
  unchanged.

A pending or uncertain turn is never reforked or resent. Persisted start intent
prevents an uncertain delivery from being repeated. An uncertain fork response
also fails closed instead of being retried automatically.

Direct Effective Primary tasks retain stored-history reads and task resume.
Reviewer routing and both frozen source identities remain unchanged for either
binding mode.

## Tool preflight

Before every Primary turn, the coordinator:

1. resumes or checks the Effective Primary as appropriate for its binding mode;
2. fully paginates `mcpServerStatus/list`;
3. requires the participant server inventory to expose exactly one tool,
   `consensus_apply_patch`;
4. completes that preflight before `turn/start`.

The operator plugin's tool surface does not prove participant tool visibility.
Missing, extra, disabled, or mismatched participant tools fail before the
Primary turn starts.

## Controlled patch boundary

`consensus_apply_patch` is participant-only and request-bound. The daemon checks
the exact Run ID, request hash, pending Primary turn, clean authorized target,
both frozen ancestors, unchanged source refs, safe text-only patch shape, and
single-use record before applying it.

The launcher never calls this tool. A successful patch remains attributed to
its exact request and binding generation; a retry may not silently apply it a
second time.

## Binding diagnostics

- `SOURCE_BINDING_MISMATCH`: the selected task says the frozen source does not
  match its own context. Stop and ask the user to correct the mapping.
- `CONTROLLED_PATCH_TOOL_UNAVAILABLE`: inspect the exact Effective Primary and
  MCP inventory; use same-Run recovery only when the daemon permits it.
- `HISTORY_UNAVAILABLE`: do not construct a new mirror manually. Preserve the
  same Run and read `recovery.md`.
- `thread not loaded`: distinguish the frozen Source from an ephemeral mirror;
  never resume an ephemeral task directly.
- incomplete or uncertain dispatch: preserve the pending record and do not
  resend.

The required experimental App Server method contract begins at Codex
`>=0.144.1`.
