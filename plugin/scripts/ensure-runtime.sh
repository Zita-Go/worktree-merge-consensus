#!/bin/sh
set -eu

plugin_name="worktree-merge-consensus"
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
plugin_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"
manifest="$plugin_root/.codex-plugin/plugin.json"

fail() {
  printf '%s: %s\n' "$plugin_name" "$*" >&2
  exit 1
}

plugin_version="$({
  sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest"
} | head -n 1)"
[ -n "$plugin_version" ] || fail "could not read the plugin version from $manifest"

# Codex cachebusters use SemVer build metadata. Release assets and the native
# binary retain the base package version.
release_version="${plugin_version%%+*}"
expected_version="codex-consensus $release_version"

binary_version() {
  "$1" --version 2>/dev/null || true
}

compatible_binary() {
  [ -x "$1" ] && [ "$(binary_version "$1")" = "$expected_version" ]
}

emit_binary() {
  printf '%s\n' "$1"
  exit 0
}

if [ -n "${CODEX_CONSENSUS_BIN:-}" ]; then
  [ -x "$CODEX_CONSENSUS_BIN" ] ||
    fail "CODEX_CONSENSUS_BIN is not executable: $CODEX_CONSENSUS_BIN"
  actual_version="$(binary_version "$CODEX_CONSENSUS_BIN")"
  [ "$actual_version" = "$expected_version" ] ||
    fail "CODEX_CONSENSUS_BIN version mismatch: expected '$expected_version', got '${actual_version:-<unavailable>}'"
  emit_binary "$CODEX_CONSENSUS_BIN"
fi

os_name="$(uname -s 2>/dev/null || true)"
machine="$(uname -m 2>/dev/null || true)"
if [ "$os_name" != "Linux" ]; then
  path_binary="$(command -v codex-consensus 2>/dev/null || true)"
  if [ -n "$path_binary" ] && compatible_binary "$path_binary"; then
    emit_binary "$path_binary"
  fi

  codex_path="$(command -v codex 2>/dev/null || true)"
  if [ -n "$codex_path" ]; then
    adjacent_binary="${codex_path%/*}/codex-consensus"
    if compatible_binary "$adjacent_binary"; then
      emit_binary "$adjacent_binary"
    fi
  fi

  for system_binary in /usr/local/bin/codex-consensus "${HOME:-}/.local/bin/codex-consensus"; do
    if compatible_binary "$system_binary"; then
      emit_binary "$system_binary"
    fi
  done

  fail "automatic runtime installation supports Linux only; install '$expected_version' and set CODEX_CONSENSUS_BIN"
fi

case "$machine" in
  x86_64 | amd64)
    target="x86_64-unknown-linux-musl"
    ;;
  aarch64 | arm64)
    target="aarch64-unknown-linux-musl"
    ;;
  *)
    fail "unsupported Linux architecture '$machine'; supported architectures are x86_64 and aarch64"
    ;;
esac

if [ -n "${PLUGIN_DATA:-}" ]; then
  data_root="$PLUGIN_DATA"
elif [ -n "${XDG_DATA_HOME:-}" ]; then
  data_root="$XDG_DATA_HOME/$plugin_name"
elif [ -n "${HOME:-}" ]; then
  data_root="$HOME/.local/share/$plugin_name"
else
  fail "PLUGIN_DATA, XDG_DATA_HOME, and HOME are all unavailable"
fi

runtime_dir="$data_root/runtime/v$release_version/$target"
managed_binary="$runtime_dir/codex-consensus"
umask 077
mkdir -p "$runtime_dir"
for private_dir in \
  "$data_root" \
  "$data_root/runtime" \
  "$data_root/runtime/v$release_version" \
  "$runtime_dir"
do
  chmod 0700 "$private_dir" || fail "could not make runtime cache private: $private_dir"
done

if compatible_binary "$managed_binary"; then
  emit_binary "$managed_binary"
fi

if ! command -v tar >/dev/null 2>&1; then
  fail "tar is required for automatic runtime installation"
fi

download() {
  url="$1"
  destination="$2"
  if command -v curl >/dev/null 2>&1; then
    curl --disable --proto '=https' --tlsv1.2 \
      --fail --location --silent --show-error --retry 3 \
      --connect-timeout 15 --output "$destination" "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget --https-only --quiet --output-document="$destination" "$url"
  else
    fail "curl or wget is required for automatic runtime installation"
  fi
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    fail "sha256sum or shasum is required for runtime verification"
  fi
}

temporary="$(mktemp -d "$runtime_dir/.download.XXXXXX")" ||
  fail "could not create a temporary runtime directory"
cleanup() {
  rm -rf "$temporary"
}
trap cleanup EXIT HUP INT TERM

asset="codex-consensus-v${release_version}-${target}.tar.gz"
base_url="${CODEX_CONSENSUS_RELEASE_BASE_URL:-https://github.com/Zita-Go/worktree-merge-consensus/releases/download/v${release_version}}"
base_url="${base_url%/}"
case "$base_url" in
  https://*) ;;
  *) fail "release base URL must use HTTPS: $base_url" ;;
esac
checksums="$temporary/SHA256SUMS"
archive="$temporary/$asset"

printf '%s: installing verified runtime %s for %s\n' \
  "$plugin_name" "$release_version" "$target" >&2
download "$base_url/SHA256SUMS" "$checksums" ||
  fail "could not download $base_url/SHA256SUMS"
download "$base_url/$asset" "$archive" ||
  fail "could not download $base_url/$asset"

expected_sha="$({
  awk -v asset="$asset" '$2 == asset || $2 == "*" asset { print $1; exit }' "$checksums"
})"
[ "${#expected_sha}" -eq 64 ] || fail "SHA256SUMS has no valid entry for $asset"
case "$expected_sha" in
  *[!0-9A-Fa-f]*) fail "SHA256SUMS contains an invalid digest for $asset" ;;
esac

actual_sha="$(sha256_file "$archive")"
[ "$actual_sha" = "$expected_sha" ] ||
  fail "checksum mismatch for $asset (expected $expected_sha, got $actual_sha)"

tar -xzf "$archive" -C "$temporary"
extracted="$temporary/codex-consensus-v${release_version}-${target}/codex-consensus"
[ -f "$extracted" ] || fail "$asset does not contain the expected codex-consensus binary"
chmod 0755 "$extracted"
actual_version="$(binary_version "$extracted")"
[ "$actual_version" = "$expected_version" ] ||
  fail "downloaded runtime version mismatch: expected '$expected_version', got '${actual_version:-<unavailable>}'"

staged="$runtime_dir/.codex-consensus.$$"
install -m 0755 "$extracted" "$staged"
mv -f "$staged" "$managed_binary"
compatible_binary "$managed_binary" || fail "installed runtime failed its version check"

emit_binary "$managed_binary"
