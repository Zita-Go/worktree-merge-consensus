# Changelog

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
