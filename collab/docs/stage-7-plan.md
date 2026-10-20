# 阶段 7 设计：真实多 Agent 协作

日期：2026-10-20（Asia/Shanghai）。
状态：**计划中**（7-1 待凭证；7-2 起依赖 7-1 的实测结果）。

## 出发点

6-4 证明了真实服务商、真实提交、真实集成——但只覆盖「1 个 Agent × 1 个任务 × 无依赖」。这个项目的核心论点是**一个主 Agent 与多个工作 Agent 共享需求、决策、任务与产物**，而这条链路的全部证据目前只有本地模拟：

| # | 缺口 | 证据 | 影响 |
| --- | --- | --- | --- |
| 1 | 真实多 Agent 从未跑过 | `evidence/stage-6-real.json` 只有一个 `agent`；`tests/runtime_e2e.rs` 的三 worker 场景用 mock 上游 | 「委派 → 审查 → 集成」的真实时序、令牌预算、模型是否真的按工具契约行事，全是推断 |
| 2 | 依赖门只有模拟证据 | 依赖门在 `store.rs::claim`（编码依赖必须 `integrationStatus=integrated`）；只有 mock e2e 断言过 | 真实运行里下游会不会在集成前偷跑，没有证据 |
| 3 | 审批会打断协作 | 6-4 里模型自己的 `git commit` 各触发一次审批；多 worker 时中断次数成倍 | 一次委派可能让操作者连续点多次审批，是否可用要看实测 |
| 4 | 协调者的可见性未验证 | 网关注入 `agents` 名册与共享记录（`store.rs::context`），协调者据此派任务 | 真实模型能否正确从名册里挑 worker、能否看懂下游 `queued` 的原因，没有证据 |
| 5 | 集成顺序与冲突未验证 | `task/integrate` 只在 6-4 单任务里跑过一次 | 两个 worker 先后集成的顺序、同一文件冲突时是否真的暂停，没有证据 |
| 6 | 上下文压缩未验证 | `collab_compact` 需要 `expectedRevision`，只有单测 | 真实长会话里压缩是否可用、压缩后共享记录是否还完整 |

缺口 1 是本阶段的主线：它同时把 2–6 从「代码上应该如此」变成「跑过、有时序证据」。

## 7-1 真实派发与依赖门

**场景剧本**（一次运行，约 6–10 次模型 turn，费用很小）

- 新状态目录 + 一个只含 `README.md` 的仓库；校验命令用 `/usr/bin/true`（每步都要求全量通过会与「依赖尚未集成」冲突，校验的严格版留给 7-2 的冲突场景）。
- 会话要求（`requirement`）：保留公开接口，测试通过。
- 三个 Agent（固定 UUID，角色与 6-4 同一套）：
  - **C 主控**：唯一持有 `collab_create_task` / `collab_wait_task` / `collab_review_diff` / `collab_integrate` / `collab_compact`。
  - **W1 工作**：无依赖的编码任务，新建 `a.txt`。
  - **W2 工作**：编码任务，新建 `b.txt`，**依赖 W1 的任务**。
- 操作者只派一个任务给 C：「委派 W1 建 a.txt、W2 建 b.txt（依赖 W1），审查差异并按顺序集成」。
- C 从注入的 `agents` 名册取 W1/W2 的 id，`collab_create_task` 两次，`collab_wait_task` 轮询，`collab_review_diff` 看差异，`collab_integrate` 集成 W1，再等 W2、集成 W2。

**断言**

- 会话里恰好 4 个任务（C + W1 + W2），全部 `completed`，W1/W2 最终 `integrationStatus=integrated`。
- **依赖门时序**：W2 第一次 `running` 的时间 **不早于** W1 的 `integrated` 时间；W2 在此之前保持 `queued`。（轮询每秒记录一次状态/集成状态，时间线进证据。）
- 集成分支上同时有 `a.txt` 与 `b.txt`；原始仓库不被修改（会话是拷贝，不是原地）。
- C 的公开消息里出现两次委派与两次审查；`integration` 事件两条 `integrated`。
- 审批逐条记录（命令 + 理由），默认由操作者在 TUI 决定，`--approve` 只用于重放。

**证据**：`evidence/stage-7-collab.json`（时间线 + 任务快照 + 审批 + 集成记录）、`evidence/stage-7-collab.log`（派生日志）、两张实时快照（共享记录列的委派/审查、任务详情里的依赖与集成状态）。

## 7-2 并发、集成顺序与冲突（依赖 7-1）

- 两个互不依赖的编码任务同时派给 W1/W2，断言二者 `running` 时间段**重叠**（证明并发不是串行），集成仍按协调者顺序逐条执行。
- 让两个任务改同一文件的不同行/同一行：前者应可集成，后者应进入冲突并**暂停**（`integrationStatus=conflict`），协调者/操作者能看到原因；不自动解决冲突。
- 若 7-1 已暴露集成顺序问题，先修再跑本段。

## 7-3 审批策略（可选，取决于 7-1 的摩擦）

- 现状：每条 worker 命令都要人工决定。多 Agent 时中断成倍，7-1 会给出真实次数。
- 方案方向：任务级「预批准执行策略」（沿用 codex 的 `execpolicy_amendment`），在派发时由操作者显式选择，事件里记录批准了哪几条命令前缀；默认仍是逐条审批。**不做**全局静默自动批准。
- 只在 7-1 证明中断确实妨碍使用时才做。

## 7-4 收尾

- `README.md` 与 `stage-7-plan.md` 记录实测结论与限制；`evidence/stage-7-summary.json`（`real_multi_agent_tested`）。
- 一键重放脚本 `scripts/stage7_evidence.sh`：默认离线（mock e2e + 单元测试），`--live` 才走真实服务商。

## 安全边界

- 每个 Agent 一个 Keychain 项（`dev.codex.collab` / agent UUID）。操作者在 TUI 里逐个 `Ctrl+K`；脚本不读、不打印、不落盘密钥。
- 端点在证据里继续脱敏为 `https://<provider>/v1`；脚本默认就这样写。
- 真实运行默认人工审批；`--approve` 模式逐条打印并记录放行的命令。
- 费用：一次运行约 6–10 次模型 turn；探针不计入本阶段（6-4 已覆盖）。

## 顺序与依赖

- 7-1 先行；7-2 依赖 7-1 的时序证据；7-3 只在摩擦被证实后做；7-4 收尾。
- 每段沿用既有节奏：`--locked` workspace 测试 + tui `--include-ignored` + clippy `-D warnings` + fmt `--check` + demo/live pty 探针 + 快照 → 更新文档与证据 → 提交推送 `origin`（不建 PR）。

## 待确认

1. 是否按 3 个 Agent（C/W1/W2）跑；操作者需要 `Ctrl+K` 三次（同一个 key 可以写三次）。
2. 是否允许这次运行产生真实费用（约 6–10 次小模型 turn）。
3. 7-1 期间是否由操作者在 TUI 里逐条审批；若要无人值守，我改用 `--approve` 并逐条记录放行的命令。
