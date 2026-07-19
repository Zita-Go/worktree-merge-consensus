# Explicit Task-to-Worktree Binding Design

Date: 2026-07-19

Status: approved in conversation; awaiting written-spec review

This specification amends the supported-environment, task-selection, Git
preflight, CLI, plugin, and acceptance sections of the original 2026-07-18
design. Where those sections require a task's App Server cwd to be its source
worktree, this document supersedes that requirement. The existing consensus
phase machine and post-freeze safety model remain authoritative.

## Context

The current implementation infers each source worktree from the selected Codex
task's `ThreadSummary.cwd`. That assumption is too strict. A task may be created
from an unrelated directory and later modify a persistent Git worktree through
an absolute path, `git -C`, or an internal `cd`. The App Server continues to
report the task's original working directory even though its committed source
belongs to another worktree.

Task identity and source identity are separate facts:

- a Codex thread ID identifies the conversation whose history and intent must
  participate in consensus;
- a registered Git worktree, source ref, and HEAD SHA identify the code that
  must be frozen and integrated.

The coordinator must bind these facts explicitly instead of treating task cwd
as proof of source ownership.

## Goals

- Allow both selected Codex tasks to have arbitrary, identical, non-Git, or
  changing registered working directories.
- Let users independently select the primary and reviewer tasks and the two
  persistent source worktrees.
- Retain deterministic Git validation, source freezing, least-privilege turn
  policies, reviewed integration, isolated verification, and exact-SHA result
  approval.
- Support both interactive selection and complete non-interactive CLI/MCP
  automation.
- Detect the legacy same-named standalone Skill that can otherwise mask the new
  plugin-backed workflow.
- Preserve the local-only result boundary: one new integration branch, no
  push, no pull request, and no update to either source ref.

## Non-goals

- Inferring source ownership from shell history, command items, or model
  recollection.
- Supporting sources that exist only as loose commits or refs without two
  persistent registered worktrees.
- Coordinating tasks or worktrees on different hosts or under different Unix
  accounts.
- Allowing a frozen run to remap either task or worktree.
- Proving cryptographically that a particular task authored a commit. The user
  supplies the task-to-worktree association, and the daemon verifies all Git
  facts that can be checked mechanically.

## Considered Approaches

### Explicit independent binding

Select tasks and worktrees independently, then confirm the complete mapping.
This is the chosen approach. It is deterministic, keeps user intent explicit,
and does not add a model-dependent pre-freeze phase.

### Task self-report

Ask each task to report its source path, ref, and SHA before freezing the run.
This offers less user input but depends on model recollection, introduces a new
protocol phase, and cannot establish ownership more strongly than explicit user
selection.

### Command-history inference

Parse historical command cwd values, `cd`, and `git -C` invocations. This is
rejected because direct file operations, missing history, multiple repositories,
and path changes make the result incomplete and ambiguous.

## Identity Model

A run freezes two independent associations:

```text
primary_thread_id  -> primary_worktree -> primary_ref -> primary_sha
reviewer_thread_id -> reviewer_worktree -> reviewer_ref -> reviewer_sha
```

The thread IDs must differ. The canonical worktree paths must differ. Both
worktrees must resolve to the same canonical Git common directory. A task's
App Server cwd remains useful display metadata only and has no authority over
the binding.

`RunFacts` already stores thread IDs and worktree paths separately, so the
persisted identity model and task protocol envelopes do not require a new
field. The coordinator must stop deriving the stored worktrees from task cwd.

## CLI Experience

### Interactive run

`codex-consensus run` performs this sequence in a TTY:

1. List all tasks visible to the same local App Server.
2. Select the primary task.
3. Select a different reviewer task without filtering by cwd.
4. Resolve a repository anchor from `--repository`, the current directory when
   it is a Git worktree, or an interactive absolute-path prompt.
5. Run `git worktree list --porcelain` against the anchor's common repository.
6. Display registered worktrees with canonical path, source ref or detached
   state, abbreviated HEAD SHA, and clean/dirty status.
7. Select the primary source worktree.
8. Select a different reviewer source worktree.
9. Display and confirm the complete task-to-worktree-to-ref-to-SHA mapping.
10. Freeze the run only after confirmation.

Task rows may continue to show the App Server cwd for orientation, but labels
must not imply that it is the selected source worktree.

### Non-interactive run

Automation supplies all four binding values:

```bash
codex-consensus run \
  --primary-thread THREAD_A \
  --primary-worktree /repo/.worktrees/change-a \
  --reviewer-thread THREAD_B \
  --reviewer-worktree /repo/.worktrees/change-b \
  --integration-branch consensus/combined \
  --test "cargo test --workspace" \
  --json
```

The two thread flags form one required pair, and the two worktree flags form a
second required pair. JSON or other non-interactive use requires both complete
pairs and never opens a prompt. Interactive use may prompt for omitted pairs.
Supplying only one member of either pair is a validation error.

### Worktree discovery command

The CLI adds a stable discovery command:

```bash
codex-consensus worktrees list --repository /repo --json
```

The JSON output includes the canonical worktree path, canonical common
directory, source ref when attached, full HEAD SHA, and clean state. Discovery
is read-only and reports invalid or inaccessible entries rather than pruning or
repairing them.

## Plugin and MCP Experience

The plugin remains a launcher for the persistent daemon. Its workflow becomes:

1. `consensus_doctor`
2. `consensus_list_threads`
3. user selection of primary and reviewer task roles
4. collection of a repository or worktree anchor path
5. `consensus_list_worktrees({ repository_path })`
6. user selection of the two source worktrees
7. `consensus_start` with both task IDs and both worktree paths

`consensus_list_threads` returns all tasks visible on the host and does not
filter them by Git state or cwd. A new `consensus_list_worktrees` MCP tool
exposes the same read-only discovery data as the CLI. `consensus_start`
requires `primary_worktree` and `reviewer_worktree` in addition to the two task
IDs. Optional integration-branch and extra-test inputs remain unchanged.

The plugin Skill must present the complete mapping for confirmation. It must
not ask the user to create tasks in particular directories and must not infer a
worktree from a selected task's cwd.

## Preflight and Freeze

Before creating a run, the daemon independently validates the supplied paths:

- both paths are absolute and can be canonicalized;
- both are registered by `git worktree list --porcelain` for the same common
  repository;
- neither path is a bare repository entry;
- the canonical worktree paths differ;
- both worktrees are clean;
- each HEAD resolves to a readable 40-hex commit;
- each attached source ref and its target SHA can be recorded;
- the integration branch is unique and absent;
- both selected tasks are distinct and visible through the same managed App
  Server.

Detached source worktrees remain supported and are frozen by path and SHA.
User confirmation authorizes exactly the displayed mapping. No task cwd
comparison occurs during preflight or later revalidation.

## Coordinator Execution

The existing consensus phases remain unchanged after freeze. Every task turn
uses the explicitly bound worktree as its execution cwd and runtime workspace
root, irrespective of the task's original or subsequently reported cwd.

- Contract, plan, and review turns remain read-only and offline.
- The reviewer remains unable to write either source or integration state.
- The primary integration turn may write only the bound primary worktree and
  the required source Git common directory, subject to the existing Git command
  allowlist.
- Verification remains a separate turn in an exact-SHA, detached, remote-free
  clone with only frozen test commands allowed.

The coordinator continues to verify that both task IDs exist, but removes the
check that `ThreadDetail.summary.cwd` resolves to the frozen source worktree.
All source drift, ancestry, changed-file, conflict-marker, test-evidence, and
unchanged-ref checks operate on the frozen worktrees and SHAs exactly as before.

The first prompt sent to each task includes its role, bound worktree, source
ref, and frozen SHA. The task inspects the code at the supplied execution cwd
while using its existing conversation history to build its contract. It must
return `SOURCE_BINDING_MISMATCH` if that worktree does not contain the
implementation represented by its history. It may not search for or switch to
another source directory.

## Failure Semantics

Preflight uses explicit reason codes:

| Reason | Condition |
| --- | --- |
| `UNREGISTERED_WORKTREE` | A selected path is not a registered persistent worktree. |
| `DUPLICATE_WORKTREE` | Both roles select the same canonical worktree. |
| `REPOSITORY_MISMATCH` | The worktrees have different canonical common directories. |
| `DIRTY_WORKTREE` | Either selected source contains uncommitted changes. |
| `WORKTREE_UNAVAILABLE` | A selected path is missing or inaccessible. |
| `SOURCE_DRIFT` | A frozen path, HEAD, or attached source ref changes. |
| `SOURCE_BINDING_MISMATCH` | A task rejects the frozen user-selected source association. |

Interactive validation errors return to selection before a run is frozen.
Once confirmed, task IDs, paths, refs, and SHAs are immutable. A later binding
mismatch or source change terminates the run as `BLOCKED`; `resume` cannot
replace frozen identity. The user corrects the association or Git state and
starts a new run.

## Legacy Skill Conflict

The existing manually installed Skill at
`$CODEX_HOME/skills/worktree-merge-consensus` has the same public name as the
plugin Skill but follows the older native thread-routing workflow. This can
make the UI appear to launch the plugin while no MCP tools are available.

`doctor` and `consensus_doctor` must inspect the effective Codex home for this
legacy path. When it exists and the plugin-backed MCP server is not the active
surface, diagnostics return `LEGACY_SKILL_CONFLICT` with migration guidance.
The tool never deletes or overwrites the directory automatically. Binary and
plugin release versions must match, and installation documentation requires a
Codex restart or new task after plugin changes.

## Compatibility

The task-to-task `worktree-merge-consensus/v1` envelope and phase machine do
not change. Persisted run facts already separate task IDs from source paths, so
the database schema does not require a migration. Existing frozen runs retain
their recorded mappings and can resume.

The CLI and MCP start interfaces are intentionally tightened for new runs.
Plugin and binary artifacts must ship from the same release so the new required
MCP arguments and tool list cannot drift. Codex CLI compatibility remains
`>=0.144.1`, subject to the existing managed App Server identity, method, and
response-shape checks.

## Automated Verification

Unit, process, and end-to-end coverage must prove:

- two tasks with the same App Server cwd can bind different worktrees;
- tasks whose App Server cwd is outside any Git repository can bind valid
  worktrees;
- a task cwd change after freeze does not affect a run;
- interactive task selection no longer filters by cwd;
- interactive and JSON worktree discovery return canonical Git facts;
- partial CLI and MCP binding arguments fail closed;
- duplicate, unrelated, unregistered, dirty, inaccessible, and drifting
  worktrees produce the exact reason codes;
- all task turns use the bound worktree cwd and expected sandbox roots;
- reviewer turns remain read-only and primary writes remain bounded;
- daemon restart preserves the frozen mapping without duplicate actions;
- existing conflict resolution, isolated test evidence, result approval, and
  source-ref invariants still pass.

The fake App Server acceptance fixture must deliberately report identical or
non-Git task cwd values while expecting turns to execute in two explicit source
worktrees. This prevents accidental reintroduction of cwd inference.

## Real Codex Acceptance

The first real run uses the ordinary public fork checkout on `basestream-cpu`:

```text
/gpfs/users/i-zhangguoqiang/workspace/gh_testtest
/gpfs/users/i-zhangguoqiang/workspace/gh_testtest/.worktrees/feature-expansion
```

The two existing tasks may both retain
`/gpfs/users/i-zhangguoqiang/workspace/gh_testtest` as their registered cwd.
The run explicitly maps each task to a different source worktree. Acceptance
evidence must show:

1. the plugin lists the existing same-host tasks;
2. the plugin lists both registered worktrees with accurate refs and SHAs;
3. identical task cwd values do not block startup;
4. each coordination turn executes at the frozen bound worktree;
5. plan and result review preserve both task contracts;
6. the accepted SHA contains both frozen source commits;
7. every frozen test has authoritative passing command-item evidence;
8. both source refs remain at their frozen SHAs;
9. only the new local integration branch is created, with no push, PR, or
   existing-branch update.

The ordinary repository and source tasks must not describe themselves as a
coordinator fixture. Redacted acceptance evidence belongs only in this tool's
real-Codex smoke-test record.
