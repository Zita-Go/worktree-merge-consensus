# 快速演示

[English](quick-demo.md)

这个演示会创建一个一次性仓库，让两个 Codex 任务因为不同目标修改同一个小程序。正确集成必须
同时保留两项行为，因此 Reviewer 有明确细节需要保护。

演示仍使用无人值守 `dangerFullAccess`，只能在可信主机上对一次性文件运行。

## 1. 创建仓库

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

现在有两个已注册来源 worktree：

```text
/tmp/worktree-merge-consensus-demo/.worktrees/json
/tmp/worktree-merge-consensus-demo/.worktrees/uppercase
```

## 2. 创建两个 Codex 任务

在同一主机和本地 Codex 账号中，为两个 worktree 分别打开一个任务。

给第一个任务发送：

```text
增加 `./greet --json NAME`。输出必须是一个包含 `greeting` 字段的 JSON 对象，
同时保留现有纯文本行为。扩展并运行 `test.sh`，然后在 feature-json 上提交实现。
```

给第二个任务发送：

```text
增加 `./greet --uppercase NAME`。它只能把最终问候语转成大写，同时保留现有
纯文本行为。扩展并运行 `test.sh`，然后在 feature-uppercase 上提交实现。
```

等待两个任务都提交，然后确认两个 worktree 干净：

```bash
git -C "$DEMO/.worktrees/json" status --short
git -C "$DEMO/.worktrees/uppercase" status --short
```

两条命令都不应输出内容。

## 3. 在 Codex 中启动共识

在同一主机新建第三个 Codex 任务。它是启动器，不是第三个来源或复核参与者。调用：

```text
$worktree-merge-consensus:worktree-merge-consensus
```

选择：

| 角色 | 任务 | Worktree |
| --- | --- | --- |
| Primary | JSON 实现任务 | `/tmp/worktree-merge-consensus-demo/.worktrees/json` |
| Reviewer | 大写实现任务 | `/tmp/worktree-merge-consensus-demo/.worktrees/uppercase` |

如果询问额外冻结测试命令，直接使用已提交脚本：

```text
./test.sh
```

不要使用 `bash test.sh`；动态 shell/解释器启动器不能作为冻结测试命令。

## 4. 观察复核

启动任务应持续展示：

1. 双方独立契约；
2. 同时覆盖 `--json`、`--uppercase` 和纯文本行为的 Primary 方案；
3. Reviewer 的缺口意见及修订方案；
4. 唯一新本地集成分支和精确 SHA；
5. 冻结 `./test.sh` 的退出码；
6. Reviewer 针对精确 SHA 的决定和终态。

如果 Run 到达 `ACCEPTED`，使用只读命令验证结果：

```bash
codex-consensus status RUN_ID --json
git -C "$DEMO/.worktrees/json" branch --list 'consensus/*'
git -C "$DEMO/.worktrees/json" status --short
git -C "$DEMO/.worktrees/uppercase" status --short
```

验收记录应给出新本地集成分支与 SHA、冻结测试证据，并说明两个来源引用未变且没有 push。

## 5. 保存可公开证据

录制公开演示或发布验收时，请脱敏本地任务 ID、账号标识、主机名和用户目录路径。只展示启动
任务中的有界公开进度、最终分支/SHA、测试退出码，以及两个来源引用未变化的只读 Git 证据。
未经检查不要发布原始参与历史、隐藏推理、提示词、状态数据库或命令输出。

完整清单见[真实 Codex 验收记录](real-codex-smoke-test.md)。
