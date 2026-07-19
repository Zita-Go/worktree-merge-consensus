#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'static linkage check failed: %s\n' "$*" >&2
  exit 1
}

binary="${1:-}"
[[ -n "$binary" ]] || fail 'binary path is required'
[[ -x "$binary" ]] || fail "binary is not executable: $binary"
command -v readelf >/dev/null || fail 'readelf is required'

dynamic_section="$(readelf -d "$binary")"
if grep -Fq '(NEEDED)' <<<"$dynamic_section"; then
  fail 'binary has a dynamic library dependency'
fi

program_headers="$(readelf -l "$binary")"
if grep -Fq 'Requesting program interpreter' <<<"$program_headers"; then
  fail 'binary requires a dynamic program interpreter'
fi

printf 'static linkage checks passed for %s\n' "$binary"
