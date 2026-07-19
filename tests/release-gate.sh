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

current_version="$({
  awk '
    /^\[workspace\.package\]$/ { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && /^version[[:space:]]*=/ {
      value = $0
      sub(/^[^=]*=[[:space:]]*"/, "", value)
      sub(/"[[:space:]]*$/, "", value)
      print value
      exit
    }
  ' Cargo.toml
})"
[[ -n "$current_version" ]] || fail 'workspace package version is missing'

if FAKE_VERSION="1${current_version}" CODEX_CONSENSUS_BIN="$fake_binary" \
  bash tests/release.sh "v${current_version}" >/dev/null 2>&1; then
  fail 'a substring-compatible but unequal binary version was accepted'
fi

FAKE_VERSION="$current_version" CODEX_CONSENSUS_BIN="$fake_binary" \
  bash tests/release.sh "v${current_version}" >/dev/null

printf 'release gate self-test passed\n'
