# 第 1 阶段：源码基线与接口验证

日期：2026-10-19（Asia/Shanghai）。

## 源码基线

- 本地项目：`/Users/Apple/Documents/ZcyCode/codex-collab`。
- 上游：https://github.com/openai/codex。
- 提交：`89461ea6833832b0551ce24ee32205824d5c108d`。
- 开发分支：`codex/multi-model-desktop`；上游 remote：`upstream`。
- 当前仅建立本地派生项目，没有创建 GitHub 账户下的远端 fork，也没有推送。
- 原 RoboPi 目录已有未提交修改；本阶段未修改该项目。
- 仓库根目录和拟新增目录无适用的 AGENTS.md；现有两个局部 AGENTS.md 不覆盖新增目录。

## 可复现验证

`collab/scripts/probe_app_servers.py` 并发启动两个 App Server，通过独立临时 CODEX_HOME、不同本地 API 地址、不同模型名及不同虚拟密钥完成验证。
每个进程经历 initialize → thread/start → turn/start → item/tool/call → 工具结果回传 → 文本流式输出 → turn/completed。

结果：两组请求均完成，每组恰好一次动态工具调用、两次 /v1/responses 请求，模型和测试密钥均与对应接口匹配。
原始结果见 `collab/evidence/app-server-probe.json`。

**验证边界：** 实测执行文件为本机已安装的 `codex-cli 0.162.1`，不是本次下载源码的构建产物。测试没有调用真实服务商，不能证明真实模型能力、计费、限流和跨服务商兼容性。

## 关键源码发现

- `codex-rs/model-provider-info/src/lib.rs` 的 WireApi 仅支持 Responses；`wire_api = "chat"` 会明确报已移除错误。
- `codex-rs/app-server-protocol/src/protocol/v2/thread.rs` 的 ThreadStartParams 提供 model、modelProvider、config、dynamicTools 等参数。
- 动态协作工具可通过 `item/tool/call` 回调实现，无需让工作 Agent 再派生子 Agent。
- `codex-rs/codex-api/src/common.rs` 提供 Responses 请求和事件类型，是协议映射的参考。
- 现有 `responses-api-proxy` 仅为 Responses 转发代理，不提供 Chat Completions 转换。

## 环境与构建

- Apple Silicon macOS；Rust 1.95.0，与源码声明版本相符；Node 24.14.0。
- 系统安装了 Command Line Tools；未配置完整 Xcode。尚未验证桌面打包及签名。
- 离线模型服务商模块测试因本机缺少锁定的 Git 依赖而无法启动。
- 在线执行模型服务商模块测试成功：33 passed，0 failed，包含 Chat Completions 配置已移除的错误提示测试。
- 该结果仅覆盖 codex-model-provider-info 及其编译依赖，不代表完整 CLI、App Server 或桌面安装包已构建。

## 第 2 阶段待确认范围

新增独立 Rust 协作模块及无界面测试入口：SQLite 会话/事件/任务，四 Agent 调度及动态工具桥接，worktree 隔离、串行集成与验证，取消/异常恢复，以及 Keychain 密钥接口。

Chat Completions 接入采用由协作服务管理的每 Agent 本地协议适配器：把执行侧 Responses 请求与事件转换为上游 Chat Completions 文本和函数工具调用；执行侧仍运行原 Codex 权限与工具循环。Responses 配置通过同一本地入口转发到服务商，保留原协议。两种模式都在每次请求开始前注入共享记录。

适配器只承诺文本、流式输出和函数工具调用；设置 service provider 能力边界，禁用或明确拒绝无法转换的内置工具/扩展功能，不能静默丢弃。为各进程提供独立环境和本地访问凭证，用户的 Chat Completions 密钥只传给适配器。取消需要中止上游流。

上下文同步按“每轮模型调用”实现：协作服务控制的模型请求入口在请求开始前读取共享版本，注入尚未消费的共享记录。仅依赖 turn/start 会遗漏同一 turn 内工具调用后的后续模型请求，因此必须覆盖这些请求。确认决策、来源、任务依赖和版本持久化，不保存私有推理。

验收：模拟接口驱动四 Agent 端到端协作，覆盖两种协议、队列/依赖、取消、重启恢复、旧成果重新验证、测试失败和合并冲突；通过后再进入桌面阶段。
