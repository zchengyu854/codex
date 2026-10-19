# 第 2 阶段：Rust 协作核心

状态：实现与本地模拟验收完成；桌面界面属于第 3 阶段。

## 已实现

- 独立 Cargo workspace，保留 Codex 上游源码基线；新增 Rust 库及 JSON 行后台入口，后续终端界面可直接复用 Runtime。
- SQLite 会话、Agent、任务、递增事件、审批审计及附件引用；状态目录单宿主锁与崩溃恢复。
- 最多四 Agent 并发、单 Agent 串行、依赖排队、取消和主 Agent 动态协作工具。工作 Agent 不再派生子 Agent。
- 两种 API 协议适配、每次模型请求注入共享记录、角色和来源区分、带版本检查的摘要检查点及上下文预算检查。
- 各任务独立 App Server、CODEX_HOME、临时凭证和 Git worktree；公开文本流订阅、当前草稿补读及持久化结果。
- Keychain 凭证接口、子进程环境隔离、跨流式片段的密钥脱敏、取消时清理整个进程组。
- 精确 Agent/任务关联的操作审批、重复与迟到审批拒绝、等待审批期间取消。
- 指定提交差异审查、串行集成、最新分支重新验证、冲突与失败保留候选；验证命令改写源码时拒绝直接合入。

## 验收

最终目录验收：**33 passed，0 failed，0 ignored**；格式检查及 Clippy（警告视为错误）通过。后台启动、JSON 请求、输入关闭时的响应清空及退出检查通过。原始日志见 `../evidence/stage-2-tests.log`，结构化结果见 `../evidence/stage-2-summary.json`。

```sh
cargo test --locked --manifest-path collab/Cargo.toml
cargo test --locked --manifest-path collab/Cargo.toml --test runtime_e2e -- --ignored
cargo fmt --manifest-path collab/Cargo.toml --check
cargo clippy --locked --manifest-path collab/Cargo.toml --all-targets -- -D warnings
```

端到端测试需要 PATH 中存在 Codex，或设置 CODEX_COLLAB_BIN 为源码构建产物的绝对路径。常规测试默认跳过这一项，完整验收使用 `-- --include-ignored`。

测试覆盖持久化恢复、并发与队列、编码依赖集成、失败依赖、取消竞争、审批路由、过期审批、权限角色、跨会话隔离、摘要版本、字节预算、两种协议的上下文刷新、SSE 分片、命名空间工具、工具参数分片、usage、有限重试、协议拒绝、worktree 隔离、过期结果重新验证及合并失败。

四 Agent 端到端用例使用真实安装的 Codex 执行进程：主 Agent 拆分三个编码任务，其中两个工作 Agent 使用 Chat Completions；工作 Agent 调用真实文件工具创建文件；主 Agent 查看差异、执行内容验证并合入专用协作分支。测试同时确认原始工作区不出现这些产物。

## 已知边界

- 模拟验收使用 codex-cli 0.162.1，尚未构建整个上游 CLI；可用 CODEX_COLLAB_BIN 切换后再运行相同测试。
- 未调用真实供应商、未测试真实 Keychain 项的交互式读写；Keychain 系统接口已通过 macOS 编译检查，测试使用内存凭证存储。
- 超大上下文明确停下并要求摘要或交接，不保证任意历史都能自动无损压缩。字节预算需要按真实模型能力配置。
- 冲突、验证失败及崩溃期间的外部副作用需要人工核查；不会自动重放或清理。
- 尚未实现图形多窗口、安装包、签名公证和真实 API 验收。

界面方向已按用户更正为单终端多窗格，先交付模拟壳层。参见 [终端壳层验收](stage-3-terminal.md)；确认后再接入模型与协作后台。
