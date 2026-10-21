# Codex 多模型协作项目

后续交付目标为可安装的 macOS 桌面 App，提供项目/会话导航、聊天、代码审查、内置终端和多 Agent 协作。当前已有 Rust 协作核心与 TUI，桌面界面尚未实现。总开发顺序和各阶段验收见[桌面 App 总体开发流程](docs/desktop-app-roadmap.md)。

基于 Codex 的独立扩展项目，与 RoboPi 分开。上游提交固定于 `upstream.json`，开发分支为 `codex/multi-model-desktop`。目前完成源码/接口验证、Rust 协作核心、终端壳层演示、终端接入协作后台的四个段落（真实会话/Agent/任务显示、公开文本流、操作审批与密钥提交、编码开关与差异审查集成、会话创建与上下文压缩）、阶段 5 的五段（Agent 配置、连接自检、用户约束、任务结果查看、输入行光标编辑与命令历史），阶段 6 的四段（任务编排：目标/验收标准/依赖/编码；会话列表与切换；任务详情与差异高亮；真实服务商与 Keychain 验证），以及阶段 7（真实多 Agent 派发与依赖门；宿主兜底与提交归属；并发、冲突与队列卫生；证据汇总，按实测暂缓可选审批策略）。现已进一步完成阶段 8-1、8-2 的本地代码项目协作与恢复验收。界面为单终端多窗格。

- [桌面 App 总体开发流程：D0–D7、首版范围与验收](docs/desktop-app-roadmap.md)
- [原后续开发计划：阶段 8–11 的拆分与验收约束](docs/development-roadmap.md)
- [第 1 阶段：基线与接口验证](docs/stage-1.md)
- [第 2 阶段：协作核心及验收](docs/stage-2.md)
- [终端壳层：运行与操作](docs/stage-3-terminal.md)
- [阶段 4：终端接入协作后台](docs/stage-4-host-client.md)
- [阶段 5：补齐终端契约覆盖](docs/stage-5-coverage.md)
- [阶段 6：参数可达与真实验证（6-1 至 6-4 已实现并验证）](docs/stage-6-plan.md)
- [阶段 7：真实多 Agent 协作（已收尾，含验证范围与限制）](docs/stage-7-plan.md)
- [阶段 8：代码项目验收（8-1、8-2 本地通过，真实模型验收待续）](docs/stage-8-plan.md)
- [后台接口与行为约定](docs/core-api.md)

## 当前能力

一个主 Agent 和三个工作 Agent 可分别配置 API、模型及协议，共享需求、决策、任务状态和产物。每个任务使用独立的 Codex 执行会话和 Git worktree；主 Agent 审查后，在专用集成工作区验证通过再合入协作分支。

支持 Responses 和 OpenAI 兼容 Chat Completions 的文本、流式输出和函数工具调用。使用 SQLite 保存状态、macOS Keychain 保存 API Key。包含操作审批、任务取消、排队、恢复检查、流式通知与结果补读。

## 运行终端演示

```sh
cargo run --locked --manifest-path collab/Cargo.toml -p codex-collab-tui -- --demo
```

左侧主控、右侧三个工作终端；Tab 切换、Enter 单独发送、F2 放大、F3 共享记录、Ctrl+C 停止当前模拟任务、Ctrl+Q 确认退出。所有内容为模拟数据，不连接 API。完整操作和边界见终端壳层文档。

## 运行实时客户端

终端可连接真实协作后台，显示既有会话的 Agent、任务状态与公开消息流：

```sh
cargo build --locked --manifest-path collab/Cargo.toml -p codex-collab-core
cargo run  --locked --manifest-path collab/Cargo.toml -p codex-collab-tui -- \
  --state /absolute/path/to/state --session <会话 ID>
```

`--demo` 与实时模式二选一；未指定 `--session` 时使用第一个会话，没有任何会话且给出 `--repository` 时按其创建（需至少一条 `--validation`），否则弹出创建表单。输入行支持光标编辑与命令历史（`←`/`→` 移动光标、`Home`/`End` 行首行尾、`Backspace`/`Delete` 删除、`Up`/`Down` 回看本窗格已派发的输入）。Enter 派发任务（验收标准留空、无依赖）、Ctrl+F 打开任务编辑器（多行目标与验收标准、依赖勾选、编码开关；`Enter` 换行，`Ctrl+S` 派发，验收标准必填）、Ctrl+L 打开会话列表（↑/↓ 选择、Enter 切换，切换后窗格/任务/共享记录/命令历史整体重载）、Ctrl+D 查看当前任务详情（状态/上下文版本/工作区/基线/候选提交/集成状态/依赖/阻塞原因/结果摘要，PgUp/PgDn 滚动）、Ctrl+C 取消、Ctrl+R 刷新、Ctrl+A 配置 Agent（角色由窗格位置决定，`agent/configure`）、Ctrl+T 连接自检（`agent/testConnection`，会真实调用模型）、Ctrl+U 发布用户约束（`event/publish`，需求/用户覆盖优先于模型决策）、Ctrl+O 查看任务输出（`task/output`，持久化摘要 + 运行中公开草稿 + 只读附件）、Ctrl+K 设置密钥、Ctrl+E 切换编码任务、Ctrl+G 审查并集成（冲突/验证失败/中断的集成任务再次按下则显式放弃本次集成、保留候选提交；仅接受当前归属任务，先核查工作区）、Ctrl+P 压缩上下文、Ctrl+N 新建会话、F3 共享记录；待审操作会弹出审批模态（Y 批准 / N 拒绝）。详见阶段 4、5 与阶段 6 文档。

## 运行后台

需要 Rust 1.95+ 和可执行的 Codex CLI：

```sh
cargo run --locked --manifest-path collab/Cargo.toml -- /absolute/path/to/state /absolute/path/to/codex
```

后台使用 JSON 行请求/响应，密钥经标准输入提交，不放在命令行参数中；具体方法见接口文档。生产入口使用 Keychain，测试使用独立内存凭证。

## 验证

```sh
cargo test --locked --manifest-path collab/Cargo.toml --workspace
cargo test --locked --manifest-path collab/Cargo.toml --test runtime_e2e -- --ignored
cargo test --locked --manifest-path collab/Cargo.toml -p codex-collab-tui -- --include-ignored
cargo clippy --locked --manifest-path collab/Cargo.toml --workspace --all-targets -- -D warnings
python3 collab/scripts/probe_terminal_live.py collab/target/debug/codex-collab-tui collab/target/debug/codex-collab-core
python3 collab/scripts/snapshot_terminal.py collab/target/debug/codex-collab-tui --demo
collab/scripts/stage5_evidence.sh   # 重放以上步骤并重写阶段 5 证据日志与快照
collab/scripts/stage6_evidence.sh   # 阶段 6：同上思路，快照为任务编辑器、任务详情与会话列表画面
python3 collab/scripts/stage6_errors.py collab/target/debug/codex-collab-core
                                    # 6-4 失败矩阵（地址规则、Keychain 临时账户、401/429/传输/缺凭证），全程回环
STATE=<状态目录> SESSION=<会话 id> collab/scripts/stage6_real_evidence.sh --reuse
                                    # 6-4 真实调用：去掉 --reuse 才会访问服务商并计费，且需先在 TUI 里 Ctrl+K 写入密钥
collab/scripts/stage8_evidence.sh   # 阶段 8-1/8-2：本地协作、失败/冲突返工、取消/重启、双探针；不调用付费模型
collab/scripts/stage7_evidence.sh   # 阶段 7：测试、lint、双探针与 demo 快照
STATE=<状态目录> SESSION=<会话 id> collab/scripts/stage7_real_evidence.sh --reuse
                                    # 7 真实多 Agent：去掉 --reuse 才派发任务并计费
STATE=<状态目录> SESSION=<会话 id> collab/scripts/stage7_conflict_evidence.sh --reuse
                                    # 7-3 并发/冲突：重建日志和快照；去掉 --reuse 才派发任务并计费
```

6-4 的真实调用已验证：`agent/testConnection` 返回 `verified=true` 与 `text / stream / function_tools`（经 `chat_completions` 转换路径），随后一个编码任务产出 `file diff` 附件（`hello.txt`，133 字节）并由宿主提交、`task/integrate` 集成。端点按阶段 6 的安全边界在证据里脱敏为 `https://<provider>/v1`，密钥只经 `Ctrl+K` 进入 Keychain，脚本、日志与快照都不接触它。记录见 `evidence/stage-6-real.log`、`evidence/stage-6-real.json` 与两张实时快照。

7 的真实多 Agent 已验证：一条派发给主控，主控自己建两个带依赖的工作任务，下游在依赖集成完成的同一秒才启动（`queued` → `running`），两个差异经审查先后集成到协作分支。模型在收尾前漏掉写操作时，宿主会向同一线程续跑（有预算与停滞判定），并在仍不完成时把 `task_incomplete` 同时写进窗格与共享记录；编码任务不再自己提交，审批从每任务 2 条降到 0。记录见 `evidence/stage-7-guard.log`、`stage-7-guard.json` 与四张实时快照。

7-3 已验证两个 worker 的运行区间重叠、同行修改冲突时分支不前进、放弃后工作区恢复且候选提交保留。失败 turn 会报告遗留子任务，依赖详情区分「已完成 · 未集成」；429 使用秒级有界退避。最新真实重放也已完成放弃后的继续集成（`verdict.ok=true`），两个 worker 的运行观察区间重叠 73.8 秒，0 条审批。记录见 `evidence/stage-7-conflict.{json,log}` 与阶段 7 文档。

阶段 7 汇总见 [stage-7-summary.json](evidence/stage-7-summary.json)。可选的任务级预批准策略因最近两轮均为 0 条审批而暂缓；默认仍逐条人工审批。真实验证仅覆盖 `chat_completions` 与小文本任务，`responses` 和长会话的模型侧压缩尚无真实服务商证据。

端到端测试运行真实 Codex 执行进程和本地模拟 API，不使用真实 API Key。可设置 `CODEX_COLLAB_BIN` 指向源码构建的执行文件；默认使用 PATH 中的 Codex。终端接入测试需先构建 `codex-collab-core`（或设置 `CODEX_COLLAB_CORE`），伪终端实时脚本使用本地行协议，不调用真实模型。

`snapshot_terminal.py` 把伪终端里的画面重建成文本快照，便于审阅界面与留证：`--demo` 抓演示界面，`--live --core <路径>` 会先建一个真实会话，再用 `--keys '^U|\e[C|keep the public API stable'` 之类的按键脚本（`^X` 控制键、`\e` 转义、其余为文本）驱动到指定界面后截图。

第 1 阶段双进程探针仍可独立重跑：

```sh
python3 collab/scripts/probe_app_servers.py /tmp/codex-collab-probe.json
```

本目录是独立 Cargo workspace，避免为终端宿主引入整个 Codex 内部依赖图。执行器通过 App Server 协议接入，原有 Codex 执行与审批机制继续使用。
