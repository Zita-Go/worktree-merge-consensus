#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

fail() {
  printf 'docs check failed: %s\n' "$*" >&2
  exit 1
}

required_files=(
  README.md
  README.zh-CN.md
  SECURITY.md
  LICENSE
  docs/protocol-v1.md
  docs/compatibility.md
  docs/real-codex-smoke-test.md
  .github/workflows/ci.yml
  .github/workflows/release.yml
  tests/release.sh
  tests/release-gate.sh
)

for path in "${required_files[@]}"; do
  [[ -f "$path" ]] || fail "missing required file: $path"
done

commands=(doctor threads run status resume cancel)
for readme in README.md README.zh-CN.md; do
  for command in "${commands[@]}"; do
    grep -Fq "codex-consensus $command" "$readme" ||
      fail "$readme does not document codex-consensus $command"
  done

  for marker in same-host 0.144.5 no-push SHA256SUMS; do
    grep -Fq "$marker" "$readme" || fail "$readme is missing the $marker contract"
  done

  grep -Fq 'codex plugin marketplace add' "$readme" ||
    fail "$readme is missing marketplace registration"
  grep -Fq 'codex plugin add' "$readme" || fail "$readme is missing plugin installation"
done

help_bin="${CODEX_CONSENSUS_BIN:-target/debug/codex-consensus}"
if [[ ! -x "$help_bin" ]]; then
  cargo build --locked -p codex-consensus
fi
help_text="$($help_bin --help)"
for command in "${commands[@]}"; do
  grep -Eq "^[[:space:]]+$command([[:space:]]|$)" <<<"$help_text" ||
    fail "documented command is absent from --help: $command"
done

grep -Fq 'cargo fmt --all --check' .github/workflows/ci.yml || fail 'CI omits rustfmt'
grep -Fq 'cargo +1.85.0 check --locked --workspace --all-targets' .github/workflows/ci.yml ||
  fail 'CI omits the declared Rust MSRV check'
grep -Fq 'cargo clippy --workspace --all-targets -- -D warnings' .github/workflows/ci.yml ||
  fail 'CI omits warning-denied Clippy'
grep -Fq 'cargo test --workspace' .github/workflows/ci.yml || fail 'CI omits workspace tests'
grep -Fq 'bash tests/docs.sh' .github/workflows/ci.yml || fail 'CI omits docs checks'
grep -Fq 'cargo audit' .github/workflows/ci.yml || fail 'CI omits cargo audit'
grep -Fq 'cargo deny check licenses' .github/workflows/ci.yml || fail 'CI omits license checks'

for marker in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu SHA256SUMS cyclonedx-json plugin; do
  grep -Fq "$marker" .github/workflows/release.yml ||
    fail "release workflow is missing $marker"
done

grep -Fq 'qualify:' .github/workflows/release.yml || fail 'release omits qualification job'
grep -Fq 'needs: qualify' .github/workflows/release.yml || fail 'release builds bypass qualification'
grep -Fq 'bash tests/release.sh "$GITHUB_REF_NAME"' .github/workflows/release.yml ||
  fail 'release does not validate tag and package versions'
grep -Fq 'bash tests/release-gate.sh' .github/workflows/release.yml ||
  fail 'release does not regression-test exact binary version matching'
grep -Fq 'cargo test --workspace' .github/workflows/release.yml || fail 'release omits workspace tests'
grep -Fq 'cargo +1.85.0 check --locked --workspace --all-targets' .github/workflows/release.yml ||
  fail 'release omits the declared Rust MSRV check'
grep -Fq 'cargo clippy --workspace --all-targets -- -D warnings' .github/workflows/release.yml ||
  fail 'release omits warning-denied Clippy'
grep -Fq 'cargo audit' .github/workflows/release.yml || fail 'release omits cargo audit'
grep -Fq 'cargo deny check licenses' .github/workflows/release.yml ||
  fail 'release omits license checks'
grep -Fq 'codex-consensus --version' .github/workflows/release.yml ||
  fail 'release does not smoke-test packaged binaries'

python3 - <<'PY'
from pathlib import Path
import re
import sys

root = Path.cwd()
markdown_files = sorted(root.rglob("*.md"))
link_pattern = re.compile(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+[^)]*)?\)")
errors = []

for document in markdown_files:
    text = document.read_text(encoding="utf-8")
    for target in link_pattern.findall(text):
        if target.startswith("https://"):
            continue
        if target.startswith("#"):
            continue
        if target.startswith(("/", "//")) or "://" in target or target.startswith("mailto:"):
            errors.append(f"{document.relative_to(root)}: disallowed link target {target}")
            continue
        local_target = target.split("#", 1)[0].split("?", 1)[0]
        if local_target and not (document.parent / local_target).exists():
            errors.append(f"{document.relative_to(root)}: missing relative link target {target}")

if errors:
    print("\n".join(errors), file=sys.stderr)
    raise SystemExit(1)
PY

printf 'documentation checks passed\n'
