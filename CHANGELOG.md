# Changelog

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
