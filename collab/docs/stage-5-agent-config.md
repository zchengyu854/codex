# 阶段 5：补齐终端契约覆盖

状态：第一段（Agent 配置表单）完成并验证。其余段落见文末“后续段落”。

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

## 运行

```sh
cargo build --locked --manifest-path collab/Cargo.toml -p codex-collab-core
cargo run  --locked --manifest-path collab/Cargo.toml -p codex-collab-tui -- \
  --state /absolute/path/to/state --session <会话 ID>
```

在终端内：`Ctrl+N` 新建会话 → `Tab` 选窗格 → `Ctrl+A` 配置该窗格的 Agent（并 `Ctrl+K` 设置密钥）→ 输入 Enter 派发任务。

## 协议映射（新增部分）

| 客户端行为 | 后台方法 | UI 效果 |
| --- | --- | --- |
| Ctrl+A | —（本地打开表单） | 配置模态显示 id、角色与预填字段 |
| 配置模态 Enter | `agent/configure` | 成功则刷新四窗格；失败回显宿主错误 |
| `agent/list` | —（响应） | 窗格额外记录 `contextBytes` 并在状态行显示预算 |

## 安全与边界

- `id` 与角色不可手填，避免跨会话迁移 Agent 或改变角色；宿主 `store.configure` 对此有独立校验，客户端只是不给用户制造非法输入。
- Base URL 的本地校验是**友好提示**，不是安全边界：宿主 `Agent::validate` 仍是唯一权威；客户端不会向未通过本地校验的地址提交请求。
- 配置 Agent 是宿主已支持的写操作，未新增任何远程或文件访问面；终端仍只经行协议交互。
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

- 单元测试新增：表单提交与字段映射、空模型/非法 URL/越界预算的拒绝与焦点跳转、协议切换与字段循环、Esc 取消、按字段清洗的粘贴；`build_agent_form` 对空窗格生成合法 UUID、对已有 Agent 预填 id/角色/模型/协议/预算；Base URL 校验与宿主规则逐条对照；模态渲染与底部键位提示。
- 真实宿主冒烟 `real_host_drives_session_and_agent_listing` 增补：`agent/list` 返回 `contextBytes`；非 UUID 的 `id` 与越界 `contextBytes` 被宿主明确拒绝。
- 伪终端脚本 `probe_terminal_live.py` 增补：`Ctrl+A` 在已配置窗格预填现有 Agent（编辑路径）；`Tab` 到工作窗格后用表单新增 Agent，客户端生成 UUID、宿主校验端点，窗格随后渲染新模型名与预算。
- 本次工作区回归 **81 passed / 0 failed / 2 ignored**（core 32 / tui 49）；tui `--include-ignored` **50 passed**（含真实宿主冒烟）；Clippy（警告视为错误）与格式检查通过；demo 与 live 伪终端冒烟通过。原始日志见 `../evidence/stage-5-tests.log`，结构化结果见 `../evidence/stage-5-agent-config-summary.json`。

## 后续段落（未开始）

1. **连接自检**：`agent/testConnection` + `agent/connectionResult`，在终端验证某 Agent 的文本/流式/函数工具能力。
2. **用户约束**：`event/publish`，从终端发布 `requirement`/`user_override`/`suggestion`。
3. **结果查看**：`task/output`，查看任务持久化结果摘要与运行中公开草稿（完整结果在附件文件）。
4. **输入编辑增强**：光标编辑与命令历史（当前仅末尾追加与退格）。
