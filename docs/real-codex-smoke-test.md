# Real Codex Smoke-Test Record

## Release gate

No disposable real-Codex run has yet been recorded for Codex CLI `0.144.5`.
Automated tests use a process-level fake App Server and do not satisfy this
gate. Therefore v0.1 release automation creates a GitHub **pre-release** until a
maintainer completes this checklist and commits the evidence below.

## Required environment

- A supported Linux x86_64 or ARM64 host.
- Exact `codex --version` output within `>=0.144.5, <0.145.0`.
- One disposable local Git repository with two committed branches checked out
  in two distinct worktrees.
- Two existing Codex tasks, each attached to one of those worktrees under the
  same local Codex account.
- No remote is required and no push is permitted.

## Procedure

1. Record `codex --version`, `git --version`, OS, architecture, and the
   `codex-consensus` release SHA.
2. Record both task IDs, worktree paths, source refs, and source SHAs. Use only
   disposable code and redact account identifiers.
3. Give the two branches a small overlapping change whose correct integration
   requires preserving a reviewer-only behavior and resolving one conflict.
4. Confirm both source worktrees are clean and the intended integration branch
   does not exist.
5. Run:

   ```bash
   codex-consensus doctor --json
   codex-consensus run \
     --primary-thread PRIMARY_TASK_ID \
     --reviewer-thread REVIEWER_TASK_ID \
     --integration-branch consensus/real-smoke \
     --test "cargo test --workspace" \
     --json
   codex-consensus status RUN_ID --json
   ```

6. Observe at least one plan correction from the reviewer, exact plan approval,
   primary-only source Git writes, and exact result-SHA approval. Confirm review
   turns report read-only/offline policy and the integration turn reports
   bounded source-workspace-write/offline policy. Confirm a separate
   verification turn uses only the persisted `verification_worktree`, runs each
   frozen command exactly once, and records successful `commandExecution`
   evidence with turn ID, item ID, command, cwd, and exit code.
7. Restart the coordinator daemon during a second disposable integration turn
   and confirm the persisted run resumes without a duplicate integration
   action. Repeat during a verification command that creates a disposable test
   artifact; confirm the same verification turn is recovered without rerunning
   the command and without relaxing exact-SHA/no-remote isolation.
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
| Restart recovery | `NOT_RECORDED` |
| Cancellation preservation | `NOT_RECORDED` |

## Promotion rule

A stable release requires reviewed evidence for the release's supported Codex
adapter and no unresolved safety discrepancy. Changing the workflow from
pre-release to stable must be a separate reviewed commit after this record is
complete.
