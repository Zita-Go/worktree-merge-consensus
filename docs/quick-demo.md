# Quick Demo

[简体中文](quick-demo.zh-CN.md)

This walkthrough creates a disposable repository where two Codex tasks edit the
same small program for different reasons. The correct integration must preserve
both behaviors, which gives the Reviewer something concrete to protect.

The demo still uses unattended `dangerFullAccess`. Run it only on a trusted host
with disposable files.

## 1. Create the repository

```bash
DEMO=/tmp/worktree-merge-consensus-demo
mkdir -p "$DEMO"
cd "$DEMO"
git init -b main

cat > greet <<'EOF'
#!/bin/sh
set -eu
name="${1:-world}"
printf 'Hello, %s!\n' "$name"
EOF

cat > test.sh <<'EOF'
#!/bin/sh
set -eu
test "$(./greet Codex)" = "Hello, Codex!"
EOF

chmod +x greet test.sh
git add greet test.sh
git commit -m "Create greeting example"

git worktree add .worktrees/json -b feature-json main
git worktree add .worktrees/uppercase -b feature-uppercase main
```

The two registered source worktrees are now:

```text
/tmp/worktree-merge-consensus-demo/.worktrees/json
/tmp/worktree-merge-consensus-demo/.worktrees/uppercase
```

## 2. Create two Codex tasks

Open one Codex task for each worktree on the same host and local Codex account.

Give the first task this request:

```text
Add `./greet --json NAME`. It must output one JSON object with a `greeting`
field while preserving the existing plain-text behavior. Extend `test.sh`, run
it, and commit the implementation on feature-json.
```

Give the second task this request:

```text
Add `./greet --uppercase NAME`. It must uppercase only the rendered greeting
while preserving the existing plain-text behavior. Extend `test.sh`, run it,
and commit the implementation on feature-uppercase.
```

Wait for both tasks to commit their work. Confirm both worktrees are clean:

```bash
git -C "$DEMO/.worktrees/json" status --short
git -C "$DEMO/.worktrees/uppercase" status --short
```

Both commands should print nothing.

## 3. Launch consensus in Codex

Open a fresh third Codex task on the same host. This is the launcher, not a
third source or review participant. Invoke:

```text
$worktree-merge-consensus:worktree-merge-consensus
```

Choose:

| Role | Task | Worktree |
| --- | --- | --- |
| Primary | JSON implementation task | `/tmp/worktree-merge-consensus-demo/.worktrees/json` |
| Reviewer | Uppercase implementation task | `/tmp/worktree-merge-consensus-demo/.worktrees/uppercase` |

When asked for an extra frozen test command, use the committed script directly:

```text
./test.sh
```

Do not use `bash test.sh`; dynamic shell/interpreter launchers are not accepted
as frozen test commands.

## 4. Observe the review

The launcher task should display:

1. both independent contracts;
2. a Primary plan covering `--json`, `--uppercase`, and plain text;
3. any Reviewer objection and the revised plan;
4. the unique local integration branch and exact SHA;
5. the frozen `./test.sh` exit code;
6. the Reviewer's exact-SHA decision and terminal status.

If the Run reaches `ACCEPTED`, verify the result without changing it:

```bash
codex-consensus status RUN_ID --json
git -C "$DEMO/.worktrees/json" branch --list 'consensus/*'
git -C "$DEMO/.worktrees/json" status --short
git -C "$DEMO/.worktrees/uppercase" status --short
```

The accepted record should name a new local integration branch and SHA, report
the frozen test evidence, and state that both source refs are unchanged and no
push occurred.

## 5. Record reusable evidence

For a public demo or release qualification, redact local task IDs, account
identifiers, hostnames, and home-directory paths. Capture only the launcher's
bounded public progress, final branch/SHA, test exit codes, and read-only Git
proof that both source refs remained unchanged. Never publish raw participant
history, hidden reasoning, prompts, state databases, or command output without
review.

The full qualification checklist is in
[Real Codex Smoke-Test Record](real-codex-smoke-test.md).
