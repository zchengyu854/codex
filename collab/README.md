# Codex 多模型协作终端

基于 Codex 的独立扩展项目，与 RoboPi 分开。上游提交固定于 `upstream.json`，开发分支为 `codex/multi-model-desktop`。目前完成源码/接口验证、Rust 协作核心、终端壳层演示、终端接入协作后台的四个段落（真实会话/Agent/任务显示、公开文本流、操作审批与密钥提交、编码开关与差异审查集成、会话创建与上下文压缩），并补齐 Agent 配置表单、连接自检与用户约束发布（阶段 5 前三段）。界面为单终端多窗格。

- [第 1 阶段：基线与接口验证](docs/stage-1.md)
- [第 2 阶段：协作核心及验收](docs/stage-2.md)
- [终端壳层：运行与操作](docs/stage-3-terminal.md)
- [阶段 4：终端接入协作后台](docs/stage-4-host-client.md)
- [阶段 5：补齐终端契约覆盖](docs/stage-5-coverage.md)
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

`--demo` 与实时模式二选一；未指定 `--session` 时使用第一个会话，没有任何会话且给出 `--repository` 时按其创建（需至少一条 `--validation`），否则弹出创建表单。Enter 派发任务、Ctrl+C 取消、Ctrl+R 刷新、Ctrl+A 配置 Agent（角色由窗格位置决定，`agent/configure`）、Ctrl+T 连接自检（`agent/testConnection`，会真实调用模型）、Ctrl+U 发布用户约束（`event/publish`，需求/用户覆盖优先于模型决策）、Ctrl+O 查看任务输出（`task/output`，持久化摘要 + 运行中公开草稿 + 只读附件）、Ctrl+K 设置密钥、Ctrl+E 切换编码任务、Ctrl+G 审查并集成、Ctrl+P 压缩上下文、Ctrl+N 新建会话、F3 共享记录；待审操作会弹出审批模态（Y 批准 / N 拒绝）。详见阶段 4 与阶段 5 文档。

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
```

端到端测试运行真实 Codex 执行进程和本地模拟 API，不使用真实 API Key。可设置 `CODEX_COLLAB_BIN` 指向源码构建的执行文件；默认使用 PATH 中的 Codex。终端接入测试需先构建 `codex-collab-core`（或设置 `CODEX_COLLAB_CORE`），伪终端实时脚本使用本地行协议，不调用真实模型。

`snapshot_terminal.py` 把伪终端里的画面重建成文本快照，便于审阅界面与留证：`--demo` 抓演示界面，`--live --core <路径>` 会先建一个真实会话，再用 `--keys '^U|\e[C|keep the public API stable'` 之类的按键脚本（`^X` 控制键、`\e` 转义、其余为文本）驱动到指定界面后截图。

第 1 阶段双进程探针仍可独立重跑：

```sh
python3 collab/scripts/probe_app_servers.py /tmp/codex-collab-probe.json
```

本目录是独立 Cargo workspace，避免为终端宿主引入整个 Codex 内部依赖图。执行器通过 App Server 协议接入，原有 Codex 执行与审批机制继续使用。
