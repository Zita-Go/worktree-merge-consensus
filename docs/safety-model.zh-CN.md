# 安全模型

[English](safety-model.md)

Worktree Merge Consensus 用于降低两个可信 Codex 任务集成已提交代码时，行为和实现意图被意外
丢失的风险。它不是操作系统 sandbox，不是针对恶意代码的安全边界，也不能替代生产发布前的
人工复核。

## 信任前提

协调器启动的每个参与 turn 都使用 App Server 审批策略 `never` 和 sandbox 策略
`dangerFullAccess`。流程刻意采用无人值守模式：Primary 和 Reviewer 不会等待逐条命令确认，
App Server 也不会为这些 turn 提供操作系统隔离。

只有在以下对象都可信时才能使用：

- 本地主机账号和 Codex 安装；
- 两个选中任务及其已有历史；
- 两个来源提交和仓库指令；
- 每一条冻结测试命令及其执行的代码。

协调器可以在发现禁止证据或状态漂移后拒绝验收，但不能撤销参与任务或测试进程已经完成的动作。

## 冻结身份

复核开始前，协调器会冻结并持久化：

- Primary 和 Reviewer 任务 ID；
- 两个规范化、已注册的 worktree 路径；
- 共享 Git common directory；
- 来源引用、可能的 detached 状态和完整 commit SHA；
- 唯一目标集成分支；
- 声明的测试命令。

任务选择与 worktree 选择互相独立。App Server task cwd 只用于定位展示，绝不会被当作来源身份。
两个路径必须是同一 Git common directory 中不同、干净、已注册的 worktree。整个 Run 会持续
复验来源引用和 SHA。

## 写入所有权

只有 Primary 可以写入集成结果，而且只能写入授权的新本地分支。Reviewer 只读。协调器执行
权威 Git 检查并创建私有验证克隆，但没有 push、创建 PR、更新来源引用、rebase、reset、删除
分支、管理凭据或清理 worktree 的能力。

参与任务专用的 `consensus_apply_patch` 比普通仓库写权限更窄。它绑定一个活动 Run 和请求哈希，
最多接受一次成功且不超过 512 KiB 的纯文本补丁，拒绝不安全路径，使用 Git 预检，重新验证
冻结引用，并在 SQLite 中记录单次使用来源。它不能选择仓库、创建 Run、选择分支或发布结果。
所需审批设置是：

```text
plugins.worktree-merge-consensus.mcp_servers.worktreeMergeConsensus.tools.consensus_apply_patch.approval_mode = "approve"
```

`codex-consensus configure` 只写入并验证这个键；缺少或被覆盖时会以
`APPROVAL_CONFIGURATION_REQUIRED` 失败关闭。

## 验证证据

Primary 验证 turn 只负责 marker，禁止运行 Shell、Git、文件、MCP、补丁或其他工具。marker
完成后，daemon 通过 App Server `command/exec` 在精确集成 SHA 的干净 detached 克隆中执行
全部冻结的直接非 Git 命令。

验证克隆：

- Git common directory 与两个来源 worktree 独立；
- 没有 remote；
- 固定在精确结果 SHA；
- 保留在私有状态目录中，用于审计和恢复。

每条命令在派发前记录为 STARTED，完成后记录精确 turn 身份、确定性 item 身份、命令、cwd、
退出码和有界诊断。进程重启后可以复用精确 COMPLETED 结果；无法证明完成的命令会以
`VERIFICATION_EXECUTION_UNCERTAIN` 停止，绝不会自动重复。模型自行上报成功不构成测试证据。

## 参与任务绑定

第一个主修动作之前，选定的 **Source Primary** 会绑定到 **Effective Primary**。
`notLoaded` Source Primary 在加载任务作用域参与配置后直接绑定；已加载的 Source Primary 如果
缺少精确参与工具，则会在 goal 为 null 后使用 `ephemeral: true` 的完整历史 `thread/fork`。
镜像不会携带活动 Source goal，只使用 `thread/read(includeTurns: false)` 检查。

每个 Primary turn 之前，协调器都会在 `turn/start` 前读取全部 `mcpServerStatus/list` 页面，
并要求参与工具面精确只有 `consensus_apply_patch`。Reviewer 路由和两个冻结来源身份保持不变。
pending 或 uncertain turn 不会重新 fork，也不会重发；响应不确定的 fork 不会自动重试。
该绑定契约要求 Codex CLI `>=0.144.1`，并要求 App Server 方法和响应结构通过适配检查。

## 协议与历史检查

`worktree-merge-consensus/v2` 要求每次参与回复只包含一个
`<consensus-result>...</consensus-result>` 标记。契约包含一个机器可读 JSON 正文，后续说明采用
自由 Markdown。daemon 把每个回复绑定到精确 pending task turn，并独立推导分支、SHA、改动
文件、祖先关系和测试证据。

如果规范历史包含意外写入、发布、破坏性 Git、shell 串联、错误目录操作、未知 item 或不明确
副作用，验收会被拒绝。只允许规范化一层已知 App Server shell 包装后检查精确内部命令；嵌套
包装和动态启动器仍被禁止。

## 状态、事件与隐私

默认状态目录为 `$XDG_STATE_HOME/codex-consensus`；未设置时为
`~/.local/state/codex-consensus`。目录权限为 `0700`，Unix socket 和状态文件仅限当前用户。

SQLite 保存任务 ID、本地路径、引用、SHA、协议 payload、pending-send 元数据、结构化证据和
有界公开进度事件。它不保存凭据或完整任务会话全文。Codex 自身仍会在用户正常数据控制范围内
保存协调器消息和参与任务回复。

公开事件流可以包含已声明契约、方案、Reviewer 意见、集成身份、命令名与退出码和最终验收；
不会包含隐藏推理、参与任务提示词、原始任务历史或命令 stdout/stderr。

## 不在安全范围内

安全声明不覆盖被入侵的 Codex、Git、主机账号、操作系统、任务模型或仓库中已有的恶意代码。
测试使用 App Server 进程用户身份和 `dangerFullAccess` 执行；无 remote 克隆只提供 Git 隔离，
不是操作系统隔离。

漏洞报告和安全修复版本见 [SECURITY.md](../SECURITY.md)；精确适配和恢复边界见
[兼容策略](compatibility.md)与[恢复说明](recovery.zh-CN.md)。
