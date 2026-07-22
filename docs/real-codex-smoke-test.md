# Real Codex Smoke-Test Record

## Release gate

No disposable real-Codex run has yet been recorded for the supported Codex CLI
range beginning at `0.144.1`. Automated tests use a process-level fake App
Server and do not satisfy this gate. Therefore release automation creates a
GitHub **pre-release** until a maintainer completes this checklist and commits
the evidence below.

## Required environment

- A supported Linux x86_64 or ARM64 host.
- Exact `codex --version` output satisfying `>=0.144.1`.
- One disposable local Git repository with two committed branches checked out
  in two distinct worktrees.
- Two existing Codex tasks under the same local Codex account. Their reported
  cwd may be identical or outside Git because source worktrees are bound
  explicitly.
- No remote is required and no push is permitted.

## Procedure

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
     --repository /gpfs/users/i-zhangguoqiang/workspace/gh_testtest \
     --json
   codex-consensus run \
     --primary-thread PRIMARY_TASK_ID \
     --primary-worktree /gpfs/users/i-zhangguoqiang/workspace/gh_testtest \
     --reviewer-thread REVIEWER_TASK_ID \
     --reviewer-worktree /gpfs/users/i-zhangguoqiang/workspace/gh_testtest/.worktrees/feature-expansion \
     --integration-branch consensus/real-smoke \
     --test "cargo test --workspace" \
     --json
   codex-consensus status RUN_ID --json
   ```

6. Confirm both task summaries may report the same repository-root cwd while
   every turn executes at its separately frozen worktree. Observe at least one
   plan correction from the reviewer, exact plan approval,
   primary-only source Git writes, and exact result-SHA approval. Confirm review
   turns report read-only/offline policy and the integration turn reports
   bounded source-workspace-write/offline policy. Confirm a separate
   verification turn uses only the persisted `verification_worktree`, runs each
   frozen command exactly once, and records completed `commandExecution`
   evidence with turn ID, item ID, command, cwd, exit code, and bounded failure
   diagnostics. Exercise one nonzero command and confirm the same Run returns
   to a controlled integration correction, verifies the new SHA, and proceeds
   to result review only after every frozen command passes.
   On an App Server whose completed `thread/read` history omits command or MCP
   items, confirm the daemon has persisted matching `item/started`,
   `item/completed`, and `turn/completed` evidence in private SQLite and still
   derives the same exact command, cwd, exit code, and bounded output. Restart
   the daemon after the completion barrier and confirm it reuses that evidence
   without asking either task to serialize it in JSON.
   Confirm contracts use one result marker plus a JSON body, while the plan,
   review feedback, integration summary, verification summary, and final review
   use one result marker plus ordinary Markdown. No participant should have to
   repeat run IDs, plan hashes, branches, SHAs, changed files, or test evidence.
7. While the coordinator daemon remains alive, restart the managed App Server
   and confirm `doctor` repairs the daemon-owned proxy, reaps the old proxy
   process, and permits an idempotent task read without manual coordinator
   restart. Then restart the coordinator daemon during a second disposable
   integration turn and confirm the persisted run resumes without a duplicate
   integration action. Repeat during a verification command that creates a
   disposable test artifact; confirm the same verification turn is recovered
   without rerunning the command and without relaxing exact-SHA/no-remote
   isolation.
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
8. Verify `accepted_result` records the authoritative tests,
   `source_refs_unchanged: true`, and local-only/no-push/no-PR fields. Verify the
   test cwd is a cleanly materialized clone of the exact integration SHA with a
   distinct Git common directory, detached HEAD, and no remote. Verify with
   read-only Git commands that both original refs and SHAs are unchanged, both
   frozen commits are ancestors of the accepted SHA, and no remote ref or
   existing branch changed.
9. Run `codex-consensus cancel RUN_ID` on a third disposable run and confirm
   cancellation preserves existing Git state.

## Evidence template

Replace `NOT_RECORDED` only with reproducible, redacted evidence.

| Field | Evidence |
| --- | --- |
| Date (UTC) | `NOT_RECORDED` |
| Tester | `NOT_RECORDED` |
| OS / architecture | `NOT_RECORDED` |
| Codex CLI | `NOT_RECORDED` |
| Project commit | `NOT_RECORDED` |
| Run IDs | `NOT_RECORDED` |
| Frozen primary/ref SHA | `NOT_RECORDED` |
| Frozen reviewer/ref SHA | `NOT_RECORDED` |
| Accepted branch/SHA | `NOT_RECORDED` |
| Source refs unchanged | `NOT_RECORDED` |
| Required tests | `NOT_RECORDED` |
| Verification clone / command-item evidence | `NOT_RECORDED` |
| App Server proxy reconnection | `NOT_RECORDED` |
| Controlled-patch approval configuration/recovery | `NOT_RECORDED` |
| Restart recovery | `NOT_RECORDED` |
| Cancellation preservation | `NOT_RECORDED` |

## Promotion rule

A stable release requires reviewed evidence for the release's supported Codex
adapter and no unresolved safety discrepancy. Changing the workflow from
pre-release to stable must be a separate reviewed commit after this record is
complete.
