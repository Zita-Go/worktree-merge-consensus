# Changelog

## 0.3.8

- Recognize only the exact App Server `-32600 / thread not loaded: <id>`
  response for the requested ephemeral Primary identity; near-match identities
  and every other transport failure remain fail-closed.
- Resume the same eventless successful-patch Run after that exact ephemeral
  task has been unloaded by a hot reload or App Server lifecycle transition.
  The coordinator repeats all 0.3.7 provenance and Git checks before recovery.
- Atomically archive the old attempt, rotate its binding only after the pending
  request is provably unsent and retains the same frozen Source-history hash,
  and ask the replacement ephemeral Primary for the final read-only
  confirmation. The controlled patch, merge, staging, and commit are not run
  again.

## 0.3.7

- Recover one exact ephemeral Primary integration turn when its request-bound
  controlled patch and clean commit succeeded but the App Server event stream
  lost every item and completion notification after the task became idle.
- Revalidate successful patch provenance, the clean authorized target branch,
  source ancestry, and frozen source references before recovery. Refresh the
  App Server proxy, archive the eventless attempt atomically, and retry only the
  final read-only `INTEGRATION_READY` confirmation; never repeat the patch.
- Bound this compatibility path to one attempt per deterministic request and
  continue to fail closed for partial event histories, dirty worktrees, source
  drift, mismatched bindings, or a second lost confirmation.
- Apply the same exact recovery after an initial proxy refresh failure has
  paused the Run: explicit same-Run resume rechecks the idle ephemeral task and
  every Git/provenance invariant before retrying confirmation only.

## 0.3.6

- Authorize the request-bound controlled patch after a successful frozen
  verification when Reviewer result feedback advances the same Run to a new
  correction round. Previously this legitimate post-review state was mistaken
  for an inactive patch request and failed with `PATCH_NOT_AUTHORIZED`.
- Recover that exact accepted blocker once for both direct and ephemeral
  Primary bindings. Recovery requires durable completed-turn evidence, the
  original request marker and response hash, identical failed patch payloads,
  exact `PATCH_NOT_AUTHORIZED` results, no successful patch record, a clean
  unchanged integration SHA, complete successful frozen tests, and retained
  Reviewer feedback.
- Preserve the existing integration commit and retry only the rejected
  correction request. The recovered Run must still create a new SHA, rerun all
  frozen verification commands, and return to Reviewer result approval.

## 0.3.5

- Reconstruct archived ephemeral turns from their durable item-event rows before
  validating request markers and command evidence. Real App Server
  `turn/completed` notifications may contain `items: []` with
  `itemsView: "notLoaded"`; the item events remain the canonical full trace.
- Exercise every persisted ephemeral integration recovery fixture with that
  production completion shape so offline replay cannot accidentally audit only
  the summary envelope.

## 0.3.4

- Preserve completed ephemeral turn payloads and command-item evidence when a
  same-Run retry archives an earlier attempt, instead of deleting the only
  durable proof needed by a later recovery.
- If a read-only post-patch confirmation task is unloaded before producing any
  durable event, re-audit and replay the already-completed archived integration
  turn on the same Run. The coordinator verifies the exact request marker,
  command policy, successful patch provenance, clean target, ancestry, and
  unchanged frozen sources before doing so.
- Process a pending Primary turn directly from durable completion evidence
  without requiring its ephemeral task to remain loaded. The lost confirmation
  is archived as unavailable; merge, patch, staging, and commit are never
  repeated.

## 0.3.3

- Accept only the exact read-only target preflight
  `git show-ref --verify --quiet refs/heads/<target>` in addition to the
  existing non-quiet form; continue to reject reordered flags, other refs,
  extra arguments, and every write-capable `show-ref` shape.
- Let same-Run recovery retain the canonical exit-one target-absence result,
  then continue auditing the already-completed merge, controlled patch,
  staging, Unicode commit token, and clean result without repeating them.
- Replace the synthetic recovery preflight in the production-shaped
  regression with the exact command observed in the real ephemeral Primary
  turn, and document both accepted spellings in the integration prompt.

## 0.3.2

- Accept a single Unicode letter-or-number commit token, plus `-`, `_`, `.`,
  or `:`, under the existing 120-byte integration command limit. Continue to
  reject whitespace, shell metacharacters, slashes, emoji, and multi-token
  messages fail-closed.
- Recover the same Run when a completed ephemeral Primary integration used
  such a safe Unicode commit token and an older policy incorrectly classified
  it as `FORBIDDEN_OPERATION`; re-audit the persisted command evidence without
  repeating merge, patch, staging, or commit.
- Define `ONE_SAFE_TOKEN` explicitly in the Primary prompt and add exact
  policy plus production-shaped same-Run recovery regressions.

- Rework the English and Simplified Chinese project landing pages around the
  user problem, visible Codex workflow, quick installation, exact output, and
  concise safety boundaries instead of duplicating release-by-release internals.
- Add a reproducible two-worktree demo, dedicated safety and recovery guides,
  contribution guidance, structured issue templates, and a GitHub-ready social
  preview asset.
- Keep deep participant-binding, compatibility, and same-Run recovery contracts
  in canonical technical documents while adding documentation gates for README
  length, quick-start placement, visual assets, and bilingual links.

## 0.3.1

- Accept exactly `git branch --show-current`, directly or through one canonical
  `/bin/bash -lc` wrapper, as a read-only current-branch query in the frozen
  Primary worktree; keep every argument-bearing or write-capable `git branch`
  form outside the existing exact branch-list exception fail-closed.
- Make the integration prompt explicit that the coordinator derives and
  validates branch/HEAD identity itself, while naming both accepted equivalent
  current-branch queries so model command choice cannot change the outcome.
- Recover the same Run after either an interrupted command denial or a
  completed post-patch confirmation that misclassified this exact query.
  Recovery revalidates canonical terminal history and the authoritative target
  and never repeats the patch, merge, staging, or commit.
- Add regressions proving a revised plan resets the effective unchanged-review
  fingerprint. `no_progress_rounds` remains the configured threshold, not the
  current no-progress counter.

## 0.3.0

- Persist a bounded public event for every consensus Run state transition in
  the same SQLite transaction as the state change, with monotonic cursors that
  survive daemon and launcher restarts.
- Add `consensus_wait`, a bounded long-poll MCP tool that returns public
  contracts, plans, Reviewer feedback, integration identity, test evidence,
  and final acceptance without exposing hidden reasoning, prompts, raw task
  history, or command output.
- Keep nonterminal polling responses small, include the cumulative snapshot
  only at pause or termination, cap each artifact at 48 KiB and each batch at
  six events, and retain the existing full `consensus_status` contract for
  compatibility.
- Add `codex-consensus watch` for human-readable or JSON-lines observation with
  cursor resume support.
- Keep the launcher task active after `consensus_start` so Codex App users see
  stage, round, proposal, objections, verification, and final result updates in
  the task where they launched the workflow.

## 0.2.15

- Recover the production confirmation shape in which the one successful
  controlled patch belongs to an archived ephemeral Primary attempt while the
  current completed attempt contains only read-only result confirmation.
- Validate the archived patch record and frozen Source lineage separately from
  the current confirmation turn instead of requiring a second patch call in
  that turn.
- Permit only canonical, agent-initiated, exit-zero read-only commands in the
  Primary worktree before the final response; reject MCP calls, file changes,
  dynamic tools, writes, uncertain commands, and commands after the response.
- Revalidate the unchanged frozen refs, clean authorized target, source
  ancestry, and authoritative integration result before archiving only the
  current confirmation and retrying the same Run.
- Preserve the existing integration branch, commit, request, binding lineage,
  and single patch record; never repeat the patch, merge, staging, or commit.

## 0.2.14

- Permit exactly `git symbolic-ref --short HEAD`, either directly or through
  one canonical App Server shell wrapper, as a read-only current-branch query
  in the frozen Primary worktree.
- Keep every other `git symbolic-ref` form forbidden, including alternate
  references, option variants, deletes, and two-argument writes.
- Add an explicit same-Run migration for the exact 0.2.13
  `FORBIDDEN_OPERATION` blocker produced after a successful integration when
  that one query was misclassified during the read-only confirmation.
- Revalidate the completed turn, request and binding provenance, successful
  controlled patch, frozen source refs, clean target, source ancestry, and
  authoritative integration result before atomically archiving only the
  confirmation and reacquiring the repository lock.
- Retry only the result confirmation; never repeat the patch, merge, staging,
  or commit, and keep every near-match or side-effectful history terminal.

## 0.2.13

- Load a persisted Source Primary reported as `notLoaded` with the
  task-scoped participant configuration before recreating its ephemeral
  Effective Primary mirror.
- Verify the resumed Source identity and idle state, then preserve the
  existing frozen Source-history fingerprint while rotating the mirror.
- Add an explicit same-Run migration for the exact 0.2.12
  `HISTORY_UNAVAILABLE` blocker produced before the proven-unsent replacement
  request was dispatched.
- Reacquire the repository lock only when the pending request has no task ID,
  turn ID, or turn-start intent and its active binding, archived completed
  patch attempt, request hash, and frozen lineage all match.
- Keep every sent, uncertain, divergent, mixed-provenance, or near-match state
  terminal, with end-to-end regressions for accepted and rejected boundaries.

## 0.2.12

- Recover a same-Run completed-integration confirmation when its ephemeral
  Effective Primary has disappeared but the replacement request is provably
  unsent: no effective task ID, turn ID, or turn-start intent has been stored.
- Rotate the ephemeral binding and rebind that pending request in one SQLite
  transaction while preserving the frozen Source Primary history fingerprint.
- Retain successful controlled-patch provenance on the archived completed
  generation and accept it across the replacement generation only when both
  bindings share the exact frozen ephemeral lineage and archived request.
- Continue to fail closed for sent, intent-recorded, uncertain, divergent, or
  mixed-provenance requests, with unit and end-to-end regressions for both the
  accepted and rejected boundaries.

## 0.2.11

- Recognize Codex App Server `unifiedExecStartup` command items as
  agent-initiated execution evidence during completed-integration and
  interrupted-turn recovery.
- Continue to accept a missing source only as the App Server schema's legacy
  default while rejecting `userShell`, `unifiedExecInteraction`, null,
  malformed, and unknown sources.
- Add focused recovery regressions using the canonical source emitted by Codex
  0.145.0 and preserve the existing command, cwd, terminal-result, side-effect,
  frozen-state, and target-result checks.

## 0.2.10

- Revalidate a completed-integration command-audit recovery with the authorized
  integration-in-progress policy instead of requiring the Primary worktree to
  remain checked out at its frozen source HEAD.
- Preserve the same frozen source-ref, reviewer-worktree, target-branch, patch,
  ancestry, cleanliness, and final-SHA checks while allowing the Primary
  worktree to be attached to the already-created integration branch.
- Add a regression test that makes the frozen-HEAD check fail after a
  successful integration commit and proves same-Run recovery uses the
  authorized target-branch path.

## 0.2.9

- Audit integration commands by side effect: approved writes still require a
  canonical completed result with exit code zero, while retry-safe read-only
  inspections may be archived after a canonical nonzero terminal result.
- Recover the exact completed integration turn that successfully applied its
  request-bound patch and commit before the legacy command audit blocked it.
  Explicit resume preserves the same Run and existing integration result,
  archives only the rejected response attempt, and requests one read-only
  confirmation without repeating a write.
- Accept an explicit null App Server `pluginId` only for the exact injected
  participant server and patch tool, while retaining all request, generation,
  patch-hash, and source-identity checks.
- Match retry diagnostics to a provenance-complete ephemeral Effective Primary
  without weakening the frozen Source Primary identity.
- Direct Primary integration turns to use `git ls-files` when `rg` is absent
  and to stage new files before inspecting them instead of using
  `git diff --no-index`.

## 0.2.8

- Support Codex App Server ephemeral task constraints by using summary-only
  reads and never calling `thread/resume` for ephemeral Effective Primary
  tasks.
- Reconstruct ephemeral terminal turns from durably journaled item and turn
  events.
- Persist Source Primary history identity and pre-dispatch turn-start intent
  so changed history or uncertain delivery fails closed without duplicate
  sends or replacement forks.
- Enforce the same contract in unit and process-level acceptance fakes.

## 0.2.7

- Introduce durable Source/Effective Primary bindings and verified ephemeral
  full-history participant forks for preloaded Primary tasks.
- Inject and preflight the request-bound participant patch capability before
  Primary actions.
