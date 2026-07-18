#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

fail() {
  printf 'release check failed: %s\n' "$*" >&2
  exit 1
}

tag="${1:-}"
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] ||
  fail "release tag must be semantic: ${tag:-<missing>}"
release_version="${tag#v}"

workspace_version="$({
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
[[ -n "$workspace_version" ]] || fail 'workspace package version is missing'
[[ "$workspace_version" == "$release_version" ]] ||
  fail "tag $release_version does not match workspace version $workspace_version"

plugin_version="$(python3 -c 'import json; print(json.load(open("plugin/.codex-plugin/plugin.json", encoding="utf-8"))["version"])')"
[[ "$plugin_version" == "$release_version" ]] ||
  fail "tag $release_version does not match plugin version $plugin_version"

while IFS= read -r manifest; do
  grep -Fq 'version.workspace = true' "$manifest" ||
    fail "$manifest does not inherit the release workspace version"
done < <(find crates tests -mindepth 2 -maxdepth 2 -name Cargo.toml -print | sort)

if [[ -n "${CODEX_CONSENSUS_BIN:-}" ]]; then
  [[ -x "$CODEX_CONSENSUS_BIN" ]] || fail "binary is not executable: $CODEX_CONSENSUS_BIN"
  version_output="$("$CODEX_CONSENSUS_BIN" --version)"
  [[ "$version_output" == "codex-consensus $release_version" ]] ||
    fail 'binary version does not match the release tag'
  "$CODEX_CONSENSUS_BIN" --help >/dev/null || fail 'binary --help smoke test failed'
fi

printf 'release version checks passed for %s\n' "$tag"
