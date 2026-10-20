# 阶段 8：真实代码项目验收

日期：2026-10-20（Asia/Shanghai）。
状态：**8-1、8-2 已实现并通过本地验收；8-3 真实模型场景已准备，尚未运行**。协作场景使用真实 Codex 和本地模拟上游；取消使用 fixture App Server，重启使用持久化崩溃状态模拟。尚无阶段 8 的真实服务商结果。

## 阶段目标

依据[后续开发计划](development-roadmap.md)，把阶段 7 的小文本产物验收升级为代码修改、非空项目测试、依赖顺序集成及失败恢复。模型自述和模拟 API 通过均不能替代真实服务商验收。

## 8-1 可复现的代码项目与离线剧本（已完成）

### 测试项目

`fixtures/stage8-project/base/` 是可复制到独立 Git 仓库的 Python 标准库项目。无需安装包，验证命令为：

```sh
/usr/bin/python3 -B -m unittest discover -s tests -v
```

基线包含 `slug.py`、`total.py` 和 4 项测试；`changes/` 提供模拟上游使用的候选实现与扩展测试：

| 任务 | 变更 | 依赖 |
| --- | --- | --- |
| 名称规范化 | 处理连续空白、制表符、换行及 Unicode 标题；扩展名称测试 | 无 |
| 数字求和 | 接受整数字符串，拒绝无效数字；扩展求和测试 | 无 |
| 命令行报告 | 新增 report.py，组合标题与数字结果；测试 CLI 输出和无效输入 | 前两个任务均已集成 |

每份独立候选可以单独通过项目测试。依次集成后测试数量为 6、8、11；最终 CLI 对含空白的 Hello World 和 2/-1/4 输出 `hello-world:5`。基线测试不会被删除。

### 实现路径

升级既有 `core/tests/runtime_e2e.rs`，复用其本地上游、内存凭证、真实 Codex 工具调用、Runtime 调度和协议转换；不再用“写三个文本文件”作为此测试的最终产物。一主三工作 Agent 使用 Responses 和 Chat Completions 两条本地协议路径。

主控的工具调用由本地 mock 确定：创建两个独立任务，再创建依赖它们的第三个任务；等待、读取差异、提交指定候选的审查记录，交给宿主运行 unittest 后顺序集成。worker 实际通过 Codex shell 工具写代码并执行测试，测试程序不替 worker 直接修改其工作区。

断言覆盖：

- 基线 4 项和最终 11 项测试实际执行成功。
- 第三个任务创建后处于 queued，其 task_started 事件晚于两个依赖的 integrated 事件。
- 第三个任务的 baseCommit 是第二次集成产生的 head；三次集成的 previousHead/head 连续衔接。
- 工作任务的 createdBy 指向主控；每个任务完成且 integrated。
- 三次集成都保存实际验证命令、成功标记、退出码和输出；源仓库仍干净，report.py 只存在于协作工作区。
- 两种协议均经本地请求路径运行；测试凭证未进入公开事件。

`core/tests/project.rs` 另做两项本地回归：逐个注入三个功能的错误实现，确认项目测试拒绝；把会导致真实 unittest 失败的候选送入 Git 集成，确认退出码为 1、分支不前进、候选和失败工作区保留、源仓库不变。这提供了 8-2 的失败基线；后续恢复链路见下节。

### 产品接口变更

集成结果的 `checks` 新增 `exitCode`，保留已有 `success`、命令与输出。成功测试记录 0；本次 unittest 失败记录 1；无退出码时为 null。它不改变推进分支的条件，不增加重试或清理操作。

### 重放与证据

```sh
bash collab/scripts/stage8_evidence.sh
```

脚本构建 core/TUI，运行 workspace 全部测试（包含需安装 Codex 的 opt-in 场景和 TUI 真实宿主测试）、clippy、fmt；输出：

- `evidence/stage-8-code.json`：任务、递增事件、三次集成、基线/最终 unittest 原始输出与退出码。
- `evidence/stage-8-failure.json`、`stage-8-conflict.json`：验证失败/代码冲突、显式放弃及新任务返工的记录。
- `evidence/stage-8-interruption.json`：fixture 父子进程取消、持久化状态重开、候选保留与不自动重放的记录。
- `evidence/stage-8-tests.log`：全套测试、lint、格式检查、双 pty 探针与 demo 快照；快照另存 `stage-8-demo-screen.txt`。
- `evidence/stage-8-summary.json`：从运行记录与日志派生的结果，明确 `real_provider_tested=false`。

8-1 首轮全套测试 128 项通过（含 opt-in）。8-2 新增恢复及终端路由回归，最终计数见 `stage-8-summary.json`，完整测试、clippy、fmt、双 pty 探针和 demo 快照由同一脚本生成。

本次环境为 macOS，执行器 `codex-cli 0.162.1`；可用 `CODEX_COLLAB_BIN` 指定另一个 Codex 执行文件，但更换后需要重放。脚本会替换本阶段生成的 JSON/日志；失败时保留测试日志，不把旧成功汇总冒充本次结果。

## 8-2 失败与冲突恢复（本地验收已完成）

### 修复的缺口

测试失败后的集成工作区必须保留供核查，但原先没有显式恢复入口。另一项复现表明，拿旧冲突任务执行放弃，可能清掉后来任务的失败候选。

本段复用 `task/abortIntegration`，没有增加新任务状态或通用清理命令：

- 接受已完成编码任务的 `conflict`、`failed`、`interrupted` 集成状态；拒绝 pending、checking 和 integrated。
- 在集成互斥锁内检查会话最新 integration 事件的 taskId；只允许当前归属任务放弃候选，旧任务不得清理其它任务的工作区。
- 新集成先检查工作区干净，再记录 checking。脏工作区拒绝的新尝试不会改变任务状态或抢走归属。
- 放弃前检查 Git 可见的未跟踪文件；存在时先拒绝，保持原样供人工检查，不自动删除。没有这些文件时恢复已跟踪内容到当前 HEAD，任务集成状态退回 pending，保留原结果和候选提交。

操作路径：先用 `Ctrl+D` 查看工作区和候选信息、在工作区核查失败原因，再对当前暂停任务按 `Ctrl+G` 放弃本次集成。它会丢弃集成工作区的已跟踪手工修改，不修改源仓库或候选任务工作区。检查并移走 Git 可见的未跟踪产物后才能继续。返工用 `Ctrl+F` 显式新建任务，在目标中引用旧 taskId；终端的历史任务选择入口仍属于阶段 9。

### 四项回归与证据

`core/tests/recovery.rs` 复用阶段 8 代码项目，在临时仓库中通过真实 Runtime/Git/unittest 验证：

1. **失败后返工**：错误实现使 unittest 返回 1；分支不前进。脏工作区阻止新候选且不改写它的 pending 状态；未跟踪诊断文件阻止放弃并保留原候选内容。人工处理文件后，放弃成功，原提交/结果保留；引用旧任务的新修复任务通过 6 项测试并集成。
2. **代码冲突返工**：两个同基线任务改同一实现行，首个通过 6 项测试集成，第二个冲突且不推进分支。显式放弃后，新建引用旧任务的返工任务，基线等于当前集成头；新实现通过 7 项测试后集成。
3. **过期冲突保护**：模拟操作者在宿主外恢复文件后，旧任务仍显示 conflict，而后来任务已有失败候选。旧任务放弃请求被拒绝，后来候选内容保留；当前任务可以放弃。
4. **取消与重开**：fixture App Server 启动真实父子进程并留下部分文件；取消后两进程均退出，文件保留。随后构造 running/checking 的崩溃遗留状态，关闭并重开 Store、执行 recover：任务/集成变 interrupted，原候选提交和结果保留。重启调度器不重放 interrupted/cancelled 任务；恢复事件不重复产生，中断集成可显式放弃。

终端回归覆盖 `conflict` / `failed` / `interrupted` 的放弃路由，以及 pending 仍打开审查表单。双 pty 探针覆盖既有操作和终端恢复。

验收边界：候选由测试夹具确定，取消由 fixture App Server 驱动；重启覆盖持久化状态恢复及重开后的调度，不宣称已强杀生产宿主做过完整故障注入。真实 Codex 协作链路仍由 8-1 本地 mock 场景覆盖。

## 8-3 真实模型验收（下一步）

使用同一合成项目，由真实主控与 worker 自主产生变更，验证完整依赖协作以及冲突返工。mock 提供的 changes 不能替真实模型预写结果。保留本地测试作为确定性的验收标准，记录真实请求、用量、耗时、审批及服务商限制。

脚本 `scripts/stage8_real.py` 已实现有界场景：`prepare` 在独立状态目录初始化合成项目并配置一主两 worker；操作者通过 TUI 的 `Ctrl+K` 将凭证写入 Keychain；`verify` 只派发一个主控任务，由模型创建三个代码任务、实现两个独立改动及双依赖报告，再创建两个同基线 `total.py` 候选，集成其中一个、观察另一个真实 Git 冲突、显式放弃并创建引用冲突 taskId 的返工任务。脚本默认最多创建 7 个本轮任务（含主控），超限会取消仍在运行的任务；运行超时也会取消活动任务并保存部分证据。

最终验收不只检查模型的自述：脚本确认依赖任务开始时两个前置集成都已完成；从集成事件确认 conflict 和 pending 放弃记录；检查五个 worker 集成、一个冲突候选保留为 pending、返工目标引用该候选且返工已集成；同基线冲突候选中另一个已集成；复制最终集成工作区到临时目录，用夹具提供的可信测试替换模型可修改的测试后运行完整 unittest；确认源仓库干净。输出 `evidence/stage-8-real.json`，服务商 URL 脱敏，包含任务、状态时间线、审批、完成响应 usage、运行耗时、集成检查和 oracle 输出。

运行步骤（真实请求会产生费用；`--approve` 会自动接受并记录 Agent 的任务命令审批）：

```sh
python3 collab/scripts/stage8_real.py prepare \
  --core collab/target/debug/codex-collab-core \
  --state /private/tmp/codex-collab-stage8-live \
  --base-url https://<provider>/v1 --model <model>
# 在输出的 TUI 命令中分别对主控和两个 worker 按 Ctrl+K 保存凭证，退出 TUI
python3 collab/scripts/stage8_real.py verify \
  --core collab/target/debug/codex-collab-core \
  --state /private/tmp/codex-collab-stage8-live \
  --json collab/evidence/stage-8-real.json --approve --timeout 1800 --max-tasks 7
```

`prepare` 仅接受新状态目录；已存在 `repo/.git` 时会拒绝重用，避免覆盖已有场景。真实执行前需对服务商端点、模型、发送的合成项目/提示、请求费用范围及自动命令审批另行明确授权。当前用户此前授权的单次请求属于阶段 7，不能作为阶段 8 请求授权。

本轮未发起付费请求，未把本地 mock 结果标记为真实模型通过。真实运行范围与目标沿用开发计划中的授权要求；失败记录保留，环境错误与产品错误分别说明。
