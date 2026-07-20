#!/bin/sh
set -eu

launch_binary() {
  exec "$1" mcp-server
}

if [ -n "${CODEX_CONSENSUS_BIN:-}" ]; then
  if [ ! -x "$CODEX_CONSENSUS_BIN" ]; then
    printf 'worktree-merge-consensus: CODEX_CONSENSUS_BIN is not executable: %s\n' \
      "$CODEX_CONSENSUS_BIN" >&2
    exit 127
  fi
  launch_binary "$CODEX_CONSENSUS_BIN"
fi

binary_path="$(command -v codex-consensus 2>/dev/null || true)"
if [ -n "$binary_path" ] && [ -x "$binary_path" ]; then
  launch_binary "$binary_path"
fi

codex_path="$(command -v codex 2>/dev/null || true)"
if [ -n "$codex_path" ]; then
  binary_path="${codex_path%/*}/codex-consensus"
  if [ -x "$binary_path" ]; then
    launch_binary "$binary_path"
  fi
fi

for binary_path in \
  /usr/local/bin/codex-consensus \
  "${HOME:-}/.local/bin/codex-consensus"
do
  if [ -x "$binary_path" ]; then
    launch_binary "$binary_path"
  fi
done

printf '%s\n' \
  'worktree-merge-consensus: codex-consensus was not found; install the matching release or set CODEX_CONSENSUS_BIN' \
  >&2
exit 127
