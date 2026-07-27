# 恢复与故障排查

[English](recovery.md)

恢复策略是保守、显式并且始终绑定同一个持久 Run。仅安装新的 binary/plugin 不会修改或恢复
被阻塞的 Run。先解决上报条件并检查保留的 Git 状态；只有下面明确标记可恢复的原因才能调用
`codex-consensus resume RUN_ID`。

## 操作者命令

```bash
codex-consensus doctor
codex-consensus status RUN_ID --json
codex-consensus watch RUN_ID
codex-consensus resume RUN_ID
codex-consensus cancel RUN_ID
```

`cancel` 会保留已有 Git 状态，包括已经创建的集成分支；取消后的 Run 不能恢复。

## 恢复不变量

任何允许的重试开始前，协调器都会重新验证精确 Run、pending 请求、参与绑定、轮次、来源任务
ID、worktree 路径、来源引用与 SHA、授权目标分支和已有集成 SHA。它还会审计规范 App Server
历史中的完成命令、文件变更、MCP 调用、未知 item 和不确定副作用。

恢复绝不会暗中：

- 创建替代 Run；
- 选择不同任务或 worktree；
- 移动或修复来源引用；
- 重复不确定的补丁、merge、stage、commit 或测试命令；
- 接受未经精确 SHA 测试和复核的结果。

## 常见原因

| 原因 | 处理方式 |
| --- | --- |
| `COMMUNICATION_FAILURE` | 修复连接后显式恢复。只有规范历史证明不存在可产生副作用的 item 时，才会替换终态 failed/interrupted turn。 |
| `INVALID_TEST_COMMAND` | 修正声明的直接测试命令；Run 仍在集成前且历史只读时才可显式恢复。 |
| `INVALID_RESPONSE` | 只有集成前的契约或方案 turn 已完成、保持只读且身份匹配时才能重试。 |
| `EXECUTION_TOOL_UNAVAILABLE` | 恢复任务工具后，只有目标分支和写入都不存在时才会重试首次集成请求。 |
| `CONTROLLED_PATCH_TOOL_UNAVAILABLE` | 使用下文精确版本边界；任何近似状态仍是终态。 |
| `FORBIDDEN_OPERATION` | 某些历史精确只读命令误判具有窄范围同 Run 迁移；见下文版本说明。 |
| `INCOMPATIBLE_STATE` | 通常是终态。0.3.13 只识别下文精确的 Reviewer reasoning 生命周期遗留情况。 |
| `APPROVAL_CONFIGURATION_REQUIRED` | 插件通常会自动写入唯一的作用域键；若被托管策略阻止，使用相同 Codex 账号和 `CODEX_HOME` 运行 `codex-consensus configure`，然后重试原操作。 |
| `WAITING_THREAD` | 完成或中断无关的活动任务 turn，再恢复。 |
| `SOURCE_DRIFT` | 检查来源变化，使用重新确认的提交新建 Run；不要用不同来源身份恢复。 |
| `INTEGRATION_BRANCH_EXISTS` | 选择另一个新分支并新建 Run；协调器不会复用或删除该分支。 |
| `NO_PROGRESS`、`ROUND_LIMIT` | 终态。修订工作或契约后新建 Run。 |
| `VERIFICATION_EXECUTION_UNCERTAIN` | 设计上的终态。协调器无法证明冻结命令是否完成，因此不会自动重跑。 |
| `CANCELLED` | 终态；已有 Git 状态被保留。 |

## 受控补丁修正边界

部署匹配的 0.2.8 或更高版本后，显式调用 `consensus_resume` 只能恢复精确的
`CONTROLLED_PATCH_TOOL_UNAVAILABLE` 修正 blocker。恢复保留同一 Run、轮次、分支、旧 SHA 与
失败的冻结验证证据。它只归档空的修正 turn，在同一事务中重新获取锁；只允许一次绑定请求的修正补丁
和一次修正 commit。新 SHA 必须前进，全部冻结验证命令会重新执行。仅安装或启用匹配
版本绝不会改变阻塞 Run。

每个重试的 Primary turn 前，协调器都会重新执行参与绑定预检。参与工具面必须精确只有
`consensus_apply_patch`；选定 Source Primary、Effective Primary 谱系、Reviewer、来源引用和
worktree 保持冻结。任何 sent 或 uncertain turn 都不会重新 fork（refork）或重发（resent）。

## 已完成集成恢复

绑定请求补丁和集成 commit 已成功，但后续只读确认被拒绝时，恢复可以保留精确结果，而不是
重复写入。协调器要求：

- 成功补丁记录与请求哈希匹配；
- 授权目标在权威集成 SHA 上保持干净；
- 两个冻结提交都是结果祖先；
- 来源引用未变；
- 成功写入后的规范终态历史只包含允许的只读检查。

它只归档被拒绝的确认，并请求一次只读 `INTEGRATION_READY` 回复；绝不会重复补丁、建分支、
merge、stage 或 commit。

真实生产布局可能把成功补丁保存在已归档 ephemeral Primary 尝试中，而当前尝试只包含结果确认。
两份历史会分别验证，并且必须共享精确冻结 Source 谱系和请求身份。

### 历史 commentary/final-answer 审计迁移

0.3.12 可以恢复由 0.3.11 精确错误
`controlled patch call appears after the final agent response` 阻塞的同一个 Run，但不会只根据
错误文本判断资格。规范 App Server 历史必须证明：带 `phase: commentary` 的
`agentMessage` 位于唯一一次成功且绑定请求的受控补丁之前，带 `phase: final_answer` 的最终回复
位于补丁之后；同时成功补丁记录、干净的权威目标、来源祖先关系、冻结引用、请求身份和参与绑定
必须全部重新通过验证。

恢复只归档这次已完成回复，并请求一次只读 `INTEGRATION_READY` 确认。缺失、格式错误或未知的
消息 phase 在审计中仍被视为终态。真正发生在 `final_answer` 之后的补丁或命令、第二次补丁、
不完整历史或任何仓库漂移都不能恢复。

### 历史 Reviewer reasoning 生命周期迁移

0.3.13 只能在精确的 0.3.12 最终裁决错误
`turn <id> completed before all item lifecycle events were persisted` 下恢复同一个 Run，且 pending
动作必须是绑定 Reviewer 的 `REQUEST_REVIEWER_RESULT_VERDICT`。必须存在持久化且成功的
`turn/completed` 事件，并且所有未完成 item 都必须精确为 `type: reasoning`、状态为
`STARTED`。未完成的命令、MCP 调用、文件变更、未知类型、其他生命周期状态或未成功 turn
仍然失败关闭。

协调器会重新验证未变化的冻结引用、干净且已测试的集成 SHA、完整成功的冻结测试证据、请求
标记、Reviewer 身份、规范最终回复，以及 Reviewer 冻结 worktree 中已完成的只读 Git 轨迹。
随后直接消费这条已经完成的回复；不会再次发送 Reviewer turn，也不会重复验证、补丁、建分支、
merge、stage 或 commit。marker 回复与旧协议 JSON 回复都会在状态推进前完成验证。

## 历史只读命令迁移

0.2.14 增加精确只读查询 `git symbolic-ref --short HEAD`；0.3.1 增加
`git branch --show-current`。二者可以直接执行，也可以只带一层规范 App Server
`/bin/bash -lc` 包装。

如果旧版 `FORBIDDEN_OPERATION` 只由其中一条精确查询导致，安装匹配产物后可以恢复同一 Run。
恢复要求规范历史处于终态、冻结引用未变、权威目标干净；如果集成已经发生，还要求成功补丁
来源。带参数或可写变体、未知命令、文件变更、MCP 调用、不确定结果和最终回复后的命令仍是终态。

## daemon 与 App Server 重启

daemon 会在 App Server 派发前持久化待发送动作。重启后只恢复幂等读取和能够证明完成的动作；
丢失响应的非幂等 `turn/start` 不会盲目重发。已完成的协调器验证命令可以从精确日志复用，只有
STARTED 而无法证明完成的命令会失败关闭。

0.3.7 还识别一个严格边界：ephemeral Primary 的请求绑定补丁和干净提交已经成功，任务已
idle，但协调器没有持久化该 turn 的任何事件。此时会重新验证补丁来源、目标分支祖先关系、
工作树洁净性和冻结引用，刷新 App Server 连接，并且只重试一次只读结果标记；不会从部分
事件记录推断成功，也不会重复应用补丁。如果首次刷新连接本身失败并暂停 Run，修复连接后对
同一个 Run 显式 resume 会再次执行全部校验，并且仍然只允许这一次“仅确认”恢复。

0.3.8 只把同一边界扩展到 App Server 在热重载或生命周期切换后返回精确的
`-32600 / thread not loaded: <所请求的 ephemeral id>`。显式恢复同一个 Run 会先重复
0.3.7 的全部校验，归档零事件尝试，并且只有在确定性 pending 请求可证明尚未发送、冻结 Source
历史指纹不变时才轮换 ephemeral 绑定。新副本只接收最终确认请求；身份不一致、发送不确定、
部分事件或 Source 历史变化仍然失败关闭。

如果托管 App Server 在 daemon 存活时重启，`doctor` 会同时探测新直接连接和 daemon 内部 proxy，
并在幂等任务读取前修复已关闭 proxy。修复失败时应保留 Run，先修复 App Server 连接，再显式恢复。

## 安装故障排查

### 缺少插件工具

```bash
codex plugin list
codex mcp list --json
```

0.3.9 会在 MCP 握手前解析并安装精确版本的静态运行时。全新安装时，第一个任务最多等待五分钟
下载并校验同版本发布包。确认 `worktreeMergeConsensus` 已启用，检查其启动诊断中的下载、校验和、
架构或托管策略错误，然后新建启动任务。`consensus_doctor` 等 MCP 名称不是 shell 可执行文件。

已验证运行时按版本保存在插件私有 `PLUGIN_DATA` 中，故意不要求出现在 `PATH`。如果系统中还存在
直接安装的 `codex-consensus`，仍可选择执行：

```bash
command -v codex-consensus
codex-consensus --version
codex-consensus doctor
```

离线或集中运维安装可把 `CODEX_CONSENSUS_BIN` 指向一个可执行文件；其输出必须精确等于插件对应的
`codex-consensus <version>`。版本不匹配会直接拒绝，不会静默混用 binary/plugin。

### `LEGACY_SKILL_CONFLICT`

旧的手动安装 `$CODEX_HOME/skills/worktree-merge-consensus` 覆盖了插件流程。请自行备份或删除，
从对应 marketplace 发布版重新安装插件并新建任务；诊断不会自动删除旧目录。

### `INCOMPATIBLE_CODEX`

确认 `codex --version` 为 `>=0.144.1`，然后阅读[兼容策略](compatibility.md)。适配器还会检查
App Server 身份、必需方法和响应结构；仅通过数字版本下限并不足够。

### 仓库预检失败

- `DIRTY_WORKTREE`：先提交或有意识地处理本地修改。
- `UNREGISTERED_WORKTREE` / `DUPLICATE_WORKTREE` / `REPOSITORY_MISMATCH`：从同一次
  `codex-consensus worktrees list` 结果选择两个不同条目。
- `WORKTREE_UNAVAILABLE`：恢复冻结路径或新建 Run。
- `SOURCE_BINDING_MISMATCH`：修正任务到 worktree 的映射并新建 Run；resume 不能替换冻结身份。

逐版本迁移和精确规范历史形态见[兼容策略](compatibility.md)与
[版本记录](../CHANGELOG.md)。
