# 阶段 7 设计：真实多 Agent 协作

日期：2026-10-20（Asia/Shanghai）。
状态：**阶段 7 已收尾；7-1 至 7-3 已实现并验证，7-4 按实测暂缓，7-5 已完成**（真实多 Agent 派发与依赖门；宿主兜底与提交归属；并发、冲突与队列卫生）。7-3 已在真实场景走通并发、冲突、放弃及后续继续集成。

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

- 会话里恰好 3 个任务（C + W1 + W2），全部 `completed`，W1/W2 最终 `integrationStatus=integrated`。
- **依赖门时序**：W2 第一次 `running` 的时间 **不早于** W1 的 `integrated` 时间；W2 在此之前保持 `queued`。（轮询每秒记录一次状态/集成状态，时间线进证据。）
- 集成分支上同时有 `a.txt` 与 `b.txt`；原始仓库不被修改（会话是拷贝，不是原地）。
- C 的公开消息里出现两次委派与两次审查；`integration` 事件两条 `integrated`。
- 审批逐条记录（命令 + 理由），默认由操作者在 TUI 决定，`--approve` 只用于重放。

**证据**：`evidence/stage-7-collab.json`（时间线 + 任务快照 + 审批 + 集成记录）、`evidence/stage-7-collab.log`（派生日志）、两张实时快照（共享记录列的委派/审查、任务详情里的依赖与集成状态）。

**实现记录（2026-10-20）**

交付：`scripts/stage7_collab.py` 三个子命令——`prepare`（新状态目录、仓库、会话、三个固定 UUID 的 Agent，打印三个窗格的 `Ctrl+K` 步骤）、`verify`（只派发主控一条任务，全程记录状态时间线、审批、任务快照、集成事件、协作分支文件与令牌用量）、`followup`（把主控漏掉的集成补上，并如实记录补了几次）。`scripts/stage7_report.py` 从三份 JSON 派生日志，不手写任何数字。

实测（真实服务商，`chat_completions`，同一个 key 经 `Ctrl+K` 写入三个 Agent）：

| 观测 | 结果 |
| --- | --- |
| 依赖门 | `b.txt` 从 24.1s 起 `queued`，126.4s `a.txt` 变 `integrated` 的同一秒转 `running`；它的 `baseCommit` 就是 `a.txt` 的集成头 `c1216b5`（详情快照可见） |
| 两次集成 | `a.txt`（`391cd40 → c1216b5`）、`b.txt`（`c1216b5 → 78fee2b`），校验命令重跑通过 |
| 并发 | 主控与 `a.txt` 同时运行；两个工作任务分别落在两个不同工作 Agent（…0002 / …0003），没有退化成同一个 |
| 分支内容 | 协作分支上 `a.txt`=`alpha`、`b.txt`=`beta`；源仓库 `git status` 干净 |
| 审批 | 4 条，全部是 worker 自己的 `git add/commit`（每任务 2 条），理由都是 index.lock 在沙箱外；宿主随后自己提交 |
| 令牌 | 本次场景 主控 118k / 工作1 159k / 工作2 89k（含被浪费在"自己提交"上的轮次） |

发现（直接决定 7-2 的范围）：

1. **模型在写操作前收工**。三次 turn 都以"宣布下一步"结束而没有发出那个工具调用：第一次是建第二个任务前，第二次是等 `b.txt` 前，后续派发里是集成前。rollout 证据明确：最后一次工具调用是 `collab_review_diff`，之后只有 assistant 文本和 `task_complete`，没有报错、没有失败的调用。同一条后续任务第二次派发就成功了，说明一次推动能补上——但宿主现在没有任何推动。
2. **完成是自述**。主控任务 `completed` 时，它自己创建的 `b.txt` 既没等待也没集成，宿主没有产生任何信号；操作者只能从窗格里读出来。
3. **每个编码任务固定浪费两次审批和若干轮令牌**在"自己提交"上，而提交本来由宿主在 turn 结束后负责；worker 自述里的"commit failed due to sandbox"因此是误导性叙述。

这三条都属于"宿主该兜底但没兜"，不是模型能力本身能修的。7-2 因此从"并发与冲突"调整为先做宿主兜底，冲突场景顺延到 7-3。

验证：workspace 118（core 33 / tui 85）、opt-in 86、clippy `-D warnings`、fmt `--check`、双 pty 探针（本段未改 Rust，回归基线不变）。证据：`evidence/stage-7-collab.json`、`stage-7-collab-first-attempt.json`、`stage-7-integration.json`、`stage-7-collab.log` 与三张 34×132 快照（会话总览、共享记录、`b.txt` 详情）。限制：只覆盖一个编码依赖、两次集成；冲突与并发集成顺序留到 7-3；`responses` 仍未接真实服务商，沿用 6-4 的限制。

## 7-2 宿主兜底：提前收工与"自己提交"（已实现并验证）

**实现记录（2026-10-20，第二轮）**

落地四件事：

- **委派来源**：`TaskRequest.createdBy` 由 `collab_create_task` 写入当前任务 id；公开的 `task/create` 会把它剥掉，客户端不能替别人认领任务。
- **收尾兜底（runtime）**：协调者的 turn 正常结束、但它创建的子任务仍未完成（`queued`/`running`，或 `completed` 且编码未集成）时，向**同一线程**追加一轮提示，最多 6 次；连续 2 次看到同一未完成集合即判停滞。预算或停滞用尽时发布 `task_incomplete`（带原因）并结束；每次续跑发布 `task_continued`（第几次 + 未完成清单）。不静默重试，也不无限循环。
- **提交归属**：`DEVELOPER_INSTRUCTIONS` 与每个任务的 turn 提示都写明"不要执行 `git commit` / `git add` / `git config`，宿主在 turn 结束后提交"。7-1 的实测显示只写进常驻指令时模型会忽略，所以两处都写。
- **可见性**：主控窗格出现 `[续跑] 第 N 次：还有 M 个未完成的子任务`；`task_incomplete` 同时进窗格与共享记录；任务详情多一行"宿主兜底 已由宿主续跑 N 次 / 收尾时仍有子任务未完成：…"。

两次真实运行（同一会话、同一批 Agent、同一个 key）：

| | 第一次（e/f，预算 2） | 第二次（g/h，预算 6 + 停滞判定） |
| --- | --- | --- |
| 审批 | 0 | 0 |
| 续跑 | 2 次，随后 `task_incomplete` | 2 次，模型自己收尾，没有未完成标记 |
| 依赖门 | `e.txt` 集成 96.6s = `f.txt` 启动 96.6s | `g.txt` 集成 114.6s = `h.txt` 启动 114.6s |
| 集成 | 只有 `e.txt`（`f.txt` 到上限时仍未集成） | 两个都集成：`g.txt`（`e3989c9`）→ `h.txt`（`30a0cba1`） |
| 分支 | — | `g.txt`=`eta`、`h.txt`=`theta`，源仓库干净 |

第一次运行说明"两次预算不够"：模型每轮只完成一个写操作，两次续跑分别补上了"等 e 完成"和"集成 e"，第三次收尾又发生在集成 `f.txt` 之前，宿主于是如设计那样标了未完成。把预算改成 6 并加停滞判定后，第二次运行两次续跑就走完了全流程（rev 224：等 `g` 完成并集成；rev 234：等 `h` 完成），主控在 221.5s 正常收尾。证据：`evidence/stage-7-guard.json`、`stage-7-guard-budget.json`、`stage-7-guard.log`。

worker 的两次自述都写着 "no git commands run, as instructed"，审批也从 7-1 的每任务 2 条降到 0 —— 提交归属这条约束对当前模型是有效的。

顺带观测（留给 7-3）：有一次运行因上游 429（网关重试 3 次后失败）把主控任务打成 `failed`，它留下的"已完成但未集成"的编码任务让下游一直 `queued`，直到脚本超时取消。数据没丢（操作者仍可 Ctrl+G 集成或用 `followup` 补派），但"不会有人再来集成它"这件事目前只能靠人看出来。

验证：workspace 121（core 35 / tui 86）、opt-in 87、clippy `-D warnings`、fmt `--check`、双 pty 探针、demo 快照。证据：`evidence/stage-7-guard.{json,log}`、`stage-7-guard-budget.json`、`stage-7-tests.log` 与四张 34×132 快照（总览、共享记录、主控详情、依赖任务详情）。

## 7-3 并发、集成顺序与冲突（已实现）

**实现记录（2026-10-20）**

- 冲突保留候选工作区，协作分支不前进。新增 `task/abortIntegration(taskId)`：仅接受 `integrationStatus=conflict`，持集成互斥锁，执行 `merge --abort`（squash 冲突无 MERGE_HEAD 时允许失败）和 `reset --hard HEAD`，将任务集成状态退回 `pending`，发布 `integration` 事件。任务结果和候选提交保留；集成工作区里的冲突及手工修改被丢弃。TUI 对冲突任务再次按 `Ctrl+G` 即放弃；返工后再审查、集成，不自动解决冲突。
- 队列卫生：turn 失败且遗留自己创建的未完成子任务时，发布一次 `task_incomplete`，包含失败原因与子任务清单。详情里的依赖行显示「已完成 · 未集成」，操作者可手工 `Ctrl+G` 集成依赖，或补派主控收尾。队列不会因创建者失败而绕过依赖门。
- 集成事件带任务 Agent 归属，窗格从 `evidence.message` 展示冲突原因。429 最多 4 次请求，默认等待 5/15/30 秒；5xx 最多 3 次，等待 2/4 秒。有效的秒数 `Retry-After` 优先，等待上限 60 秒且可取消；不重放已经开始的响应流。
- `scripts/stage7_conflict.py` 先集成种子文件，再把同基线、改同一行的任务派给两个 worker，按顺序集成以触发冲突，放弃后派发改另一行的恢复任务。记录错峰偏移、观察窗口、任务快照、分支头、工作区状态和审批；断言放弃不丢候选提交及最终文件内容。`stage7_conflict_evidence.sh --reuse` 从现有 JSON 重建日志和三张快照，不再请求模型。

真实记录 `evidence/stage-7-conflict.json`（`shared7.txt`，两个 worker 错峰 22 秒）：

| 观测 | 结果 |
| --- | --- |
| 宿主并发 | `84e19297` / `0aced9ba` 的 `running` 观察区间分别为 0.0–74.8 秒 / 1.0–86.9 秒，重叠 73.8 秒；两者基线均为 `acc41f5` |
| 顺序集成 | 种子 `62f6d00 → acc41f5`；首个编辑 `acc41f5 → fd2e966` |
| 同行冲突 | 第二个编辑进入 `conflict`，分支仍为 `fd2e966`，集成工作区有未合并内容 |
| 放弃候选 | `task/abortIntegration` 后分支仍为 `fd2e966`，工作区干净；任务 `completed / pending`，提交保留 |
| 真实恢复任务 | `bee5612c` 完成并集成：`fd2e966 → edcaa87`，证明放弃后可继续工作 |
| 内容与源仓库 | 最终文件为 `ONE\ntwo\nTHREE\n`；源仓库干净；0 条审批；`verdict.ok=true` |

限制：`running` 重叠证明宿主可以并发调度，不能证明服务商同时计算两个请求。交接记录指出该服务商同一凭证并发上限为 1，前几轮突发请求出现 429；本轮错峰未被限流打断，结论不外推到其它服务商。并发任务派发偏移为 0/22 秒；上述时间从错峰结束后的观察窗口起算。更早的 `shared6.txt` 恢复任务因 Keychain 不可用失败，原始失败证据保留在提交 `0467a6e`；本轮经用户明确授权真实调用、目标服务商及自动命令审批后，改由凭证可用的 worker 完整走通。

验证：workspace 124（core 37 / tui 87）、TUI 含可选测试 88；clippy、fmt、双 pty 探针与 demo 快照见 `evidence/stage-7-tests.log`。`core/tests/git.rs::a_conflicting_candidate_is_marked_conflict_and_can_be_dropped` 验证冲突不推进分支、脏工作区拒绝后续集成、放弃后内容保留且仍可集成；runtime 回归验证失败 turn 恰好一次未完成上报，gateway 回归验证 `Retry-After: 0`，TUI/store 回归覆盖真实事件结构、Agent 归属与依赖提示。证据另见 `stage-7-conflict.log` 及三张 34×132 快照（总览、共享记录、冲突任务放弃后的详情）。

## 7-4 审批策略（按实测暂缓）

7-1 的 4 条审批均来自 worker 自行提交；7-2 将提交交给宿主后，完整委派运行和 7-3 冲突重放均为 0 条审批（分别见 `stage-7-guard.json`、`stage-7-conflict.json` 的 `approvals`）。目前没有足够摩擦支持增加任务级预批准策略，本段按原计划的可选条件暂缓。

默认仍逐条人工审批；重放脚本只有显式开启 `--approve` / `STAGE7_APPROVE=1` 才逐条记录并放行。若以后真实任务出现频繁的非提交类审批，再评估由操作者选择、记录命令前缀的任务级策略。

## 7-5 收尾（已完成）

机器可读汇总：[`evidence/stage-7-summary.json`](../evidence/stage-7-summary.json)，`real_multi_agent_tested=true`。汇总引用原始运行证据，记录依赖门、主控续跑、审批次数、冲突放弃与恢复、最终验证以及已知限制。7-1 首轮和低预算运行的失败结论保留，不改写成成功。

重放入口：

- `bash collab/scripts/stage7_evidence.sh`：离线构建、测试、lint、双 pty 探针和 demo 快照。
- `STATE=<状态目录> SESSION=<会话 id> bash collab/scripts/stage7_real_evidence.sh --reuse`：复用委派运行的 JSON 和快照生成日志。
- `STATE=<状态目录> SESSION=<会话 id> bash collab/scripts/stage7_conflict_evidence.sh --reuse`：复用冲突运行的 JSON，重建日志和快照。

去掉 `--reuse` 会真实派发并计费；先关闭占用状态目录的 TUI，在各 Agent 用 `Ctrl+K` 配好凭证，并为重复运行指定新文件名。委派脚本用 `FIRST` / `SECOND`，冲突脚本用 `FILE`。

验收范围：真实 `chat_completions` 上的一主两工作 Agent、一个编码依赖、顺序集成、提前收工兜底、同行冲突和放弃后的恢复。本轮收尾只补文档及汇总，复用最终源码的 124 项 workspace / 88 项 TUI 含可选测试、lint、fmt 与双探针记录。`responses`、长会话中的模型侧压缩仍只有本地覆盖；真实场景的集成校验为 `/usr/bin/true`，不代表生产工作负载的正确性或性能。这些限制在汇总中明确保留。

## 安全边界

- 每个 Agent 一个 Keychain 项（`dev.codex.collab` / agent UUID）。操作者在 TUI 里逐个 `Ctrl+K`；脚本不读、不打印、不落盘密钥。
- 端点在证据里继续脱敏为 `https://<provider>/v1`；脚本默认就这样写。
- 真实运行默认人工审批；`--approve` 模式逐条打印并记录放行的命令。
- 费用：一次运行约 6–10 次模型 turn；探针不计入本阶段（6-4 已覆盖）。

## 顺序与依赖

- 7-1 先行；7-2 依赖 7-1 的时序证据；7-3 已完成冲突与队列卫生；7-4 按实测暂缓；7-5 汇总与验收已完成。
- 每段沿用既有节奏：`--locked` workspace 测试 + tui `--include-ignored` + clippy `-D warnings` + fmt `--check` + demo/live pty 探针 + 快照 → 更新文档与证据 → 提交推送 `origin`（不建 PR）。

## 已确认

1. 按 3 个 Agent（C/W1/W2）跑；操作者在三个窗格分别 `Ctrl+K` 写入同一个 key。
2. 允许产生真实费用；7-1 实际用了 2 次主派发 + 2 次后续派发。
3. 7-1 由 `--approve` 放行，每条命令与理由逐条进证据；后续段若有审批缺口再改回人工。
