# Worktree Merge Consensus

[English](README.md)

`worktree-merge-consensus` 用于协调两个已有 Codex 任务之间经过复核的临时集成。
两个任务的实现分别提交在同一 Git 仓库的不同 worktree 中：主修任务负责提出方案并写入
集成结果，复核任务负责确认自己的功能与实现细节没有丢失。流程最终停在一个新的本地分支。

> **实验性依赖：** 本项目使用实验性的 Codex App Server 协议。v0.1 支持
> Codex CLI `>=0.144.1`；App Server 身份、必需方法或响应结构不匹配时仍会失败关闭。
> 真实集成前请先运行 `codex-consensus doctor`。

## 安全模型

协调器在复核前冻结两个任务 ID、worktree 路径、源引用与 commit SHA，然后强制执行：

1. 两个任务分别陈述行为、约束、测试及必须保留的实现细节。
2. 主修任务提出覆盖方案。
3. 复核任务持续提出具体缺口，直到认可某一个精确的方案版本。
4. 只有主修任务可以创建唯一的新本地集成分支并组合两个冻结提交。
5. 协调器为精确结果 SHA 创建干净、detached、无 remote 的独立克隆。单独的主修验证
   turn 在其中执行全部冻结测试；协调器依据 App Server 的命令执行项生成证据并检查 Git
   不变量。
6. 复核任务审计精确的集成 SHA；只有该 SHA 能被记录为验收通过。

`same-host`（同机）和 `no-push`（不推送）是刻意设定的边界。两个任务、两个
worktree、Git common directory、Codex App Server 和协调器必须位于同一台机器。
协调器不会 push、创建 PR、合并到已有分支、更新任一源引用、rebase、reset、删除或
清理 worktree。

这些限制不只依赖提示词：复核 turn 强制只读且断网；主修集成 turn 断网、只开放受限的
源仓库写入根目录，并且只能执行狭窄的 Git 命令集。单独的验证 turn 只能写入隔离克隆，
也只能执行精确冻结的测试命令。每个命令都必须在预期 cwd 中恰好出现一次，并对应退出码
为 0 的 App Server `commandExecution` 项；模型自行声称“通过”不构成证据。确定性审批规则
会取消发布、破坏性 Git、shell 串联、错误目录命令和权限升级。冲突扫描依据 Git 从主修源
SHA 到结果 SHA 的真实差异（包括大型文本文件），而不是任务自行上报的文件。

精确边界见 [v1 协议](docs/protocol-v1.md)、
[兼容性策略](docs/compatibility.md)与[安全策略](SECURITY.md)。

## 前置条件

- 发布二进制支持 Linux x86_64 与 ARM64；其他 Unix 系统只可视为开发环境。
- `PATH` 中可以调用 Git 与 Codex CLI。
- Codex CLI `>=0.144.1`，且提供所需的实验性 App Server 方法。
- 同一台机器、同一本地账号下恰好选择两个已有 Codex 任务。
- 独立于任务选择同一 Git common directory 下两个不同的注册 worktree。task cwd 只作为显示
  元数据；两个任务可以报告相同 cwd，也可以报告 Git 仓库外的目录。
- 两边实现均已提交且 worktree 均干净。源 HEAD 可以处于 detached 状态，因为身份按 SHA
  冻结；结果仍会创建在新的 attached 本地分支上。

## 安装独立二进制

从本仓库的 GitHub Release 下载 `x86_64-unknown-linux-musl` 或
`aarch64-unknown-linux-musl` 静态产物。这些二进制不依赖宿主机的 GLIBC 版本。请同时
下载 `SHA256SUMS`，并在解压前校验全部产物：

```bash
sha256sum --check SHA256SUMS
tar -xzf codex-consensus-v0.1.1-x86_64-unknown-linux-musl.tar.gz
install -m 0755 codex-consensus-v0.1.1-x86_64-unknown-linux-musl/codex-consensus ~/.local/bin/codex-consensus
```

v0.1.0 的 GNU 产物要求 GLIBC 2.39，现已停止推荐；受支持的 Linux 主机请使用
v0.1.1 或更高版本。

Release 还包含 CycloneDX JSON SBOM 与 Codex 插件包。在真实 Codex 验收记录完成之前，
v0.1 会标记为预发布；参见[真实环境冒烟测试记录](docs/real-codex-smoke-test.md)。

也可以从源码安装：

```bash
cargo install --locked --path crates/cli
```

构建 workspace 需要 Rust 1.85 或更高版本。

## 安装 Codex 插件

先确保 `codex-consensus` 已位于 `PATH`，并确保 binary/plugin 来自同一 release。在源码
checkout 中，将本仓库注册为本地 marketplace，再安装插件：

```bash
codex plugin marketplace add /absolute/path/to/worktree-merge-consensus
codex plugin add worktree-merge-consensus@worktree-merge-consensus
```

如果下载的是插件压缩包，请先解压，再注册包含
`.agents/plugins/marketplace.json` 的目录。安装或更新后重启 Codex，或新建任务。在 Codex
任务中调用 `$worktree-merge-consensus`；Skill 只通过 7 个 MCP 工具（包括
`consensus_list_worktrees`）启动和控制持久协调器，不会引入第三个 agent 代为转发复核对话。

若 `codex-consensus doctor` 返回 `LEGACY_SKILL_CONFLICT`，说明旧的手工安装目录
`$CODEX_HOME/skills/worktree-merge-consensus` 正在遮蔽插件工作流。请自行备份或删除该
目录，重新安装版本匹配的 binary/plugin，再重启 Codex 或新建任务；工具不会自动删除它。

## 使用 CLI

先检查环境并列出本机任务：

```bash
codex-consensus doctor
codex-consensus threads list
codex-consensus worktrees list --repository /absolute/path/to/repo --json
```

交互模式先选择主修与复核任务，再独立选择两个注册源 worktree；任务行中的 task cwd 不是
源绑定：

```bash
codex-consensus run
```

脚本或 JSON 调用必须同时给出两个任务 ID 和两个绝对 worktree 路径。分支参数可省略；
省略时协调器保留 `consensus/<run-id>`。每个 `--test` 都是主修任务必须在隔离验证 turn
中执行的精确直接命令。Git 命令、shell 控制符和动态 shell/解释器启动方式会被拒绝；
组合检查应直接调用仓库中已提交的测试脚本。

```bash
codex-consensus run \
  --primary-thread THREAD_ID_A \
  --primary-worktree /repo/.worktrees/change-a \
  --reviewer-thread THREAD_ID_B \
  --reviewer-worktree /repo/.worktrees/change-b \
  --integration-branch consensus/my-integration \
  --test "cargo test --workspace" \
  --json
```

查看单个运行或全部运行：

```bash
codex-consensus status RUN_ID
codex-consensus status --json
```

若运行因明确的用户操作暂停，请先解决显示的原因，再恢复同一个持久运行：

```bash
codex-consensus resume RUN_ID
```

确认要终止时再取消。取消不会回滚或删除已经存在的 Git 状态，包括已创建的集成分支：

```bash
codex-consensus cancel RUN_ID
```

7 个公开命令组是 `codex-consensus doctor`、`codex-consensus threads`、
`codex-consensus worktrees`、`codex-consensus run`、`codex-consensus status`、
`codex-consensus resume` 和 `codex-consensus cancel`。需要自动化时，可在 `--help`
标明的操作叶节点使用稳定 JSON 输出。

## 状态与恢复

| 状态 | 含义 |
| --- | --- |
| `RUNNING` | daemon 可以发送下一个确定性步骤。 |
| `WAITING_THREAD` | 某个选中任务已有进行中的 turn。 |
| `PAUSED_USER_ACTION` | 需要显式任务输入或其他外部操作；解决后再恢复。 |
| `ACCEPTED` | 测试、源引用不变量和复核认可均对应精确的集成 SHA；`accepted_result` 记录测试结果及“仅本地、未推送”边界。 |
| `BLOCKED` | 协议、安全、轮次上限或无进展条件使运行终止。 |
| `CANCELLED` | 用户取消；已有 Git 状态保持不变。 |
| `INCOMPATIBLE_CODEX` | Codex 超出已验证适配范围，或缺少必要方法。 |

daemon 在每次 App Server turn 前先把待发送动作写入 SQLite。进程重启后，下一次 CLI 或
MCP 请求会重新连接 daemon，并以幂等方式恢复可继续的工作。不要对 `BLOCKED` 或
`CANCELLED` 运行调用 `resume`。待完成的验证 turn 可能在克隆中留下测试产物；恢复时可
允许该克隆变脏，但仍强制要求持久化路径、精确 detached SHA、独立 Git common directory
且无 remote。

## 状态、日志与隐私

默认状态目录为 `$XDG_STATE_HOME/codex-consensus`；若未设置 `XDG_STATE_HOME`，则为
`~/.local/state/codex-consensus`。可用全局参数 `--state-dir DIR` 覆盖。目录中包含：

- `state.db`：SQLite 运行状态、冻结的 Git 事实、状态迁移和待发送元数据；
- `daemon.sock`：权限为 `0600` 的本地 Unix socket；
- `daemon.pid`：托管 daemon 的进程 ID。
- `verification/<run-id>-<integration-sha>`：用于精确 SHA 测试的 detached、无 remote
  克隆；其 Git common directory 与两个源 worktree 独立，v0.1 会保留它用于审计和恢复。

目录权限为 `0700`。数据库保存规范协议 payload 和证据，但不保存任务会话全文或生成的
prompt；Codex 自身仍会在两个选中任务的历史中保留消息。敏感 App Server 诊断会被脱敏。
托管 daemon 默认不创建持久日志文件，因此 CLI 输出、Codex 任务历史和 `status --json`
是运行记录。

## 故障排查

- `INCOMPATIBLE_CODEX`：检查 `codex --version`，再与
  [兼容策略](docs/compatibility.md)对照；低于 `0.144.1`、无法解析的版本输出，以及
  App Server 身份、方法或响应结构不匹配都会失败关闭。
- `INCOMPATIBLE_STATE`：预发布数据库缺少运行状态版本或版本未知。请保留旧目录用于审计，
  并换用新的 `--state-dir`；不要手工修改 SQLite。
- `DIRTY_WORKTREE`：新运行前先提交或有意识地处理两个源 worktree 的本地修改。
- `UNREGISTERED_WORKTREE`、`DUPLICATE_WORKTREE` 或 `REPOSITORY_MISMATCH`：从同一次
  `codex-consensus worktrees list` 输出中选择两个不同的注册路径。
- `WORKTREE_UNAVAILABLE`：冻结 worktree 缺失或无法访问；恢复后重新开始运行。
- `SOURCE_BINDING_MISMATCH`：任务判断用户确认的 worktree 并不包含其会话历史对应的实现；
  修正映射后新建运行，`resume` 不能替换冻结身份。
- `INTEGRATION_BRANCH_EXISTS`：选择新的分支名；协调器不会复用或删除已有分支。
- `SOURCE_DRIFT`：冻结的源引用或 worktree HEAD 已变化；检查 Git 后用新的提交重新运行。
- `PERMISSION_REQUIRED`：在对应 Codex 任务中处理权限请求，再执行
  `codex-consensus resume RUN_ID`。
- `NO_PROGRESS` 或 `ROUND_LIMIT`：这是终止状态；应修订契约后新建运行，不能强行验收。
- daemon 启动失败：检查状态目录所有权与权限；程序不会自动删除文件，必要时通过
  `--state-dir` 隔离后重试 `codex-consensus doctor`。

## v0.1 不做什么

- 跨机器、跨账号通信。
- 单次运行协调两个以上任务。
- 绕过普通 App Server 历史去读取另一个任务的隐藏上下文。
- push、创建 PR、合并到目标分支，或替用户决定部署基线。
- 复用、覆盖、删除或清理源分支、集成分支及 worktree。
- 替代安全敏感或生产发布中的人工复核。

## 开发

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash tests/docs.sh
bash tests/release-gate.sh
```

端到端测试使用进程级 fake App Server 与一次性 Git 仓库；真实 Codex 发布仍须完成单独的
[冒烟测试清单](docs/real-codex-smoke-test.md)。

本项目采用 [Apache License 2.0](LICENSE)。
