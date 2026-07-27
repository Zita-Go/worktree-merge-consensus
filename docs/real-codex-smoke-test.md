# Real Codex Smoke-Test Record

## Release gate

The stable-release gate was completed for 0.3.15 on 2026-07-27. A real
same-host Run on Codex CLI and App Server 0.145.0 reached `ACCEPTED` after the
Reviewer found a compatibility regression, requested concrete corrections, and
approved the exact corrected SHA. The coordinator recorded six successful
frozen commands in an isolated clone, revalidated both source refs, and retained
only a new local integration branch with no push or PR.

Stable qualification combines this redacted real-Codex record with the
process-level App Server suite, disposable Git fixtures, static release builds,
RustSec audit, and license checks. The exhaustive fault-injection checklist
below is retained for adapter or safety-boundary changes; it is not manually
repeated for every patch release when the owning automated regressions pass.
The Codex App Server dependency remains experimental even when this project
publishes a stable release.

## Required environment

- A supported Linux x86_64 or ARM64 host.
- Exact `codex --version` output satisfying `>=0.144.1`.
- One disposable local Git repository with two committed branches checked out
  in two distinct worktrees.
- Two existing Codex tasks under the same local Codex account. Their reported
  cwd may be identical or outside Git because source worktrees are bound
  explicitly.
- No remote is required and no push is permitted.

## Extended manual regression checklist

Use this full matrix when the App Server adapter, participant binding, command
policy, controlled-patch boundary, durable journal, or recovery semantics
change materially. Ordinary patch releases still require a real accepted Run
covering their changed production path plus the complete automated release
gate.

1. Record `codex --version`, `git --version`, OS, architecture, and the
   `codex-consensus` release SHA.
2. Record both task IDs, worktree paths, source refs, and source SHAs. Use only
   disposable code and redact account identifiers. If doctor reports
   `LEGACY_SKILL_CONFLICT`, manually back up/remove the old standalone Skill,
   install matching binary/plugin release versions, and restart Codex first.
3. Give the two branches a small overlapping change whose correct integration
   requires preserving a reviewer-only behavior and resolving one conflict.
4. Confirm both source worktrees are clean and the intended integration branch
   does not exist.
5. Run:

   ```bash
   codex-consensus configure --json
   codex-consensus doctor --json
   codex-consensus worktrees list \
     --repository /repo \
     --json
   codex-consensus run \
     --primary-thread PRIMARY_TASK_ID \
     --primary-worktree /repo \
     --reviewer-thread REVIEWER_TASK_ID \
     --reviewer-worktree /repo/.worktrees/feature-expansion \
     --integration-branch consensus/real-smoke \
     --test "cargo test --workspace" \
     --json
   codex-consensus status RUN_ID --json
   ```

6. Confirm both task summaries may report the same repository-root cwd while
   every turn executes at its separately frozen worktree. Observe at least one
   plan correction from the reviewer, exact plan approval,
   primary-only source Git writes, and exact result-SHA approval. Confirm review
   turns and the integration turn all report `approvalPolicy: "never"` and
   `sandboxPolicy.type: "dangerFullAccess"`. Confirm the selected tasks and
   repository are trusted because this mode has no App Server OS sandbox.
   Confirm the separate Primary verification turn is marker-only and contains
   no Shell, Git, file, MCP, or patch item. After that marker, confirm the
   coordinator uses `command/exec` to run each frozen command exactly once in
   the persisted `verification_worktree` and records deterministic evidence
   with turn ID, item ID, command, cwd, exit code, and bounded failure
   diagnostics. Exercise one nonzero command and confirm the same Run returns
   to a controlled integration correction, verifies the new SHA, and proceeds
   to result review only after every frozen command passes.
   Confirm neither task asks the user to approve a command, file operation, or
   internal controlled patch.
   On an App Server whose completed `thread/read` history omits command or MCP
   items, confirm the daemon has persisted matching `item/started`,
   `item/completed`, and `turn/completed` evidence in private SQLite for
   participant side-effect and controlled-tool auditing. Confirm coordinator
   verification evidence comes from its separate command journal and does not
   require either task to serialize command output in JSON.
   Confirm contracts use one result marker plus a JSON body, while the plan,
   review feedback, integration summary, verification summary, and final review
   use one result marker plus ordinary Markdown. No participant should have to
   repeat run IDs, plan hashes, branches, SHAs, changed files, or test evidence.
   Exercise both Primary binding paths supported by Codex CLI `>=0.144.1`.
   First use a Source Primary that App Server reports as `notLoaded`; confirm it
   is loaded with participant configuration, the Effective Primary equals the
   Source Primary, and no `thread/fork` occurs. Then use a preloaded Source
   Primary without the participant tool; confirm one non-retried
   `thread/fork` creates an `ephemeral: true`, `excludeTurns: false`,
   full-history Effective Primary mirror only after `thread/goal/get` on the
   Source Primary returns null. Confirm the fork request does not carry or
   continue a goal, the complete Source turn-ID sequence matches, the mirror is
   idle, and no goal query is sent to the ephemeral mirror. Confirm the mirror
   receives every Primary action and is not treated as a third source or
   reviewer. Reviewer routing must remain unchanged, and both selected source
   task IDs, refs, SHAs, and worktrees must stay frozen.
   Before every Primary `turn/start`, confirm the coordinator resumes the
   Effective Primary and consumes all `mcpServerStatus/list` pages until it
   finds exactly `consensus_apply_patch`.
7. While the coordinator daemon remains alive, restart the managed App Server
   and confirm `doctor` repairs the daemon-owned proxy, reaps the old proxy
   process, and permits an idempotent task read without manual coordinator
   restart. Then restart the coordinator daemon during a second disposable
   integration turn and confirm the persisted run resumes without a duplicate
   integration action. Repeat immediately after a verification command reaches
   COMPLETED and confirm its exact journaled result is reused without rerunning
   the command. Separately stop the coordinator after STARTED but before a
   completion can be proven; confirm the Run fails closed with
   `VERIFICATION_EXECUTION_UNCERTAIN` and never automatically reruns that
   command. In both cases, exact-SHA/no-remote isolation remains unchanged.
   Also pause one disposable Primary integration turn at the exact internal
   `consensus_apply_patch` approval boundary, then enable the required per-tool
   setting and resume the same Run. Confirm the daemon interrupts only the
   canonical `waitingOnApproval` turn, revalidates the clean target and frozen
   refs, and retries the same request without recreating the branch or merge.
   Also exercise the hot-reload race in which App Server continues that old
   approval before the paused Run is reactivated. Confirm the canonical patch
   item completes as `failed` with `PATCH_NOT_AUTHORIZED`, no patch record or
   Git write exists, and explicit resume archives only that exact turn before
   retrying the same Run on the existing merge. Repeat with App Server leaving
   the failed item and exact final blocker in an `inProgress` turn with
   `waitingOnApproval`; confirm resume interrupts and archives only that stale
   turn. Repeat once more with the failed item canonical but no final assistant
   JSON; confirm the clean integration SHA is identical before and after the
   single-turn interruption and the same Run retries on that existing merge.
   Repeat with a machine-valid `PATCH_NOT_AUTHORIZED` blocker that omits only
   `payload.role` and free-form `blocking_condition`; confirm it is retryable,
   while omitting any request, plan, source, target, or result-SHA identity
   still fails closed.
   Finally, complete one controlled patch and integration commit, then return a
   malformed legacy final response. Resume the same Run and confirm the daemon
   matches the stored successful patch hash and authoritative Git result, asks
   for only one read-only `INTEGRATION_READY` marker response, and never applies
   another patch or repeats branch creation or merge.
   Also return one exact, completed, side-effect-free
   `BLOCKED:CARGO_UNAVAILABLE` verification result, repair Cargo, and resume the
   same Run. Confirm the unchanged verification request is retried once, and a
   second environment retry is rejected.
   Keep one disposable participant turn active for longer than five
   minutes while canonical turn items continue to change, and confirm the Run
   does not pause; unchanged state must still hit the bounded idle timeout.
   Separately remove an idle ephemeral mirror between completed actions and
   confirm it is recreated from the Source Primary's complete history. Repeat
   with a pending Primary request that has no effective task ID, turn ID, or
   turn-start intent and confirm the binding and request rotate atomically to
   the replacement mirror. Repeat while the persisted Source Primary reports
   `notLoaded`; confirm it is resumed with participant configuration before the
   replacement fork and no ephemeral task receives `thread/resume`. Recreate
   the exact 0.2.12 `BLOCKED / HISTORY_UNAVAILABLE` boundary and confirm one
   explicit resume keeps the same Run and request before completing. Then add
   start intent or a sent identity and confirm recovery is rejected without a
   refork or resend. Complete an integration turn containing the exact
   `/bin/bash -lc 'git symbolic-ref --short HEAD'` query, recreate the 0.2.13
   `BLOCKED / FORBIDDEN_OPERATION` diagnostic for it, and confirm one explicit
   resume archives only the confirmation while preserving the same Run,
   branch, commit, patch provenance, and source refs. Confirm no second patch,
   merge, staging, or commit occurs. Repeat with a two-argument
   `git symbolic-ref` write and confirm recovery is rejected. Repeat the
   production layout with the successful patch on an archived ephemeral
   Primary attempt and a separate current confirmation-only attempt. Confirm
   matching 0.2.15 artifacts accept only the patch-free, read-only
   confirmation, preserve exactly one patch record, and resume the same Run.
   Force the first post-patch App Server proxy refresh to fail, confirm the Run
   pauses with `COMMUNICATION_FAILURE`, repair connectivity, and explicitly
   resume the same Run. It must issue only the final read-only confirmation and
   must not apply a second patch or create a replacement Run.
   Repeat after plugin hot reload makes the exact ephemeral task return
   `-32600 / thread not loaded: <id>`. Matching 0.3.8 artifacts must rotate the
   binding only after the pending request is reset to unsent with unchanged
   frozen Source history, then complete through a confirmation-only turn.
   Repeat both the interrupted-denial and completed-confirmation cases with
   exact `/bin/bash -lc 'git branch --show-current'`; matching 0.3.1 artifacts
   must resume the same Run without repeating patch, merge, staging, or commit.
   Confirm direct `git branch --show-current` also passes live policy while an
   argument-bearing variant remains denied. Submit the same Reviewer issue
   before and after a materially revised plan and confirm the second verdict
   starts a new fingerprint streak rather than producing `NO_PROGRESS`.
   Add an MCP call, file change, dynamic tool, failed or uncertain command, or
   command after the final response and confirm each variant remains
   terminal. Interrupt one
   `thread/fork` response after dispatch and
   confirm the non-idempotent request is not automatically repeated.
8. Verify `accepted_result` records the authoritative tests,
   `source_refs_unchanged: true`, and local-only/no-push/no-PR fields. Verify the
   test cwd is a cleanly materialized clone of the exact integration SHA with a
   distinct Git common directory, detached HEAD, and no remote. Verify with
   read-only Git commands that both original refs and SHAs are unchanged, both
   frozen commits are ancestors of the accepted SHA, and no remote ref or
   existing branch changed.
9. Run `codex-consensus cancel RUN_ID` on a third disposable run and confirm
   cancellation preserves existing Git state.

## 0.3.15 qualification evidence

Account identifiers, hostnames, absolute user paths, task IDs, and the local
Run ID are intentionally redacted. Exact source and result SHAs are retained so
the Git result can be audited independently.

| Field | Evidence |
| --- | --- |
| Date (UTC) | `2026-07-27` |
| Tester | Project maintainer; account and host identity redacted |
| OS / architecture | Linux `5.15.0-78-generic`, `x86_64`; Git `2.39.5` |
| Codex CLI / App Server | `0.145.0` / `0.145.0` |
| Project commit | `2d51324d3a23acb340f6bb1ab6927308816bd10d` |
| Run IDs | One accepted qualification Run; opaque local ID retained in private coordinator state |
| Frozen primary/ref SHA | `master` at `3ad09cfb930a18c0c5b866a9ee0289e471984a66` |
| Frozen reviewer/ref SHA | `codex/feature-expansion` at `e9d2475a4b6f73c200f8a3610cc2c8c465efb119` |
| Accepted branch/SHA | `consensus/<redacted-run-id>` at `cb36d10fb3ff0fc3c732aa4d14b4e48af16d8c1b` |
| Source refs unchanged | Coordinator acceptance recorded `true`; local-only result, no push, PR, or merge into an existing branch |
| Required tests | Exit 0 for `cargo test`, `cargo test --all-features`, `cargo build --all-features`, locked release build, strict all-target/all-feature Clippy, and `cargo fmt -- --check` |
| Verification clone / coordinator command evidence | Six durable command records targeted one detached exact-SHA clone with a distinct Git common directory and no remote |
| Context-sensitive review | Reviewer rejected the first tested result because `log(sin,2)` lost its deterministic missing-parentheses error and required variable-state, identifier-precedence, preview-purity, and reserved-name regressions before approval |
| Unattended dangerFullAccess turns / no user approval prompts | Primary and Reviewer turns completed unattended; no participant command or controlled patch required user approval |
| App Server proxy reconnection | Doctor and the coordinator recovered fresh protocol connections while retaining the same Run and durable state |
| Controlled-patch approval configuration/recovery | Exact scoped approval was effective; completed writes were not replayed during same-Run recovery |
| Restart recovery | Coordinator upgrades/restarts preserved the same Run, branch, journal, and completed verification evidence without a replacement Run |
| Cancellation preservation | Separate disposable Runs reached `CANCELLED` without an integration SHA or source-ref movement |

## Promotion rule

A stable release requires reviewed evidence for the release's supported Codex
adapter and no unresolved safety discrepancy. The 0.3.15 record above completes
that gate. The workflow change from pre-release to stable must remain a
separate reviewed commit after this evidence commit, and any later material
adapter or safety-boundary change requires a new real-Codex qualification or an
explicit pre-release.
