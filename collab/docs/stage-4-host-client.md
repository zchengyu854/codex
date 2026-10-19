# 阶段 4：终端接入协作后台

状态：第一段（IPC 骨架与真实会话/Agent/任务显示）、第二段（操作审批与密钥提交）、第三段（编码开关、差异审查与集成）与第四段（会话创建表单、上下文压缩）完成并验证。

## 目标与范围

终端壳层此前是纯模拟（`App::demo`）。本阶段让 `codex-collab-tui` 成为协作后台的真实客户端：按用户选定方案采用**子进程宿主 + JSON 行 STDIO**，遵守 `docs/core-api.md` 的请求/响应/通知约定，不改动 `core` 的现有结构与协议。

## 第一段：IPC 骨架与真实显示

- 启动并复用 `codex-collab-core` 宿主（`--state <目录>`；宿主自带单实例状态锁）；退出时关闭 stdin 让宿主停止调度并取消运行中任务。
- 请求按递增 id 匹配响应，异步响应乱序也能对应；处理 `collab/update` 通知与 `resync_required` 全量补读。
- 用真实数据填充四个窗格：一个主控 + 最多三个工作 Agent；`agent/list` 决定角色、模型、上游地址与协议。
- 用 `task/list` 显示每个 Agent 最新任务的本地化状态；`event/list` 增量读取公开消息、任务完成、集成与产物，写入窗格历史与共享记录。
- `agent_delta` 通知把公开文本流式追加到当前任务草稿（窗格内以强调色单独显示）。
- 焦点窗格输入 Enter → `task/create`（goal 为该文本；是否编码任务由第三段开关决定）；Ctrl+C → `task/cancel`；Ctrl+R → 刷新；F3 显示共享记录。

## 第二段：操作审批与密钥提交

- `approval_requested` 通知触发 `approval/list`，前台弹出审批模态：显示 Agent、任务、操作方法与参数摘要。`Y`/Enter 批准、`N`/Esc 拒绝、Tab/方向键在多个待处理审批间切换；决定经 `approval/decide` 提交，只对本次操作生效。`approval_resolved` 事件回写窗格。
- `Ctrl+K` 打开遮罩密钥表单（焦点 Agent）；`Enter` 保存（`agent/setCredential`）、`Ctrl+D` 删除（`agent/deleteCredential`）、`Esc` 取消。密钥输入不回显、不进入窗格历史与共享记录，仅通过受控标准输入提交给宿主；宿主按原有方式写入 Keychain，终端不提供读取明文密钥的接口。
- 密钥表单与审批模态都在普通按键处理之前拦截输入，避免误触发送或取消。

## 第三段：编码开关、差异审查与集成

- `Ctrl+E` 切换“编码任务”开关（底部状态栏显示“编码:开/关”）；开启后输入 Enter 以 `coding:true` 派发，产物提交后需人工审查集成。
- `Ctrl+G` 打开审查模态：对该 Agent 最新任务请求 `task/get`，校验状态为 `completed`、`coding` 为真、`commit` 非空且未集成；随后展示候选提交、结果摘要与差异（来自产物附件 `file diff`，本地只读、上限 100000 字节）。
- 审查模态底部是审查结论输入行（可见）：`Enter` 以 `expectedCommit` 为精确提交调用 `task/integrate`，`PgUp/PgDn` 滚动差异，`Esc` 取消。后台按原有流程验证候选、处理冲突并推进协作分支；结果（`integrated`/`conflict`/`failed`/`interrupted`）回显到提示行并触发刷新。
- 非编码任务、未完成、无提交或已集成的任务会被明确拒绝，不会误触发集成。

## 第四段：会话创建表单与上下文压缩

- `Ctrl+N` 打开会话创建表单（仓库绝对路径、每行一条验证命令、可选需求；Tab/↑↓ 切换字段，Enter 创建，Esc 取消）。表单预填启动时给出的 `--repository`/`--validation`；没有会话时若未提供这些参数，客户端自动打开表单而不是报错。提交走 `session/create`。
- `Ctrl+P` 打开上下文压缩：先按 `EVENT_PAGE`（500）分页 `event/list` 追赶到会话当前 revision（上限 20 页，避免活跃会话卡住界面），再弹出摘要表单（顶部显示该 revision）。Enter 以 `expectedRevision` 精确提交 `context/compact`；后台报 `context changed; refresh before compacting` 时自动重新追赶版本、保留已输入摘要并提示重新确认，不静默失败。
- 两个表单都在普通按键前拦截输入；摘要限制 1..8192 字节（与后台一致），验证命令按空白拆分为参数数组，不做隐式 shell 拼接。

## 运行

```sh
cargo build --locked --manifest-path collab/Cargo.toml -p codex-collab-core
cargo run  --locked --manifest-path collab/Cargo.toml -p codex-collab-tui -- \
  --state /absolute/path/to/state --session <会话 ID>
```

宿主按顺序解析：`--host`（或环境变量 `CODEX_COLLAB_CORE`）→ 与当前可执行文件同目录的 `codex-collab-core` → PATH。没有会话时给出 `--repository <Git 路径> --validation '<命令>'` 会按该仓库创建会话；`--demo` 仍是纯模拟，保持不变。

## 协议映射

| 客户端行为 | 后台方法 / 通知 | UI 效果 |
| --- | --- | --- |
| 启动、`--session` 缺省 | `session/list`（空则 `session/create`） | 会话标签、四窗格占位 |
| 载入角色 | `agent/list` | 主控/工作角色、模型、协议、上游地址 |
| 状态显示 | `task/list` | 排队/工作中/已完成/失败/已取消/已中断/等待依赖 |
| 历史与共享记录 | `event/list`（`after` 递增） | 公开消息、任务完成、集成、产物、用户约束、决策 |
| 公开文本流 | `collab/update: agent_delta` | 窗格草稿实时追加 |
| 任务状态变化 | `collab/update: task_changed` | 重新拉取 `task/list` 与增量事件 |
| 事件缓冲溢出 | `collab/update: resync_required` | 清空共享记录、`after=0` 全量补读 |
| 操作审批 | `collab/update: approval_requested` → `approval/list` | 审批模态弹出 |
| 审批决定 | `approval/decide` | 模态移除、状态回写 |
| 密钥保存 / 删除 | `agent/setCredential` / `agent/deleteCredential` | 遮罩表单提交，仅显示结果 |
| Ctrl+N / 会话创建表单 | `session/create` | 新建会话并附加 |
| Ctrl+P / 压缩表单 | `event/list` 追赶 → `context/compact` | 写入检查点并刷新 |
| 焦点窗格 Enter | `task/create`（受“编码”开关影响） | 派发任务并回读状态 |
| 焦点窗格 Ctrl+C | `task/cancel` | 取消该 Agent 当前任务 |
| Ctrl+E | —（本地开关） | 切换编码任务，底部显示状态 |
| Ctrl+G → 审查模态 | `task/get` + 本地读取差异附件 | 展示候选提交/摘要/差异 |
| 审查模态 Enter | `task/integrate` | 验证候选并合入协作分支 |
| Ctrl+R | `session/list` 或 `agent/list` + `approval/list` | 手动刷新 |

状态与协议名称在 `client.rs` 内集中映射，未识别的取值回退为“空闲/—”，不会 panic。

## 安全与边界

- 客户端只通过宿主的行协议交互，不直接访问数据库、密钥或模型适配器；宿主自身的脱敏、审批与进程组取消机制继续生效。审查模态展示的差异来自宿主产物附件的**本地只读**读取（仅 `file diff` 类型、限长截断），不写入任何记录。
- 密钥只经标准输入提交，界面全程遮罩且不写入历史；终端不提供读取或回显明文密钥的路径。
- 关闭终端即可让宿主取消运行中任务，与 `core-api.md` 的约定一致；已排队任务保留。
- 写操作覆盖 `task/create`、`task/cancel`、`approval/decide`、`agent/setCredential`、`agent/deleteCredential`、`task/integrate`、`session/create`、`context/compact`。这是阶段 4 的完整客户端面。
- 集成采用“精确提交 + 审查结论”双保险：`expectedCommit` 不匹配或验证失败时后台不会推进协作分支；压缩同理，`expectedRevision` 不匹配时拒绝写入并提示重新确认。
- 后台断开时保留最后一屏并提示，不再自动重连。

## 验证

```sh
cargo test  --locked --manifest-path collab/Cargo.toml --workspace
cargo test  --locked --manifest-path collab/Cargo.toml -p codex-collab-tui -- --include-ignored   # 需先构建 core
cargo clippy --locked --manifest-path collab/Cargo.toml --workspace --all-targets -- -D warnings
cargo fmt --manifest-path collab/Cargo.toml -p codex-collab-core -p codex-collab-tui --check
python3 collab/scripts/probe_terminal.py collab/target/debug/codex-collab-tui
python3 collab/scripts/probe_terminal_live.py collab/target/debug/codex-collab-tui collab/target/debug/codex-collab-core
```

- 单元测试覆盖行协议分类（响应/通知/错误/坏行）、假宿主往返、Agents→窗格映射、审批列表映射与参数摘要、审查表单校验（未完成/非编码/已集成/无提交均拒绝）、会话创建表单的验证命令解析与字段校验、压缩表单携带精确 revision、状态与协议标签、事件文本回退、UTF-8 安全的会话标签，以及 Demo/Live 两种模式的按键、编码开关、审查集成、审批决定、密钥遮罩提交/删除/取消与出站动作。
- 忽略测试 `real_host_drives_session_and_agent_listing` 用真实 `codex-collab-core` 走通 `session/create → agent/configure → agent/list → task/list`，确认 `approval/list` 可达、未知审批/任务 ID 明确失败，并验证 `context/compact` 成功写入检查点、空摘要与过期 revision 均被拒绝。
- 集成验证由后台测试承担：`core/tests/git.rs` 覆盖候选排除脏工作区、过期重校验、验证失败保留候选、冲突暂停不回退分支、未审查改动不可混入提交。
- 伪终端脚本 `<script>_live.py` 先用行协议播种真实会话与 Agent，再在 pty 中运行终端：断言渲染真实模型名、主控窗格、会话标签与协议；切换编码开关、触发审查/压缩/新建会话路由；打开密钥表单后输入假密钥并确认**未回显**；随后确认退出且终端模式与备用屏幕恢复。
