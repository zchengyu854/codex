# 阶段 5：补齐终端契约覆盖

状态：第一段（Agent 配置表单）、第二段（连接自检）、第三段（用户约束发布）与第四段（任务结果查看）完成并验证；`docs/core-api.md` 的 19 个方法已全部接入。剩余段落见文末“后续段落”。

## 背景

阶段 4 结束时，`codex-collab-tui` 只调用了 `docs/core-api.md` 19 个方法中的 14 个。缺的 5 个方法分属四组能力，其中 `agent/configure` 是唯一的功能性断点：新建会话后无法在终端里配置四个 Agent，必须先借助外部客户端或裸协议，终端内的“建会话 → 配 Agent → 派任务 → 审查集成”闭环是断的。本阶段按优先级逐段补齐。

## 第一段：Agent 配置表单（`agent/configure`）

- `Ctrl+A`：为**焦点窗格**打开 Agent 配置表单。角色由窗格位置决定（窗格 1 为主控 `coordinator`，窗格 2–4 为 `worker`），与 `agent/list` 的窗格映射一致，用户无需也不能手填角色。
- 表单字段：模型、Base URL、协议、`contextBytes`。`id` 与角色只读显示：
  - 窗格尚无 Agent → 客户端用 `uuid::Uuid::new_v4()` 生成新 id（宿主强制校验 `agent.id` 必须是 UUID，见 `core/src/runtime.rs`）；
  - 窗格已有 Agent → 复用其 id，并从窗格当前值预填模型、Base URL、协议与 `contextBytes`（`agent/list` 返回 `contextBytes`，现存入 `Pane::context_bytes` 供预填与展示）。
- 交互：`Tab`/`↑↓` 切换字段；协议字段用 `←`/`→`/空格在 `responses` 与 `chat_completions` 之间切换（不接受逐字输入）；`Enter` 提交；`Esc` 取消。粘贴按字段清洗（换行/制表转空格、去控制字符；`contextBytes` 只收数字）。
- 本地预校验（提交前给出中文提示，宿主仍为权威）：
  - 模型 1..200 字节；
  - `contextBytes` 为 4096..1000000 的整数；
  - Base URL 镜像 `core/src/model.rs::Agent::validate`：仅 `https`，或环回地址（`localhost`/`127.0.0.1`/`[::1]`）上的 `http`；不含账号、密码、查询串或片段。
- 提交走宿主 `agent/configure`，成功后重新拉取 `agent/list` 刷新四窗格；宿主校验失败（如“coordinator already exists”“stop current task before reconfiguring agent”）原样回显到底部提示行，不静默失败。
- 窗格状态行新增预算显示：`<Base URL> / <协议> · <contextBytes> 字节预算`，便于确认当前端点的上下文预算。

## 第二段：连接自检（`agent/testConnection` / `agent/connectionResult`）

- `Ctrl+T`：对焦点 Agent 排队一个能力探针（`agent/testConnection`）。后台随即创建一个普通任务，要求“先调用 `collab_publish(kind=result, data=connection_probe_ok)`，再回答 `connection_probe_ok`”。探针是**真实模型调用**，需要该 Agent 有可用密钥；它占用该 Agent 的任务槽，忙碌时排队。未配置 Agent 的窗格会被直接拒绝并提示。
- 结果轮询：客户端记下探针任务 id，按固定节奏（900ms，最多 100 次）调用 `agent/connectionResult`，直到任务进入 `completed`/`failed`/`cancelled`；非终态只在提示行显示进度（`连接自检中（工作中）…`）。宿主的 `task_changed` 通知同样触发刷新。轮询有上限，超时提示“仍在进行（任务可能排队）”，不会无限轮询；宿主报错（如任务不存在）时立即停止轮询。
- 判定：**只有**“任务完成 + 确实收到探针工具结果（`result=connection_probe_ok` 事件）+ 有流式用量（`usage` 事件）”才算 `verified=true`，能力列表为 `text`/`stream`/`function_tools`。其余一律按未通过呈现，不谎报成功：
  - `completed` 且未验证 → “未通过：没有收到探针工具结果或流式用量”；
  - `failed`/`cancelled` → “未通过（状态 …）”。
- 显示：排队时写入窗格 `[自检] 已排队连接探针任务 <id>`；结束后写入 `[自检] 通过：…`（绿）或 `[自检] 未通过：…`（黄）。探针成为该 Agent 的最新任务，因此状态行会同步“排队/工作中/已完成”，也能被 `Ctrl+C` 取消。

## 第三段：用户约束发布（`event/publish`）

- `Ctrl+U`：打开用户约束表单。类型在 `requirement`（需求）、`user_override`（用户覆盖）、`suggestion`（建议）之间用 `←`/`→`/`Tab` 切换，`◉` 标出当前项；内容为单行文本（≤4096 字节）；`Enter` 发布，`Esc` 取消。空格是内容的一部分，**不会**切换类型。
- 语义：`requirement` 与 `user_override` 是权威记录，宿主会把它们持续注入**每一次**模型请求，并优先于模型决策；`suggestion` 仅作提示。表单提示行与模态标题都写明这一点。
- 提交走宿主 `event/publish`；返回 `revision` 后立即触发 `event/list` 拉取，因此发布内容会以 `[用户] …` / `[建议] …` 出现在共享记录（`F3` 查看）。
- 本地只拦空内容（提示“先填写内容，再发布”）。类型白名单由宿主强制（`user publications must be requirement, user_override, or suggestion`），客户端不复制该校验，只在类型选择器里只提供合法值。
- 粘贴按内容清洗：换行/制表转空格，去控制字符，超限提示“约束上限为 4096 字节”。

## 第四段：任务结果查看（`task/output`）

- `Ctrl+O`：查看**焦点窗格最新任务**的输出。窗格没有 Agent 或还没有任务时只给提示，不发请求。
- 模态内容分四块：
  - `结果摘要（持久化）`：`task/output` 响应的 `result` 字段，即宿主在任务终态时落库的摘要（最长 16000 字节，超限带 `[truncated]`）。任务失败时这里就是失败原因。
  - `运行中公开草稿`：响应的 `draft.text`，宿主内存中正在流式生成的公开消息；任务结束后该字段为 `null`。它与窗格内实时流式显示的是同一份内容，此处用于在任务结束后回看最后一段。
  - `完整结果（附件，本机只读）`：客户端按 `artifact` 事件拿到的 `full result` 附件路径，**本地只读**读取并限长 100000 字节后显示；附件在任务结束前不存在，所以运行中为空。
  - `产物`：该任务的附件清单（标签 · 字节数 · 路径），让用户能在终端外打开完整文件。
- 自动刷新：任务处于 `queued`/`running` 时，客户端每 800ms 重新拉取一次 `task/output`，草稿随之滚动更新；宿主把任务置为终态后，下一次刷新读到附件、停止轮询，模态底部提示从“运行中，自动刷新”变为静态提示。任务 id 不在当前 `task/list` 快照里（例如尚未刷新）时按“已结束”处理，只拉一次。
- 交互：`PgUp`/`PgDn` 滚动（顶部截断），`Ctrl+R` 手动重取，`Esc` 关闭；关闭时清空轮询定时器。宿主报错（如任务不存在）会停止轮询并在提示行回显宿主信息。
- 读取附件沿用 `file diff` 审查的既有模式：客户端**只读**本机文件，不回写、不移动、不解释内容，也不把内容作为请求参数发给宿主。

## 运行

```sh
cargo build --locked --manifest-path collab/Cargo.toml -p codex-collab-core
cargo run  --locked --manifest-path collab/Cargo.toml -p codex-collab-tui -- \
  --state /absolute/path/to/state --session <会话 ID>
```

在终端内：`Ctrl+N` 新建会话 → `Tab` 选窗格 → `Ctrl+A` 配置该窗格的 Agent（并 `Ctrl+K` 设置密钥）→ `Ctrl+T` 自检 → 输入 Enter 派发任务；`Ctrl+U` 可随时发布用户约束，`Ctrl+O` 查看任务结果，`F3` 查看共享记录。

## 协议映射（新增部分）

| 客户端行为 | 后台方法 / 通知 | UI 效果 |
| --- | --- | --- |
| Ctrl+A | —（本地打开表单） | 配置模态显示 id、角色与预填字段 |
| 配置模态 Enter | `agent/configure` | 成功则刷新四窗格；失败回显宿主错误 |
| `agent/list` | —（响应） | 窗格额外记录 `contextBytes` 并在状态行显示预算 |
| Ctrl+T | `agent/testConnection` | 排队探针任务并写入窗格 |
| 探针轮询 | `agent/connectionResult`（900ms，≤100 次） | 提示行进度；终态写入窗格、刷新列表 |
| Ctrl+U | —（本地打开表单） | 约束模态显示类型选择器与内容 |
| 约束模态 Enter | `event/publish` | 提示行显示版本号，随即 `event/list` 把记录写入共享记录 |
| Ctrl+O | `task/output` | 结果模态显示持久化摘要、运行中草稿与附件清单 |
| 结果模态轮询 | `task/output`（800ms，任务运行中） | 草稿随刷新更新；终态后读取 `full result` 附件并停止轮询 |
| 任务结束 | `artifact` 通知（`event/list`） | 记录 `full result` / `file diff` 附件路径，供结果查看与差异审查**本地只读**读取 |

## 安全与边界

- `id` 与角色不可手填，避免跨会话迁移 Agent 或改变角色；宿主 `store.configure` 对此有独立校验，客户端只是不给用户制造非法输入。
- Base URL 的本地校验是**友好提示**，不是安全边界：宿主 `Agent::validate` 仍是唯一权威；客户端不会向未通过本地校验的地址提交请求。
- 连接自检会**真实调用模型并可能产生费用**，提示行明确写出这一点；能力判定字段全部来自后台，客户端不自行推断。
- 用户约束一旦发布就会进入每次模型请求，**不能从终端撤回**；这是宿主的设计（记录只增不改），客户端因此把类型与语义写在模态里，且在发布前不预置任何正文。
- 类型白名单、内容长度与注入时机全部由宿主决定；客户端不把用户输入拼进任何命令或路径，文本只作为 `event/publish` 的 `data` 字段传输。
- 配置 Agent 与连接自检都只用宿主已支持的方法，未新增远程或文件访问面；终端仍只经行协议交互。
- 运行中的任务不能被重配（宿主拒绝），表单只做提示，不代为取消。
- 结果查看只读宿主**已落盘**的附件（`full result`、`file diff`），且限长 100000 字节；路径来自宿主的 `artifact` 事件，客户端不接受用户提供的路径，也不遍历目录。终端的写权限仍仅限宿主自身的状态写入。

## 验证

```sh
cargo test   --locked --manifest-path collab/Cargo.toml --workspace
cargo test   --locked --manifest-path collab/Cargo.toml -p codex-collab-tui -- --include-ignored  # 需先构建 core
cargo clippy --locked --manifest-path collab/Cargo.toml --workspace --all-targets -- -D warnings
cargo fmt    --manifest-path collab/Cargo.toml -p codex-collab-core -p codex-collab-tui --check
python3 collab/scripts/probe_terminal.py collab/target/debug/codex-collab-tui
python3 collab/scripts/probe_terminal_live.py collab/target/debug/codex-collab-tui collab/target/debug/codex-collab-core
collab/scripts/stage5_evidence.sh   # 重放以上全部步骤并重写 evidence/stage-5-tests.log
```

- 单元测试新增：配置表单的提交与字段映射、空模型/非法 URL/越界预算的拒绝与焦点跳转、协议切换与字段循环、Esc 取消、按字段清洗的粘贴；`build_agent_form` 对空窗格生成合法 UUID、对已有 Agent 预填 id/角色/模型/协议/预算；Base URL 校验与宿主规则逐条对照；连接自检的排他性（未配置窗格拒绝）、`probe_outcome` 对进行中/通过/未通过/待调度的分支；约束表单的类型前后循环与回绕、空内容拒绝、**空格属于正文而非切换键**、粘贴清洗与超限、Esc 取消；结果查看的打开动作、PgUp/PgDn 滚动与顶部截断、`Ctrl+R` 重取、Esc 关闭；四个模态的渲染与底部键位提示。
- 底部键位提示拆成两行后新增宽度回归：以 132 列演示帧渲染，断言 `Ctrl+A 配置`…`Ctrl+Q 退出` 全部完整可见（`Ctrl+Q` 移到第一行，第二行改为纯任务键位）；小尺寸遍历新增结果模态，覆盖 `scroll = usize::MAX` 的截断路径。
- 真实宿主冒烟 `real_host_drives_session_and_agent_listing` 增补：`agent/list` 返回 `contextBytes`；非 UUID 的 `id` 与越界 `contextBytes` 被拒绝；`agent/testConnection` 返回探针任务且 `connectionResult` 在无执行时返回 `verified=false` 与空能力列表；非探针任务与未知任务均被 `agent/connectionResult` 拒绝；`event/publish` 返回递增的 `revision`，且 `decision` 等非用户类型被拒绝；`task/output` 恒返回 `taskId`/`result`/`draft` 三键（`draft` 为 `null` 或带 `text`），未知任务被拒绝。响应辅助函数改为跳过 `collab/update` 通知，避免任务通知与响应乱序造成误判。
- 伪终端脚本 `probe_terminal_live.py` 增补：`Ctrl+A` 在已配置窗格预填现有 Agent（编辑路径）；`Tab` 到工作窗格后用表单新增 Agent（客户端生成 UUID、宿主校验端点，窗格渲染新模型与预算）；`Ctrl+T` 排队连接探针并确认客户端上报排队；`Ctrl+O` 打开探针任务的结果模态并断言标题渲染，`Esc` 关闭；`Ctrl+U` 切换类型到 `suggestion`、输入含空格文本并发布，再按 `F3` 确认共享记录出现 `[建议] …`——这一条同时锁住“类型选择器 → 事件 kind”与“空格不被吞掉”两个行为。
- 本次工作区回归 **89 passed / 0 failed / 2 ignored**（core 32 / tui 57）；tui `--include-ignored` **58 passed**（含真实宿主冒烟）；Clippy（警告视为错误）与格式检查通过；demo 与 live 伪终端冒烟通过。原始日志见 `../evidence/stage-5-tests.log`，结构化结果见 `../evidence/stage-5-summary.json`；日志由 `scripts/stage5_evidence.sh` 一条命令重放（构建 → 两组测试 → clippy → fmt → 两个 pty 冒烟 → 两张快照）。
- 界面留证：`scripts/snapshot_terminal.py` 把伪终端画面重建成文本快照（终端只按差分输出单元格，原始字节省略大量已存在的空格，无法直接判读）。演示画面快照见 `../evidence/stage-5-demo-screen.txt`，结果模态快照见 `../evidence/stage-5-output-screen.txt`（live 模式跑出真实的探针任务失败结果）；live 模式可用 `--keys` 驱动到任意模态再截图。
- 快照器本身的一条宽字形修正：写双宽字符的第二格时会清理**上一帧**遗留的“首格”标记，否则下一次写入会把新字符的续格清成空格（表现为 CJK 词中间多出一个空格）。结果模态标题因此从 `摘 要` 恢复为 `摘要`。

## 后续段落（未开始）

1. **输入编辑增强**：光标编辑与命令历史（当前仅末尾追加与退格）。
