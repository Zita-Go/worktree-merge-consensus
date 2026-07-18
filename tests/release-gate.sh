#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

fail() {
  printf 'release gate self-test failed: %s\n' "$*" >&2
  exit 1
}

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
fake_binary="$temporary/codex-consensus"
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'case "${1:-}" in' \
  '  --version) printf "codex-consensus %s\\n" "${FAKE_VERSION:?}" ;;' \
  '  --help) exit 0 ;;' \
  '  *) exit 1 ;;' \
  'esac' >"$fake_binary"
chmod 0755 "$fake_binary"

if FAKE_VERSION=10.1.0 CODEX_CONSENSUS_BIN="$fake_binary" \
  bash tests/release.sh v0.1.0 >/dev/null 2>&1; then
  fail 'a substring-compatible but unequal binary version was accepted'
fi

FAKE_VERSION=0.1.0 CODEX_CONSENSUS_BIN="$fake_binary" \
  bash tests/release.sh v0.1.0 >/dev/null

printf 'release gate self-test passed\n'
