# 阶段 5：补齐终端契约覆盖

状态：第一段（Agent 配置表单）与第二段（连接自检）完成并验证。其余段落见文末“后续段落”。

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

## 运行

```sh
cargo build --locked --manifest-path collab/Cargo.toml -p codex-collab-core
cargo run  --locked --manifest-path collab/Cargo.toml -p codex-collab-tui -- \
  --state /absolute/path/to/state --session <会话 ID>
```

在终端内：`Ctrl+N` 新建会话 → `Tab` 选窗格 → `Ctrl+A` 配置该窗格的 Agent（并 `Ctrl+K` 设置密钥）→ `Ctrl+T` 自检 → 输入 Enter 派发任务。

## 协议映射（新增部分）

| 客户端行为 | 后台方法 / 通知 | UI 效果 |
| --- | --- | --- |
| Ctrl+A | —（本地打开表单） | 配置模态显示 id、角色与预填字段 |
| 配置模态 Enter | `agent/configure` | 成功则刷新四窗格；失败回显宿主错误 |
| `agent/list` | —（响应） | 窗格额外记录 `contextBytes` 并在状态行显示预算 |
| Ctrl+T | `agent/testConnection` | 排队探针任务并写入窗格 |
| 探针轮询 | `agent/connectionResult`（900ms，≤100 次） | 提示行进度；终态写入窗格、刷新列表 |

## 安全与边界

- `id` 与角色不可手填，避免跨会话迁移 Agent 或改变角色；宿主 `store.configure` 对此有独立校验，客户端只是不给用户制造非法输入。
- Base URL 的本地校验是**友好提示**，不是安全边界：宿主 `Agent::validate` 仍是唯一权威；客户端不会向未通过本地校验的地址提交请求。
- 连接自检会**真实调用模型并可能产生费用**，提示行明确写出这一点；能力判定字段全部来自后台，客户端不自行推断。
- 配置 Agent 与连接自检都只用宿主已支持的方法，未新增远程或文件访问面；终端仍只经行协议交互。
- 运行中的任务不能被重配（宿主拒绝），表单只做提示，不代为取消。

## 验证

```sh
cargo test   --locked --manifest-path collab/Cargo.toml --workspace
cargo test   --locked --manifest-path collab/Cargo.toml -p codex-collab-tui -- --include-ignored  # 需先构建 core
cargo clippy --locked --manifest-path collab/Cargo.toml --workspace --all-targets -- -D warnings
cargo fmt    --manifest-path collab/Cargo.toml -p codex-collab-core -p codex-collab-tui --check
python3 collab/scripts/probe_terminal.py collab/target/debug/codex-collab-tui
python3 collab/scripts/probe_terminal_live.py collab/target/debug/codex-collab-tui collab/target/debug/codex-collab-core
```

- 单元测试新增：配置表单的提交与字段映射、空模型/非法 URL/越界预算的拒绝与焦点跳转、协议切换与字段循环、Esc 取消、按字段清洗的粘贴；`build_agent_form` 对空窗格生成合法 UUID、对已有 Agent 预填 id/角色/模型/协议/预算；Base URL 校验与宿主规则逐条对照；连接自检的排他性（未配置窗格拒绝）、`probe_outcome` 对进行中/通过/未通过/待调度的分支；模态渲染与底部键位提示。
- 真实宿主冒烟 `real_host_drives_session_and_agent_listing` 增补：`agent/list` 返回 `contextBytes`；非 UUID 的 `id` 与越界 `contextBytes` 被拒绝；`agent/testConnection` 返回探针任务且 `connectionResult` 在无执行时返回 `verified=false` 与空能力列表；非探针任务与未知任务均被 `agent/connectionResult` 拒绝。响应辅助函数改为跳过 `collab/update` 通知，避免任务通知与响应乱序造成误判。
- 伪终端脚本 `probe_terminal_live.py` 增补：`Ctrl+A` 在已配置窗格预填现有 Agent（编辑路径）；`Tab` 到工作窗格后用表单新增 Agent（客户端生成 UUID、宿主校验端点，窗格渲染新模型与预算）；`Ctrl+T` 排队连接探针并确认客户端上报排队。
- 本次工作区回归 **83 passed / 0 failed / 2 ignored**（core 32 / tui 51）；tui `--include-ignored` **52 passed**（含真实宿主冒烟）；Clippy（警告视为错误）与格式检查通过；demo 与 live 伪终端冒烟通过。原始日志见 `../evidence/stage-5-tests.log`，结构化结果见 `../evidence/stage-5-summary.json`。

## 后续段落（未开始）

1. **用户约束**：`event/publish`，从终端发布 `requirement`/`user_override`/`suggestion`。
2. **结果查看**：`task/output`，查看任务持久化结果摘要与运行中公开草稿（完整结果在附件文件）。
3. **输入编辑增强**：光标编辑与命令历史（当前仅末尾追加与退格）。
