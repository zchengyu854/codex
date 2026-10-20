# 阶段 6 设计：从「方法可达」到「参数可达与真实验证」

日期：2026-10-19（Asia/Shanghai），实施记录更新于 2026-10-20。
状态：**6-1、6-2、6-3 已实现并验证**；6-4 待凭证信息，6-5 不纳入本阶段。

## 出发点

第 5 阶段把 19/19 个宿主方法接到了终端，那是**方法粒度**的可达。逐条核对客户端实际发出的参数后发现，仍有几处缺口——它们不是"少一个按键"，而是核心已经实现的能力在终端不可达，或从未被真实验证过。

| # | 缺口 | 证据 | 影响 |
| --- | --- | --- | --- |
| 1 | 任务参数不可达 | `collab/tui/src/client.rs` 的 `Action::Send` 恒发 `acceptance: ""`、`dependencies: []` | 验收标准永远为空（模型拿不到 `Acceptance`）；依赖门控（`queued`/`blocked`、编码任务需 `integrationStatus=integrated` 才满足下游）在终端完全不可达 |
| 2 | 会话只有启动时绑定 | `collab/tui/src/main.rs:29-31`（`--session` 或列表首个）；`session/list` 只在启动与重建连接时调用 | 多会话无法切换，`session/create` 之后也回不到旧会话 |
| 3 | 任务详情不成体系 | `task/get` 只在审查模态里间接用到一部分；`worktree`/`baseCommit`/`commit`/`contextVersion`/`integrationStatus` 没有集中视图 | 出问题时无法在终端判断"卡在哪一步"，只能读日志 |
| 4 | 真实调用从未验证 | `collab/evidence/stage-{2,4,5}-summary.json` 均 `real_api_tested: false`；live 探针任务因无凭证而失败（见 `stage-5-output-screen.txt`） | 全部"能力"结论建立在本地模拟之上，Keychain 读写也没做过交互式验证 |
| 5 | 结果模态仍是纯文本 | `ui.rs` 结果模态只渲染限长正文与附件路径 | 差异与附件无法在终端内快速判读 |

缺口 1 是本阶段价值最高的一项：它把核心已写好的编排能力（依赖、验收、阻塞原因）从"后台有"变成"终端可用"。

## 分段设计

### 6-1 任务编排可达（验收标准 + 依赖声明）

**范围**

- 新增任务编辑器（`Ctrl+F`「任务编排」；`Enter` 的快速派发路径保留，行为不变）。编辑器字段：多行目标、验收标准、依赖选择、编码开关。
- 按键说明：原计划的 `Ctrl+I` 不可用——终端里它与 `Tab` 同为 `0x09`，无法区分；改用 `Ctrl+F`，与其余模态的 `Ctrl+字母` 惯例一致。
- 依赖列表来自当前会话 `task/list`，只列出可选任务（自身、已完成且已集成的编码任务、运行中的不做默认勾选），并显示每个候选的 `status`/`integrationStatus`。
- 派发参数补齐：`acceptance`、`dependencies`；空验收在客户端给出提示（宿主允许为空，但这是常见误操作）。
- 窗格与任务行显示依赖与阻塞原因：`blocked` 时标注前置任务，`queued` 时标注"等待轮转"。

**按键约定（沿用既有规矩）**

- 编辑器内 `Enter` = 换行（多行目标），`Ctrl+S` = 提交；`Esc` = 取消。文本字段里 `Space` 只作正文，选择型字段才用 `Space` 切换——这是第 5 阶段约束表单定下的规矩，继续遵守。
- `↑`/`↓` 在字段间移动，`Tab` 在字段内元素间移动；提交前做一次宿主等价校验（端口、非空、依赖存在）。

**协议映射**：`task/create`（新增 `acceptance`、`dependencies`）、`task/list`、`task/get`（读取依赖与阻塞原因）。

**验收**

- 从终端能创建带验收标准与依赖的任务；被依赖任务未集成时，下游任务在宿主侧进入 `blocked`，终端显示原因。
- 依赖候选列表不包含自身与不可选任务；提交失败时表单不关闭且保留输入。
- 快速派发（`Enter`）行为与本阶段之前逐字节一致（回归断言）。

**证据**：表单字段映射与校验的单测、依赖筛选单测、`blocked` 渲染快照、live pty 探针（建两个任务 → 第二个带依赖 → 断言状态与提示）。

**实现记录（2026-10-20）**

已交付，全部落在终端客户端，宿主协议未改动：

| 能力 | 实现位置 | 说明 |
| --- | --- | --- |
| 共享光标缓冲 `TextBuf { text, cursor }` | `tui/src/app.rs` | 输入行、目标、验收共用一份光标算术；`Pane.input` 由 `String` 改为 `TextBuf`，兼容 `PartialEq<&str>` |
| `Ctrl+F` 任务编辑器 | `app.rs::open_task` / `task_key` | 需已配置窗格；打开时向宿主发 `task/list` 拉候选依赖 |
| 四字段编排 | `TaskForm { goal, acceptance, dependencies, index, coding, field, note }` | `Tab`/`BackTab` 循环 0 目标 → 1 验收 → 2 依赖 → 3 编码；`Esc` 取消 |
| 多行文本 | `task_edit` + `push_caret_field` | 编辑器内 `Enter` 插入换行（目标 ≤64000B、验收 ≤16000B）；`Space` 只是正文 |
| 依赖选择 | `task_toggle_dependency` / `push_dependencies` | 最多 8 项；`failed`/`cancelled`/`interrupted`/`blocked` 的候选不可勾选并提示「勾选后新任务会直接阻塞」；编码任务完成但未集成标「等待集成」 |
| 派发 | `submit_task` → `Action::CreateTask` → `client.rs::dispatch_task` | `acceptance` 必填（空则留表单并提示走 `Enter` 快速派发）；`Action::Send` 与 `Action::CreateTask` 共用同一线上形状 |
| 阻塞事件 | `client.rs` `task_blocked` | 窗格追加 `[依赖] 任务已阻塞：<宿主原因>`（`WARN` 色） |
| 键位与画面 | `ui.rs` 模态 + 两行底部提示 | 依赖窗口固定 8 行；光标用内联 `❙` 标记（不改终端光标），越界时夹取到字符边界 |

与设计的差异只有一处：打开键从 `Ctrl+I` 改为 `Ctrl+F`——`Ctrl+I` 在终端里就是 `0x09`，与 `Tab` 不可区分，无法作为独立绑定；`Ctrl+S` 提交保持原样（raw 模式已关闭 `IXON`）。`Enter` 快速派发路径逐字节未变，由 `task_editor_enter_inserts_newlines_and_space_stays_text` 与既有派发断言共同守住。

验证：workspace 104 项全绿（core 32 / tui 72，另 2 项 opt-in 忽略项单跑通过）、clippy `-D warnings`、fmt `--check`、demo 与 live pty 探针均通过。live 探针在编辑器里输入两行目标、一行验收、打开编码开关后 `Ctrl+S`，退出 TUI 后重新用宿主读回该任务，断言 `goal` 含换行、`acceptance` 与 `coding` 逐字一致——参数确实到达了 `task/create`，而不只是"界面看起来能填"。

证据文件：`evidence/stage-6-tests.log`（一条命令重放）、`evidence/stage-6-task-screen.txt`（编辑器画面，含依赖行与 `❙` 光标）、`evidence/stage-6-summary.json`；重放脚本 `scripts/stage6_evidence.sh`。


### 6-2 会话管理

**范围**

- 会话列表模态（`session/list`）：`id` 短前缀、`repository`、`branch`、`baseCommit`、验证命令条数，以及当前会话标记。
- 切换会话（重建连接语义：重读 `agent/list`、`task/list`、`event/list`、清空历史与 `task_status`），并在切换后重载共享记录。
- 每会话的 `requirement`（`event/publish` 的 `requirement` 记录）在会话列表与共享记录里可见；用户约束不因切换而丢失。
- 新建会话复用现有 `Ctrl+N` 表单；切换与新建共用同一份重载路径。

**协议映射**：`session/list`、`agent/list`、`task/list`、`event/list`。

**验收**

- 两个会话间来回切换，窗格、任务、共享记录、命令历史（按窗格隔离）都正确重载，不串数据。
- 会话列表对空会话、缺字段、超长子路径均安全渲染。

**证据**：切换重载单测（含串数据反例）、会话列表空/满渲染快照、live pty 探针（`Ctrl+N` 建第二个会话 → 列表切换 → 断言窗格与记录切换）。

**实现记录（2026-10-20）**

已交付，宿主协议未改动（只修正了客户端一处入参）：`session/list` 本来就能返回全部持久化会话，缺的是终端入口与切换语义。

| 能力 | 实现位置 | 说明 |
| --- | --- | --- |
| 会话列表 `Ctrl+L` | `app.rs::open_sessions` / `sessions_key` | 仅实时模式；打开时发 `session/list`，光标默认落在当前会话 |
| 列表内容 | `ui.rs` 会话模态 + `client.rs::session_items` | 每行：短 id、验证命令条数、当前标记；详情：仓库、分支、基线提交、用户约束摘要。缺字段退化为 `—`，长路径按字符边界截断 |
| 每会话用户约束 | `client.rs::requirement_lines` | 打开列表时为每个会话读一次 `event/list`（after=0，最多 1000 条），取最近 3 条 `requirement`/`user_override`；结果按会话缓存，发布新约束后只失效当前会话。行内也可滚动 8 行窗口 |
| 切换 | `Action::SwitchSession` → `Client::attach` | 与启动、`session/create` 共用同一条重载路径 |
| 切换时的清理 | `Client::attach` + `App::reset_session_view` | 清空 pending（旧会话的迟到响应直接失效，不会弹出旧 `task/output`）、`after`、任务映射、产物、探针与轮询；窗格、共享记录、审批、各模态、命令历史全部重置；焦点/放大/共享面板/编码开关属于全局选择，保留 |
| 新建复用 | `Action::CreateSession` → `attach` | 创建后直接进入新会话；顺带修正：需求为空时不再发送 `requirement` 字段，避免宿主落一条空约束、在列表里显示成「用户约束 1 条」 |
| 键位 | 第一行底部提示 | `Ctrl+L 会话` 与 `Ctrl+A 配置` 同属全局键；两行提示在 132 列内不被截断（有回归断言） |

「不串数据」的做法是把清理放在 `attach` 里而不是调用点，因此启动、切换、新建三条入口不可能漏掉某一步。`App::reset_session_view` 单独可测：`reset_session_view_drops_previous_session_data` 先灌入共享记录、审批、旧窗格与历史，再断言全部清空且全局选择仍在。

验证：workspace 112 项全绿（core 32 / tui 80，另 2 项 opt-in 忽略项单跑通过）、clippy `-D warnings`、fmt `--check`、demo 与 live 探针通过。live 探针在真实宿主上建第二个会话 → 断言新会话窗格为空、共享记录无旧内容 → `Ctrl+L` 确认「共 2 个会话」→ `↑` 选回原会话并切换 → 断言模型名与两条用户记录重新出现；退出后再用宿主核对：两个会话互不共享 Agent/任务/用户事件，原会话的 requirement 仍在。demo 探针新增断言：`Ctrl+F`/`Ctrl+L` 在演示模式不生效。

证据文件：`evidence/stage-6-tests.log`、`evidence/stage-6-sessions-screen.txt`（三个会话的列表画面，含当前标记与约束摘要）、`evidence/stage-6-summary.json`。


### 6-3 任务详情与结果增强

**范围**

- 任务详情模态（`task/get`）：`status`、`contextVersion`、`worktree`、`baseCommit`、`commit`、`integrationStatus`、依赖与阻塞原因、结果摘要与附件清单；只读宿主已落盘内容。
- 结果模态增强：附件内联预览（限长沿用 100000 字节）、最小差异高亮（`+`/`-`/`@@` 着色，不做语法解析）。
- 详情与结果共享同一只读附件读取助手（第 5 阶段已有的 `read_attachment`），不新增路径来源，不接受用户提供的路径。

**协议映射**：`task/get`、`task/output`。

**验收**

- 详情字段与宿主返回值逐条对应；缺失字段显示为空而不是猜测。
- 差异高亮对空文件、无换行结尾、非 UTF-8 边界、超长行均不 panic，且渲染宽度不越界。

**证据**：详情字段映射单测、高亮边界单测、详情/结果两张快照。

**实现记录（2026-10-20）**

- **详情模态 `Ctrl+D`**（仅实时模式）：按窗格取当前任务，发 `task/get`，字段一一对应——状态、编码、`contextVersion`、工作区、`baseCommit`、`commit`、`integrationStatus`（映射为「未集成 / 已集成 / 存在冲突 / 集成失败 / 集成中断」）、目标、验收标准、依赖（短 id + 最近一次 `task/list` 里的状态）、阻塞原因、结果摘要、附件清单与完整结果附件。缺失字段显示 `—` 或「（空）」，不推断。`PgUp`/`PgDn` 滚动，`Ctrl+R` 重新读快照，`Esc` 关闭。
- **阻塞原因**：宿主不把原因写进任务行，只在 `task_blocked` 事件里给一次。客户端现在把最近一次原因按任务 id 记住（`Client::blocked_reason`，切换会话时清空），详情里照原文展示；没收到过事件就留空。
- **结果模态**：新增本机只读的 `file diff` 附件预览（与「完整结果」同一读取助手、同一 100000 字节上限），差异行按 `@@` / `+++` / `---` / `+` / `-` 着色；审查模态改用同一段着色逻辑，超长差异仍按 300 行截断并注明总行数。不做语法解析，不猜路径。
- **边界**：`push_diff` 逐行推入，空文本、无换行结尾、CJK、1 万字符单行都只交给渲染层换行；非 UTF-8 附件在读取处即失败并显示为空，不会进入着色。

验证：workspace 117 项全绿（core 32 / tui 85）、clippy `-D warnings`、fmt `--check`、双探针通过。live 探针在真实宿主上对新任务按 `Ctrl+D`，断言详情里出现编辑器提交的验收标准与只有 `task/get` 才有的「上下文版本」，证明详情不是从 `task/list` 拼出来的。证据：`evidence/stage-6-detail-screen.txt`（真实 `task/get` 往返，失败任务的工作区/基线/候选提交均为 `—`，结果摘要为宿主原文）、`evidence/stage-6-tests.log`。真实差异高亮目前只有单测与渲染用例覆盖——本机没有可用凭证，跑不出带 `file diff` 附件的编码任务，这条留到 6-4 用真实调用补齐。

### 6-4 真实 API 与 Keychain 验证（唯一的能力声明缺口）

**范围**

- 新增 opt-in 证据脚本（沿用 `stage5_evidence.sh` 的结构）：无凭证时**明确 skip 而非 fail**，让默认回归保持离线可复现。
- 两条路径分别验证：
  1. **Keychain 往返**：经宿主 `agent/setCredential` → `agent/deleteCredential` 写入/删除临时账户，并确认无读取明文接口；结束后清理，不留仓库痕迹。
  2. **真实探针**：配置指向真实服务商的 Agent，走 `agent/testConnection` → `agent/connectionResult`，要求 `verified=true` 且拿到 `text`/`stream`/`function_tools`；随后跑一个最小任务（自述 + `collab_publish`），确认流式增量、用量与结果落盘。
- 失败分支必须如实记录：无凭证、401、429（重试语义）、网络中断、地址被拒（HTTPS/回环规则各留一条）。

**安全边界**

- 密钥只经受控标准输入，绝不进命令行、日志、快照或证据文件；证据里只留"能力列表 + 脱敏后的错误类别"。
- 探针会产生费用，默认跳过；费用提示写在脚本与文档里。
- 不把真实 Keychain 项或服务商地址写进仓库；需要用户提供的信息只以占位符出现在文档中。

**验收**：脚本在（a）无凭证、（b）有凭证两种环境下都能给出确定结论；有凭证时 `real_api_tested` 才能翻成 `true`，并同时更新三份 summary 的该字段与说明。

**待用户输入**：服务商与端点、模型名、`responses`/`chat_completions`、是否允许产生费用、由谁在 TUI 内经 `Ctrl+K` 写入 Keychain。

### 6-5（可选）打包与发布

release 构建与版本说明、安装步骤、README 快速开始、`--demo` 之外的最小可运行示例。不影响前四段，放最后，视需要决定是否纳入。

## 顺序与依赖

- 6-1 与 6-2 相互独立，建议先 6-1（价值最高）；6-3 依赖 6-1（详情要呈现依赖与阻塞原因）；6-4 可并行准备，但必须等用户提供凭证才能产出 `true` 的证据；6-5 可选、最后。
- 每段沿用既有节奏：`--locked` workspace 测试 + tui `--include-ignored` + clippy `-D warnings` + fmt `--check` + demo/live pty 探针 + 快照 → 更新文档与证据 → 提交并推送 `origin`（**不建 PR**）。
- 新增 `collab/scripts/stage6_evidence.sh`，结构复制第 5 阶段脚本，一条命令重放全部验证并重写 `evidence/stage-6-tests.log`。

## 已确认的决策

1. 顺序按 **6-1 → 6-2 → 6-3 → 6-4** 推进；6-4 需要用户提供凭证信息才能产出 `real_api_tested: true` 的证据。
2. 6-5 不纳入本阶段。
3. 6-1 的按键：`Ctrl+I` 与 `Tab` 字节冲突，已改为 `Ctrl+F` 打开任务编辑器；编辑器内 `Enter` 换行 / `Ctrl+S` 提交不变。
4. 编辑器要求验收标准非空；「无验收」保留给窗格内 `Enter` 的快速派发路径，避免空验收成为默认习惯。
