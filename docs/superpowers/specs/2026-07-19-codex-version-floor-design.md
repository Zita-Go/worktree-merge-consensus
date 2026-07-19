# Codex Version Floor Design

Date: 2026-07-19

Status: approved for specification review

## Context

The v0.1 implementation currently accepts Codex CLI versions in the bounded
range `>=0.144.5, <0.145.0`. The project should instead support every
parseable Codex semantic version at or above `0.144.1`, without an upper
version limit.

## Compatibility rule

The executable must parse the exact semantic version reported by
`codex --version`. Compatibility succeeds when the parsed version is
`>=0.144.1`.

- `0.144.1` is accepted.
- Versions newer than `0.144.1`, including later minor and major versions, are
  accepted by the version gate.
- Versions below `0.144.1` are rejected with `INCOMPATIBLE_CODEX`.
- A prerelease such as `0.144.1-beta.1` is below the stable `0.144.1` under
  semantic-version ordering and is rejected.
- Malformed or ambiguous version output remains rejected.

The compatibility fixture will declare only `minimumVersion: "0.144.1"`;
the obsolete maximum-version field and bounded-range documentation will be
removed. The fixture filename will be changed from the version-specific
`0.144.5-methods.json` to `supported-methods.json`, because it now describes
the runtime capability contract for an open-ended version range.

## Runtime safety checks

Removing the upper bound changes only the preliminary CLI version gate. The
managed App Server must still report the same exact version as the executable,
and initialization must still expose the methods and response shapes required
by the checked-in adapter contract. Missing or malformed capabilities continue
to fail closed.

This does not claim that every future Codex release is semantically identical.
It intentionally prefers forward compatibility while retaining the runtime
checks that the coordinator can verify mechanically.

## Tests

Compatibility tests will cover the new lower boundary, the immediately older
patch, an unbounded future version, prerelease ordering, and malformed output.
Process tests will prove that `0.144.1` starts the managed App Server while
`0.144.0` does not. Existing handshake identity and required-method tests remain
unchanged except for fixture version values.

The full Rust workspace tests, Clippy, MSRV check, documentation checks, release
gate, and patch-format check must pass before the implementation is committed.

## Documentation and release status

The English and Chinese README files, compatibility guide, security guidance,
real-Codex smoke-test template, checked-in schema references, and documentation
self-tests will state `Codex CLI >=0.144.1` with no upper bound.

No real-Codex smoke evidence is inferred from this policy change. Until such a
run is recorded, the existing prerelease caveat remains in place.

## Non-goals

- No changes to the two-task consensus protocol or Git integration workflow.
- No push, pull request, or merge into an existing branch.
- No promise to bypass runtime App Server capability or identity checks.
