# 协作核心接口 v0.1

## 运行和连接

```sh
cargo run --locked --manifest-path collab/Cargo.toml -- /absolute/path/to/state /absolute/path/to/codex
```

这是供桌面客户端复用的后台入口，不是图形应用。标准输入每行一个 JSON 请求，标准输出每行一个 JSON 响应或通知。密钥只通过受控标准输入或后续 Tauri 内部调用提交；不要放进命令行参数。

请求形状：`{"id":1,"method":"session/list","params":{}}`。
响应形状：`{"id":1,"result":[...]}`；错误形状：`{"id":1,"error":{"message":"..."}}`。
并发响应可能乱序，按 id 匹配。通知形状：`{"method":"collab/update","params":{...}}`。
通知包括 `agent_delta`、`approval_requested`、`task_changed`、`resync_required`。每条均带相关 sessionId、agentId、taskId；resync_required 除外。广播缓冲最多 256 条；收到 resync_required 或重新连接时用 event/list、task/list、task/output 补读。

每个状态目录只允许一个宿主打开。多个窗口应复用同一个 Runtime/宿主，不各自启动服务。输入关闭或 Ctrl+C 结束宿主并取消正在执行的任务；已排队任务仍保留，下次启动会继续调度。持久化任务的运行中状态在崩溃后会转成 interrupted，不自动重放。

## 方法

| 方法 | params / 行为 |
| --- | --- |
| session/create | repository（绝对 Git 项目路径）、validation（非空参数数组列表）、可选 requirement。从 HEAD 创建协作分支及集成 worktree，不包含原目录未提交修改。 |
| session/list | 返回持久化会话。 |
| agent/configure | id（UUID）、sessionId、role（coordinator/worker）、model、baseUrl、protocol（responses/chat_completions）、contextBytes（4096..1000000）。运行中拒绝修改。 |
| agent/list | sessionId。返回最多一个主 Agent、三个工作 Agent。 |
| agent/setCredential | agentId、key。使用 macOS Keychain；无读取明文密钥接口。运行中拒绝修改。 |
| agent/deleteCredential | agentId。删除 Keychain 项，不撤回已经发出的请求。 |
| agent/testConnection | agentId。排队运行一个要求公开文本及 collab_publish 工具调用的能力探针。 |
| agent/connectionResult | taskId（探针任务）。仅当任务成功、流完成且确实收到探针工具结果时返回 verified=true 和 text/stream/function_tools。 |
| task/create | sessionId、agentId、goal、acceptance、dependencies（任务 ID 数组，默认空）、coding（默认 false）。忙碌时排队。 |
| task/list | sessionId。 |
| task/get | taskId。包含 status、contextVersion、worktree、baseCommit、commit、result、integrationStatus。 |
| task/output | taskId。返回持久化结果摘要和运行中的公开消息草稿。完整结果位于附件事件引用的本地文件。 |
| task/cancel | taskId。取消该任务及其进程组，不取消其他独立任务。 |
| task/integrate | taskId、expectedCommit、review。供用户明确要求集成；主 Agent 也可通过协作工具调用。 |
| event/list | sessionId、after（默认 0）、limit（默认 100，最多 1000）。按递增 revision 返回；继续补读时使用最后一项 revision。 |
| event/publish | sessionId、kind（requirement/user_override/suggestion）、data。用户约束优先于模型决策。 |
| context/compact | sessionId、summary（最多 8192 字节）、expectedRevision。保留全部审计历史，用检查点替代旧模型决策的活动视图；不丢弃用户约束。 |
| approval/list | 返回当前待处理审批，包含关联 Agent、任务及具体操作。 |
| approval/decide | approvalId、accept（布尔值）。只批准本次操作，过期或重复的审批会被拒绝。 |

所有客户端方法均代表本机用户的显式操作，不提供远程 HTTP 管理接口。模型侧另有角色检查：只有主 Agent 能创建、取消其他任务、发布正式决策、压缩上下文及集成；工作 Agent 只能查看任务/差异及发布建议、结果。

## 状态与同步约定

- 任务状态：queued → running → completed/failed/cancelled；崩溃遗留为 interrupted，失败依赖为 blocked。
- 编码任务 completed 表示产物已提交，需 integrationStatus=integrated 才能满足下游依赖。
- 集成状态：pending → checking → integrated/conflict/failed/interrupted。冲突与失败保留集成工作区供人工核查，不自动清理或重复执行。
- contextVersion 表示已成功完成模型请求所消费的共享版本。正在执行的请求使用请求开始时快照，后续请求重新读取共享记录。
- 每次请求都装配当前权威记录、任务、Agent 名单及最近活动。活动上下文不会因宿主的消费游标而丢失；注入内容并不依赖 App Server 是否把它写回原对话。
- contextBytes 是保守的 UTF-8 请求字节预算，不等同于供应商 token 上限。超预算明确暂停任务，需压缩记录、提高预算或通过交接摘要新建任务；不静默截掉用户约束。
- 每个任务拥有自己的执行会话、CODEX_HOME 和 worktree。Agent 面板可聚合这些任务的公开消息；原始执行历史保存在对应 homes 目录，不共享私有推理。

## 协议能力边界

Responses 保留原协议，通过本地入口注入共享记录。Chat Completions 转换文本消息、函数工具、命名空间及以字符串传参的自定义工具，重组分片参数和 usage。自定义工具的原格式约束放在函数说明中，由执行工具再次校验。

图片、内置搜索、服务端增量历史、结构化输出及其他无法转换的能力明确报错。私有 reasoning 不跨协议重放。HTTP 429/5xx 在未开始读取响应流时最多请求三次；不自动重放模糊的传输错误和已开始输出的流。禁止跨地址重定向。

真实 API Key 只留在 Keychain 和请求适配器内存中。执行子进程仅获得本地短期令牌；环境不继承其他 API 密钥。日志、公开结果、审批参数及输出附件按当前凭证脱敏，流式脱敏覆盖跨片段的凭证。验证命令直接传参数数组，无隐式 shell 拼接，并使用隔离后的环境。

## 验证和合并

主 Agent 读取指定提交的差异并提交 review/expectedCommit 后，宿主把结果 squash 到专用集成工作区，执行每个配置的验证命令（单项最多 300 秒）。验证通过且未改写候选文件时才提交、推进协作分支。过期基础提交上的成果同样在最新协作分支验证。不会推送远端或修改原项目主分支。
