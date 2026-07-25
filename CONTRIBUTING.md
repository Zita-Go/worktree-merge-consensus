# Contributing

Thanks for helping improve Worktree Merge Consensus. Changes are welcome in
documentation, compatibility adapters, safety policy, tests, and user
experience.

## Before opening an issue

- Search existing issues and the [troubleshooting guide](docs/recovery.md).
- Run `codex-consensus doctor` and record the exact binary and Codex versions.
- Redact task IDs, usernames, hostnames, absolute home paths, repository
  secrets, prompts, and private task history.
- For a suspected vulnerability, do not open a public issue; follow
  [SECURITY.md](SECURITY.md).

## Development setup

The workspace requires Rust 1.85 or newer, Git, and a Unix environment.

```bash
git clone git@github.com:Zita-Go/worktree-merge-consensus.git
cd worktree-merge-consensus
cargo build --locked --workspace
```

The process-level end-to-end suite uses a fake App Server and disposable Git
repositories. It does not require access to a real Codex account.

## Required checks

Run these before submitting a pull request:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=1
bash tests/docs.sh
bash tests/release-gate.sh
```

CI also runs RustSec and license audits and builds static Linux binaries for
x86_64 and ARM64.

## Documentation changes

- Keep `README.md` and `README.zh-CN.md` aligned for user-visible behavior,
  installation, safety warnings, and command examples.
- Put version-by-version changes in `CHANGELOG.md`, not in the README.
- Put exact protocol, compatibility, recovery, and safety contracts in their
  corresponding `docs/` files.
- Use relative links and run `bash tests/docs.sh` to catch missing targets and
  contract drift.
- Do not present illustrative output as evidence from a real Codex Run.

## Safety-sensitive changes

Changes to command policy, Git writes, participant binding, App Server request
shape, recovery, verification, or persisted state require:

- a fail-closed design with explicit trust assumptions;
- regression tests for both accepted and rejected boundaries;
- proof that source refs, target identity, and request provenance remain bound;
- idempotency analysis for restart and uncertain-delivery cases;
- updates to `SECURITY.md`, `docs/safety-model.md`, `docs/compatibility.md`, or
  `docs/recovery.md` as applicable.

Never broaden a command allowlist to a general executable family when one exact
operation is sufficient.

## Pull requests

Keep each pull request focused. Include:

- the user-visible problem and expected outcome;
- implementation and safety tradeoffs;
- tests run and their results;
- documentation changes;
- any real-Codex evidence, clearly separated from fake-App-Server evidence and
  fully redacted.

By contributing, you agree that your contribution is licensed under the
project's [Apache License 2.0](LICENSE).
