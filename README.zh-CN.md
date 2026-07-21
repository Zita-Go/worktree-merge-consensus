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
为 0 的 App Server `commandExecution` 项；模型自行声称“通过”不构成证据。冻结测试必须是
直接、非 Git 命令；Git 可执行文件、shell 控制符以及动态 shell/解释器启动器都会被拒绝。
确定性审批规则会取消发布、破坏性 Git、shell 串联、错误目录命令和权限升级。冲突扫描依据
Git 从主修源 SHA 到结果 SHA 的真实差异（包括大型文本文件），而不是任务自行上报的文件。

0.1.23 还支持安全策略禁止 Codex 的 bwrap 文件修改辅助程序启动的 Linux 容器。只有精确的
活动 Run 和请求哈希对应的主修参与 turn，才能在授权分支干净且同时包含两个冻结提交后调用
`consensus_apply_patch`。daemon 只接受一次成功的、不超过 512 KiB 的纯文本补丁；它会先让
Git 在不启用 unsafe paths 的前提下预检补丁，再重新验证两个源引用，并把单次使用结果记录到
SQLite。该能力没有公开 CLI 等价命令，也不能选择仓库、创建分支、启动 Run 或发布结果。

0.1.24 只为上述绑定请求的插件工具配置 Codex 审批：
`plugins.worktree-merge-consensus.mcp_servers.worktreeMergeConsensus.tools.consensus_apply_patch.approval_mode = "approve"`。
请使用运行 Codex 的同一本地账号执行一次 `codex-consensus configure`。该命令通过 App Server
`config/batchWrite` 写入、热加载用户配置并校验最终生效值；它不会修改命令审批、sandbox、
其他 MCP 工具或任何全局审批策略。若该值缺失或被覆盖，`doctor`、新 Run 启动和受控补丁恢复
都会失败关闭。

0.1.25 还会恢复一个窄范围的热加载竞态：App Server 在 Run 仍暂停时继续旧审批，协调器因此以
`PATCH_NOT_AUTHORIZED` 安全拒绝该绑定请求的补丁。显式恢复同一个 Run 时，只有规范历史中恰好
存在一个匹配且失败的补丁调用、阻塞身份完全一致、没有成功补丁记录、授权目标仍在上报的 merge
SHA 上保持干净并包含两个冻结祖先、且两个源引用未变，才会归档并替换该已完成主修 turn。恢复
复用现有 merge，绝不会创建替代 Run。

0.1.26 处理同一竞态的另一种 App Server 残留状态：精确的失败补丁调用和最终 blocker 已写入
规范历史，但主修 turn 仍停在 `inProgress + waitingOnApproval`。恢复同一 Run 时会先执行与
0.1.25 相同的身份、无写入、目标分支干净、祖先和冻结引用校验，然后只中断并替换这个陈旧 turn。
参与任务等待也从固定五分钟总时限改为默认 30 分钟的有界空闲时限；规范任务状态或 turn 历史变化
会续期，没有规范进度的任务仍以 `COMMUNICATION_FAILURE` 暂停。

0.1.27 继续处理更早一刻的精确残留状态：唯一的绑定请求补丁调用已在规范历史中标为 `failed`，
但 App Server 尚未写入最终 assistant JSON，并把 turn 留在 `inProgress + waitingOnApproval`。
只有其余规范 item 全部完成且仍在白名单、SQLite 没有成功补丁记录、授权集成分支在中断前后都
干净并保持同一已验证 merge SHA 时，才会恢复同一个 Run。未知 item、不明确写入、目标变化或源
漂移都会失败关闭。

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
tar -xzf codex-consensus-v0.1.27-x86_64-unknown-linux-musl.tar.gz
install -m 0755 codex-consensus-v0.1.27-x86_64-unknown-linux-musl/codex-consensus ~/.local/bin/codex-consensus
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
codex-consensus configure
codex-consensus doctor
```

如果下载的是插件压缩包，请先解压，再注册包含
`.agents/plugins/marketplace.json` 的目录。安装或更新后重启 Codex，或新建任务。在 Codex
任务中调用 `$worktree-merge-consensus`；插件提供 8 个 MCP 工具。其中 7 个（包括
`consensus_list_worktrees`）用于启动和控制持久协调器；第 8 个
`consensus_apply_patch` 是下文说明的、仅供内部参与 turn 使用且绑定精确请求的写入能力。
它不会引入第三个 agent 代为转发复核对话。

`consensus_doctor` 等名称是 MCP 工具名，不是 shell 可执行文件。Codex 会通过
`codex-consensus mcp-server` 启动插件服务；对应的终端诊断命令是
`codex-consensus doctor`。不要执行 `command -v consensus_doctor`。

若 `codex-consensus doctor` 返回 `LEGACY_SKILL_CONFLICT`，说明旧的手工安装目录
`$CODEX_HOME/skills/worktree-merge-consensus` 正在遮蔽插件工作流。请自行备份或删除该
目录，重新安装版本匹配的 binary/plugin，再重启 Codex 或新建任务；工具不会自动删除它。

`codex-consensus configure` 是安装流程唯一会主动写入的 Codex 配置。它只设置并验证上面的
插件/服务器/工具三级精确审批键。若托管配置层覆盖该值，配置与启动会返回
`APPROVAL_CONFIGURATION_REQUIRED`，不会要求操作者放宽更大范围的审批策略。

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

8 个公开命令组是 `codex-consensus configure`、`codex-consensus doctor`、
`codex-consensus threads`、`codex-consensus worktrees`、`codex-consensus run`、
`codex-consensus status`、`codex-consensus resume` 和 `codex-consensus cancel`。
需要自动化时，可在 `--help` 标明的操作叶节点使用稳定 JSON 输出。

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
MCP 请求会重新连接 daemon，并以幂等方式恢复可继续的工作。如果托管 App Server 在
协调器 daemon 仍存活时重启，daemon 会替换已关闭的 proxy，再重试幂等读取；`doctor`
会同时探测新建的直接连接和 daemon 内部实际持有的连接。对投递结果不确定的非幂等
`turn/start` 不会盲目重试。对 `COMMUNICATION_FAILURE` 显式执行 `resume` 时，只有在规范
历史确认旧 turn 已为 `failed` 或 `interrupted`，且没有命令、文件修改或未知副作用条目时，
协调器才会创建一次替代 turn；旧尝试仍保留在 SQLite 审计记录中。如果 contract 或 plan
声明了被禁止的 Git 测试，运行会以 `INVALID_TEST_COMMAND` 暂停。显式执行 `resume` 后，
协调器会重新验证两个冻结源，并且只有在规范历史不存在文件修改、未完成命令或未知 item
时，才归档已完成的 pre-integration 只读 turn 并请求一次修正响应。MCP 历史中只允许本插件
已完成的 `consensus_list_threads`、`consensus_list_worktrees` 或 `consensus_status` 查询；任何
写操作、外部 App 或未知 MCP 调用都会失败关闭。0.1.10 及更高版本也能恢复 0.1.9
产生的同类旧 `BLOCKED` 状态，并在同一事务中重新获取仓库锁。0.1.12 还能用相同的规范
历史检查恢复由模型格式错误导致的合并前 `BLOCKED / INVALID_RESPONSE`，但只限精确的已完成
只读 turn；合并后或含副作用的无效响应仍是终态。0.1.14 会为每个受约束的 App Server
turn 显式选择同机 `local` 执行环境；空环境数组会禁用命令与文件工具。对于精确的合并前
`BLOCKED / EXECUTION_TOOL_UNAVAILABLE`，只有在规范历史、响应哈希、源引用、干净工作树和
目标分支不存在共同证明没有发生集成副作用后，才允许恢复同一个 Run。不要对无关 `BLOCKED` 或任何
`CANCELLED` 运行调用 `resume`。0.1.15 将 App Server 的
`proposedExecpolicyAmendment` 视为一次性 `accept` 不会应用的元数据；网络或额外权限请求仍会取消。
对于首次集成阶段的精确 `BLOCKED / FORBIDDEN_OPERATION`，也只有在被拒 turn 的规范状态为
`failed` 或 `interrupted`、不存在可产生副作用的 item、两个工作树和引用仍冻结且干净、目标分支仍
不存在时，才允许恢复同一个 Run。0.1.16 能识别 App Server 规范化输出中的单层已知 shell
`-c` 或 `-lc` 包装，只剥离一层，然后继续对内部命令执行原有 Git 或冻结测试白名单。嵌套 shell、
子命令审批回调、非 `local` 执行环境和额外权限仍会失败关闭。0.1.17 只向集成白名单增加精确的
`git show-ref --verify refs/heads/<目标集成分支>` 预检。0.1.19 另外只允许等价且精确的
`git branch --list <目标集成分支>` 存在性查询，其他任何 `git branch` 形式仍会拒绝。同一 Run 的
禁止操作恢复可以保留处于终态的只读 Git 查询，但每个规范 item 都必须使用冻结的主修 cwd 且仍通过该白名单。
0.1.20 会把协调器发出的主修与复核 turn 明确标记为内部参与轮次，避免递归调用启动器 Skill；恢复时只能丢弃
已被拒绝的、精确读取本插件版本化 `SKILL.md` 的 `sed -n 1,240p` 旧查询，该查询仍不属于实时执行白名单。
0.1.21 还会识别 Codex App Server 的精确内部 `contextCompaction` 标记，但只有该对象除固定 `type`
和非空 `id` 外不含任何字段时，恢复审计才将其视为安全；它只是上下文生命周期记录，不是命令、文件修改或工具调用。
0.1.22 仅在冻结的主修 cwd 中允许精确的 `rg --files -g AGENTS.md`，用于发现仓库指令；其他 `rg` 形式仍被拒绝，
后续受跟踪文件检查必须使用既有的只读 Git 查询白名单。额外字段、`inProgress`、写命令、错误 cwd、未知 item 或其他副作用仍是终态。
0.1.23 增加上述绑定请求的 `consensus_apply_patch` 路径。若通信超时后精确的已完成主修响应为
`BLOCKED / FILE_CHANGE_TOOL_UNAVAILABLE`，只有在规范历史、批准方案身份、bwrap 权限失败证据、
上报 merge SHA、干净的授权目标分支、两个源祖先和冻结引用全部一致时，才可恢复同一个 Run。
恢复会保留已有 merge 并归档失败参与 turn；不会重新创建分支、再次 merge 或创建替代 Run。
0.1.24 通过要求上述精确工具审批设置，避免内部补丁调用陷入用户审批死锁。若旧尝试的规范
状态已经是 `waitingOnApproval`，且只有一个 `inProgress` 的 `consensus_apply_patch` 调用，
显式恢复同一 Run 时可以中断并替换该精确主修集成 turn。daemon 会先校验绑定请求的工具参数、
所有已成功执行的白名单命令、尚无成功补丁记录、授权目标干净、两个源提交均为祖先，以及冻结
引用未变。未知或多个工具调用、其他未完成 item、漂移或任何可能写入都会失败关闭；若 turn 在
中断竞态期间已经完成，则直接复用，不会重复执行。
0.1.25 处理与之对应的“已完成拒绝”竞态：配置热加载后，App Server 可能在 Run 仍暂停时立即
继续旧审批，daemon 会用 `PATCH_NOT_AUTHORIZED` 拒绝该精确补丁调用。显式恢复时，只能归档并
替换一个规范已完成的主修 turn；其中必须恰好有一个绑定请求且状态为失败的
`consensus_apply_patch` 调用，并携带完全匹配的 blocker。daemon 还要求没有成功补丁记录、现有
目标分支在上报 merge SHA 上干净、包含两个冻结提交祖先且源引用未变。未知或额外工具调用、成功
或不明确的写入、证据不匹配和仓库漂移仍是终态；不会再次创建分支或 merge。
0.1.26 还处理 App Server 已保存完全相同的失败工具调用和最终 blocker、却把 turn 留在
`inProgress + waitingOnApproval` 的精确状态。显式恢复会重新执行 0.1.25 的全部校验，只中断并
原子归档这个陈旧 turn，再重试同一请求。参与任务等待改为默认 30 分钟的规范无进度空闲时限；
规范任务状态或 turn 历史发生变化会续期，未变化的活动状态仍有界并失败关闭。
0.1.27 还允许这一陈旧 turn 尚无最终 assistant JSON，但仅限唯一绑定请求的补丁 item 已在规范
历史中标为 `failed`。daemon 会确认没有成功补丁记录，要求所有命令已完成且仍在白名单，中断前
记录干净授权分支的 merge SHA，并在中断后再次要求同一 SHA 和干净仓库状态，之后才原子重试。
若已存在 assistant 消息，它仍必须是通过完整校验的精确 `PATCH_NOT_AUTHORIZED` blocker。
0.1.13 还会在两类批准请求旁给出带权威值的扁平 payload
模板，并由 JSON Schema 拒绝仅嵌套在其他对象中的批准身份。待完成的验证 turn 可能在克隆中留下测试产物；恢复时
可允许该克隆变脏，但仍强制要求持久化路径、精确 detached SHA、独立 Git common directory
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

- 缺少 `consensus_*` 工具：先在同一主机环境运行 `codex-consensus doctor`。成功只说明
  binary 与协调器正常，不代表插件工具已注册。运行 `codex mcp list --json`，确认
  `worktreeMergeConsensus` 存在且已启用；若缺失，请重新安装匹配版本并新建任务。不要查找
  名为 `consensus_doctor` 的可执行文件。内置启动器会依次检查
  `CODEX_CONSENSUS_BIN`、`PATH`、`codex` 所在目录、`/usr/local/bin` 和
  `~/.local/bin`。
- `INCOMPATIBLE_CODEX`：检查 `codex --version`，再与
  [兼容策略](docs/compatibility.md)对照；低于 `0.144.1`、无法解析的版本输出，以及
  App Server 身份、方法或响应结构不匹配都会失败关闭。
- `APPROVAL_CONFIGURATION_REQUIRED`：使用与 Codex 相同的账号和 `CODEX_HOME` 运行
  `codex-consensus configure`，然后重新执行 `doctor`。必须设置的是上文精确的
  `consensus_apply_patch` 键；不要启用全局自动审批。若被托管层覆盖，应在控制该配置的层级修正。
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
