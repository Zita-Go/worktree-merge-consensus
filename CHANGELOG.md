# Changelog

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
