#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

fail() {
  printf 'plugin bootstrap test failed: %s\n' "$*" >&2
  exit 1
}

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT

plugin_version="$(python3 -c 'import json; print(json.load(open("plugin/.codex-plugin/plugin.json", encoding="utf-8"))["version"])')"
release_version="${plugin_version%%+*}"
target="x86_64-unknown-linux-musl"
asset="codex-consensus-v${release_version}-${target}.tar.gz"
release_dir="$temporary/release"
package_dir="$temporary/package/codex-consensus-v${release_version}-${target}"
fake_bin="$temporary/bin"
plugin_data="$temporary/plugin-data"
mkdir -p "$release_dir" "$package_dir" "$fake_bin" "$plugin_data"

cat >"$package_dir/codex-consensus" <<EOF
#!/bin/sh
case "\${1:-}" in
  --version) printf '%s\\n' 'codex-consensus $release_version' ;;
  mcp-server) printf '%s\\n' 'mcp-server' ;;
  *) exit 64 ;;
esac
EOF
chmod 0755 "$package_dir/codex-consensus"
tar -C "$temporary/package" -czf "$release_dir/$asset" \
  "codex-consensus-v${release_version}-${target}"

if command -v sha256sum >/dev/null 2>&1; then
  digest="$(sha256sum "$release_dir/$asset" | awk '{print $1}')"
else
  digest="$(shasum -a 256 "$release_dir/$asset" | awk '{print $1}')"
fi
printf '%s  %s\n' "$digest" "$asset" >"$release_dir/SHA256SUMS"

cat >"$fake_bin/uname" <<'EOF'
#!/bin/sh
case "${1:-}" in
  -s) printf '%s\n' Linux ;;
  -m) printf '%s\n' x86_64 ;;
  *) exit 64 ;;
esac
EOF

cat >"$fake_bin/curl" <<'EOF'
#!/bin/sh
destination=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output)
      destination="$2"
      shift 2
      ;;
    http://* | https://*)
      url="$1"
      shift
      ;;
    *)
      shift
      ;;
  esac
done
[ -n "$destination" ] && [ -n "$url" ] || exit 64
cp "$FAKE_RELEASE_DIR/${url##*/}" "$destination"
EOF
chmod 0755 "$fake_bin/uname" "$fake_bin/curl"

test_path="$fake_bin:/usr/bin:/bin"
output="$(
  PATH="$test_path" \
  HOME="$temporary/home" \
  PLUGIN_DATA="$plugin_data" \
  FAKE_RELEASE_DIR="$release_dir" \
  CODEX_CONSENSUS_RELEASE_BASE_URL="https://release.invalid/v${release_version}" \
    /bin/sh plugin/scripts/start-mcp.sh
)"
[[ "$output" == "mcp-server" ]] || fail "first launch did not execute the downloaded runtime"

managed_binary="$plugin_data/runtime/v${release_version}/${target}/codex-consensus"
[[ -x "$managed_binary" ]] || fail "verified runtime was not cached under PLUGIN_DATA"
[[ "$($managed_binary --version)" == "codex-consensus $release_version" ]] ||
  fail "cached runtime has the wrong version"

cat >"$fake_bin/curl" <<'EOF'
#!/bin/sh
exit 99
EOF
chmod 0755 "$fake_bin/curl"
cached_output="$(
  PATH="$test_path" \
  HOME="$temporary/home" \
  PLUGIN_DATA="$plugin_data" \
  FAKE_RELEASE_DIR="$temporary/missing" \
  CODEX_CONSENSUS_RELEASE_BASE_URL="https://release.invalid/v${release_version}" \
    /bin/sh plugin/scripts/start-mcp.sh
)"
[[ "$cached_output" == "mcp-server" ]] || fail "cached launch attempted another download"

override="$temporary/override"
cat >"$override" <<EOF
#!/bin/sh
case "\${1:-}" in
  --version) printf '%s\\n' 'codex-consensus $release_version' ;;
  mcp-server) printf '%s\\n' 'override-mcp-server' ;;
  *) exit 64 ;;
esac
EOF
chmod 0755 "$override"
override_output="$(CODEX_CONSENSUS_BIN="$override" /bin/sh plugin/scripts/start-mcp.sh)"
[[ "$override_output" == "override-mcp-server" ]] || fail "explicit exact-version override was not honored"

wrong_override="$temporary/wrong-override"
cat >"$wrong_override" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then
  printf '%s\n' 'codex-consensus 99.0.0'
fi
EOF
chmod 0755 "$wrong_override"
if CODEX_CONSENSUS_BIN="$wrong_override" /bin/sh plugin/scripts/start-mcp.sh \
  >"$temporary/wrong.stdout" 2>"$temporary/wrong.stderr"; then
  fail "wrong-version explicit override was accepted"
fi
grep -Fq 'version mismatch' "$temporary/wrong.stderr" ||
  fail "wrong-version override did not report a precise diagnostic"

corrupt_data="$temporary/corrupt-data"
corrupt_release="$temporary/corrupt-release"
mkdir -p "$corrupt_data" "$corrupt_release"
cp "$release_dir/$asset" "$corrupt_release/$asset"
printf '%064d  %s\n' 0 "$asset" >"$corrupt_release/SHA256SUMS"
cat >"$fake_bin/curl" <<'EOF'
#!/bin/sh
destination=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) destination="$2"; shift 2 ;;
    http://* | https://*) url="$1"; shift ;;
    *) shift ;;
  esac
done
cp "$FAKE_RELEASE_DIR/${url##*/}" "$destination"
EOF
chmod 0755 "$fake_bin/curl"
if PATH="$test_path" \
  HOME="$temporary/home" \
  PLUGIN_DATA="$corrupt_data" \
  FAKE_RELEASE_DIR="$corrupt_release" \
  CODEX_CONSENSUS_RELEASE_BASE_URL="https://release.invalid/v${release_version}" \
    /bin/sh plugin/scripts/start-mcp.sh \
    >"$temporary/corrupt.stdout" 2>"$temporary/corrupt.stderr"; then
  fail "corrupt runtime archive was accepted"
fi
[[ ! -e "$corrupt_data/runtime/v${release_version}/${target}/codex-consensus" ]] ||
  fail "corrupt runtime was left in the managed cache"
grep -Fq 'checksum mismatch' "$temporary/corrupt.stderr" ||
  fail "corrupt runtime did not report a checksum mismatch"

printf 'plugin bootstrap checks passed for %s\n' "$release_version"
