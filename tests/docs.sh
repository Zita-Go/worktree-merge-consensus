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
  docs/protocol-v2.md
  docs/compatibility.md
  docs/real-codex-smoke-test.md
  schemas/app-server/supported-methods.json
  .github/workflows/ci.yml
  .github/workflows/release.yml
  tests/release.sh
  tests/release-gate.sh
  tests/static-link.sh
)

for path in "${required_files[@]}"; do
  [[ -f "$path" ]] || fail "missing required file: $path"
done

commands=(configure doctor threads worktrees run status resume cancel)
for readme in README.md README.zh-CN.md; do
  for command in "${commands[@]}"; do
    grep -Fq "codex-consensus $command" "$readme" ||
      fail "$readme does not document codex-consensus $command"
  done

  for marker in same-host '>=0.144.1' no-push SHA256SUMS unknown-linux-musl dangerFullAccess command/exec; do
    grep -Fq "$marker" "$readme" || fail "$readme is missing the $marker contract"
  done

  for marker in 'worktree-merge-consensus/v2' '<consensus-result>' 'protocol-v2.md'; do
    grep -Fq "$marker" "$readme" || fail "$readme is missing the $marker protocol contract"
  done

  for marker in \
    consensus_list_worktrees \
    consensus_apply_patch \
    --primary-worktree \
    --reviewer-worktree \
    LEGACY_SKILL_CONFLICT \
    APPROVAL_CONFIGURATION_REQUIRED \
    'consensus_apply_patch.approval_mode' \
    'task cwd' \
    'binary/plugin'; do
    grep -Fq -- "$marker" "$readme" || fail "$readme is missing the $marker contract"
  done

  grep -Fq 'codex plugin marketplace add' "$readme" ||
    fail "$readme is missing marketplace registration"
  grep -Fq 'codex plugin add' "$readme" || fail "$readme is missing plugin installation"
done

for marker in dangerFullAccess command/exec VERIFICATION_EXECUTION_UNCERTAIN 'trusted tasks'; do
  grep -Fq "$marker" SECURITY.md || fail "SECURITY.md is missing the $marker boundary"
done

for method in turn/interrupt command/exec config/read config/batchWrite mcpServerStatus/list; do
  grep -Fq "\"$method\"" schemas/app-server/supported-methods.json ||
    fail "App Server fixture is missing $method"
  grep -Fq "\`$method\`" docs/compatibility.md ||
    fail "compatibility policy is missing $method"
done

python3 - <<'PY'
from pathlib import Path
import json
import re
import sys

root = Path.cwd()
fixture = json.loads((root / "schemas/app-server/supported-methods.json").read_text())
errors = []

if fixture["requestShape"].get("thread/resume") != {
    "default": ["threadId"],
    "primaryIntegration": ["threadId", "config"],
}:
    errors.append(
        "thread/resume must distinguish default [threadId] from "
        "primaryIntegration [threadId, config]"
    )

def text_for(path):
    return re.sub(r"\s+", " ", (root / path).read_text(encoding="utf-8")).replace("`", " ")

def require(path, claim, patterns):
    text = text_for(path)
    missing = [pattern for pattern in patterns if not re.search(pattern, text, re.I | re.S)]
    if missing:
        errors.append(f"{path} is missing semantic claim {claim}: {missing[0]}")

def forbid(path, claim, pattern):
    if re.search(pattern, text_for(path), re.I | re.S):
        errors.append(f"{path} contradicts the participant contract: {claim}")

def missing_patterns(text, patterns):
    return [pattern for pattern in patterns if not re.search(pattern, text, re.I | re.S)]

def require_text(path, text, claim, patterns):
    missing = missing_patterns(text, patterns)
    if missing:
        errors.append(f"{path} is missing semantic claim {claim}: {missing[0]}")

def recovery_window(path):
    text = text_for(path)
    marker = re.search(r"CONTROLLED_PATCH_TOOL_UNAVAILABLE", text)
    return text[max(0, marker.start() - 500):marker.start() + 1400] if marker else ""

variant_documents = [
    "docs/compatibility.md",
    "docs/protocol-v1.md",
    "docs/protocol-v2.md",
]
for path in variant_documents:
    require(path, "default resume stays threadId-only", [
        r"(?:default|ordinary|non-integration).{0,100}thread.?id.{0,100}(?:only|alone)",
    ])
    require(path, "Primary integration resume carries config", [
        r"primary.{0,80}integration.{0,120}(?:resume|resuming).{0,160}config",
    ])
    forbid(
        path,
        "config is universal for thread/resume",
        r"(?:all|every|ordinary|default).{0,80}(?:thread.?resume|resumes?).{0,80}config",
    )

semantic_documents = [
    "README.md",
    "docs/compatibility.md",
    "docs/protocol-v1.md",
    "docs/protocol-v2.md",
    "plugin/skills/worktree-merge-consensus/SKILL.md",
    "plugin/skills/worktree-merge-consensus/references/protocol.md",
]
for path in semantic_documents:
    require(path, "one-tool participant inventory", [
        r"(?:inventory|server|participant).{0,160}(?:exactly|only).{0,100}consensus_apply_patch|(?:exactly|only).{0,100}consensus_apply_patch.{0,160}(?:tool|inventory)",
    ])
    require(path, "preflight before every Primary integration turn", [
        r"(?:before every|every).{0,120}(?:primary.{0,40})?integration",
        r"mcpServerStatus/list",
        r"(?:before every.{0,80}(?:turn/start|such turn)|before.{0,80}turn/start)",
    ])
    require(path, "matching deployment and explicit resume", [
        r"(?:matching.{0,80}0\.2\.7|0\.2\.7.{0,80}matching)",
        r"explicit.{0,80}resume|consensus_resume",
    ])
    require(path, "same recovery identity", [
        r"same.{0,80}Run",
        r"round",
        r"branch",
        r"(?:old|prior).{0,40}(?:integration )?SHA",
        r"(?:failed.{0,80}verification|verification.{0,80}failed).{0,80}evidence",
    ])
    require(path, "one corrective patch and commit", [
        r"(?:at most one|one).{0,120}request-bound.{0,120}(?:patch|commit)",
    ])
    require(path, "SHA advance and complete frozen verification rerun", [
        r"(?:SHA.{0,80}(?:must )?advance|advance.{0,80}SHA)",
        r"(?:all|every).{0,80}frozen verification.{0,100}(?:rerun|runs again|reruns)",
    ])
    require(path, "installation alone does not mutate or recover", [
        r"(?:installing|installation|enablement).{0,160}(?:alone|never).{0,160}(?:mutat|recover)",
    ])
    forbid(
        path,
        "installation alone recovers a blocked Run",
        r"installation alone(?![^.]{0,80}(?:never|does not))[^.]{0,120}(?:mutat|recover)",
    )

recovery_claims = [
    ("exact recovery blocker", [r"CONTROLLED_PATCH_TOOL_UNAVAILABLE"]),
    ("matching deployment and explicit resume", [
        r"(?:matching.{0,80}0\.2\.7|0\.2\.7.{0,80}matching)",
        r"explicit.{0,80}(?:consensus_resume|resume)",
    ]),
    ("same recovery identity", [
        r"same.{0,80}Run",
        r"round",
        r"branch",
        r"(?:old|prior).{0,40}(?:integration )?SHA",
        r"(?:failed.{0,80}verification|verification.{0,80}failed).{0,80}evidence",
    ]),
    ("only the empty correction attempt is archived", [
        r"(?:empty|side-effect-free).{0,100}correction",
        r"(?:archive|archives).{0,80}only.{0,120}(?:correction|turn)",
    ]),
    ("repository lock reacquisition", [r"reacquir\w*.{0,80}lock"]),
    ("one request-bound correction patch", [
        r"(?:at most one|one).{0,120}request-bound.{0,160}(?:corrective|correction).{0,80}patch",
    ]),
    ("one request-bound correction commit", [
        r"(?:at most one|one).{0,180}request-bound.{0,180}(?:corrective|correction).{0,120}patch.{0,80}commit",
    ]),
    ("integration SHA advances", [r"(?:SHA.{0,80}(?:must )?advance|advance.{0,80}SHA)"]),
    ("complete frozen verification reruns", [
        r"(?:all|every).{0,80}frozen verification.{0,100}(?:rerun|runs again|reruns)",
    ]),
    ("installation alone does not mutate or recover", [
        r"(?:installing|installation).{0,120}alone.{0,160}(?:does not|never).{0,120}(?:mutat|recover)",
    ]),
]

for path in semantic_documents:
    window = recovery_window(path)
    for claim, patterns in recovery_claims:
        require_text(path, window, claim, patterns)

chinese_recovery = recovery_window("README.zh-CN.md")
chinese_claims = [
    ("exact recovery blocker", [r"CONTROLLED_PATCH_TOOL_UNAVAILABLE"]),
    ("matching deployment and explicit resume", [r"部署匹配的 0\.2\.7.{0,80}显式调用.{0,80}consensus_resume"]),
    ("same recovery identity", [r"同一 Run.{0,80}轮次.{0,80}分支.{0,80}旧 SHA.{0,100}失败.{0,80}冻结验证证据"]),
    ("only the empty correction attempt is archived", [r"只归档空的修正 turn"]),
    ("repository lock reacquisition", [r"重新获取锁"]),
    ("one request-bound correction patch", [r"(?:只允许|一次).{0,80}绑定请求的修正补丁"]),
    ("one correction commit", [r"(?:只允许|一次).{0,80}修正 commit"]),
    ("integration SHA advances", [r"新 SHA 必须前进"]),
    ("complete frozen verification reruns", [r"全部冻结验证命令会重新执行"]),
    ("installation alone does not mutate or recover", [r"仅安装或启用.{0,100}绝不会改变阻塞 Run"]),
]
for claim, patterns in chinese_claims:
    require_text("README.zh-CN.md", chinese_recovery, claim, patterns)

canonical_recovery = recovery_window("docs/compatibility.md")
for claim, patterns in recovery_claims:
    for pattern in patterns:
        mutated = re.sub(pattern, "", canonical_recovery, count=1, flags=re.I | re.S)
        if not missing_patterns(mutated, patterns):
            errors.append(f"docs gate mutation probe did not reject removal of {claim}")

if errors:
    print("\n".join(errors), file=sys.stderr)
    raise SystemExit(1)
PY

for notification in item/started item/completed turn/completed; do
  grep -Fq "\"$notification\"" schemas/app-server/supported-methods.json ||
    fail "App Server fixture is missing $notification"
  grep -Fq "\`$notification\`" docs/compatibility.md ||
    fail "compatibility policy is missing $notification"
done

[[ ! -e schemas/app-server/0.144.5-methods.json ]] ||
  fail 'obsolete version-specific App Server fixture still exists'

for document in README.md README.zh-CN.md docs/compatibility.md docs/real-codex-smoke-test.md; do
  grep -Fq '>=0.144.1' "$document" || fail "$document is missing the Codex version floor"
  if grep -Fq '<0.145.0' "$document"; then
    fail "$document still documents an obsolete Codex version ceiling"
  fi
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

for marker in x86_64-unknown-linux-musl aarch64-unknown-linux-musl musl-tools '+crt-static'; do
  grep -Fq "$marker" .github/workflows/ci.yml ||
    fail "CI is missing the portable Linux build contract: $marker"
done
grep -Fq 'bash tests/static-link.sh' .github/workflows/ci.yml ||
  fail 'CI omits the static-linkage gate'

for marker in x86_64-unknown-linux-musl aarch64-unknown-linux-musl SHA256SUMS cyclonedx-json plugin; do
  grep -Fq "$marker" .github/workflows/release.yml ||
    fail "release workflow is missing $marker"
done

for marker in musl-tools '+crt-static'; do
  grep -Fq "$marker" .github/workflows/release.yml ||
    fail "release workflow is missing the static-linkage gate: $marker"
done
grep -Fq 'bash tests/static-link.sh' .github/workflows/release.yml ||
  fail 'release workflow omits the static-linkage gate'

for marker in '(NEEDED)' 'Requesting program interpreter'; do
  grep -Fq "$marker" tests/static-link.sh ||
    fail "static-linkage gate is missing the rejection marker: $marker"
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
