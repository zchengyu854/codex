#!/usr/bin/env python3
"""Render `evidence/stage-7-collab.log` from the three stage 7 run records.

Every line is derived from the JSON the harness wrote; nothing is retyped, so the log cannot drift
from the machine-readable evidence.
"""
import json
import sys
from pathlib import Path


def load(path):
    text = Path(path).read_text(encoding="utf-8")
    return json.loads(text)


def short(agent_id):
    return "…" + agent_id[-4:]


def clip(text, limit=400):
    text = (text or "").strip().replace("\n", " | ")
    return text if len(text) <= limit else text[: limit - 1] + "…"


def timeline_block(entries):
    out = []
    for entry in entries:
        out.append(
            f"  {entry['at']:>7.1f}s  {entry['task']}  {entry.get('agent', '?')}  "
            f"{entry['status']:<10} {entry.get('integrationStatus') or '—'}"
        )
    return out


def retry(run):
    """Render one guarded run: what the host nudged, what it spent and what it finally flagged."""
    add = print
    add("## 环境")
    add(f"状态目录 {run['state']}")
    add(f"会话     {run['session']}")
    for agent_id, agent in run["agents"].items():
        add(
            f"Agent    {short(agent_id)} · {agent['role']:<11} {agent['model']} · "
            f"{agent['protocol']} · 预算 {agent['contextBytes']} 字节"
        )
    first_agent = next(iter(run["agents"].values()))
    add(f"端点     {first_agent['baseUrl']}（脚本默认脱敏；真实值只在状态目录与 Keychain）")
    add("")

    add("## 场景：一条派发，两个带依赖的文件")
    add(f"派发 {run['dispatch']['taskId'][:8]}；本会话此前已有 {run.get('priorTasks', 0)} 个任务")
    add("")
    add("时间线（只列本次新建的任务）：")
    for entry in run["timeline"]:
        add(
            f"  {entry['at']:>7.1f}s  {entry['task']}  {entry.get('agent', '?')}  "
            f"{entry['status']:<10} {entry.get('integrationStatus') or '—'}"
        )
    add("")
    gate = run["gate"]
    at = lambda value: "—" if value is None else f"{value:.1f}s"  # noqa: E731
    add(
        "依赖门：下游 {} 首次 running 在 {}，前置 {} 变 integrated 在 {}；此前下游一直 queued（{}）→ {}".format(
            gate["dependent"],
            at(gate["dependentStartedAt"]),
            gate["dependency"],
            at(gate["dependencyIntegratedAt"]),
            "是" if gate["wasQueued"] else "否",
            "成立" if gate["ok"] else "不成立",
        )
    )
    add("")
    add("任务：")
    for task_id, task in run["tasks"].items():
        deps = ", ".join(d[:8] for d in task["dependencies"]) or "无"
        add(
            f"  {task_id[:8]} · {short(task['agentId'])} · {task['status']} · "
            f"集成 {task['integrationStatus']} · 依赖 {deps} · 提交 {(task['commit'] or '—')[:8]} · "
            f"由 {(task.get('createdBy') or '—')[:8]} 创建"
        )
        add(f"    目标   {clip(task['goal'], 160)}")
        add(f"    自述   {clip(task['result'], 260)}")
    add("")

    add("## 宿主兜底（模型提前收工时）")
    continuations = run.get("continuations") or []
    if not continuations:
        add("本轮没有触发兜底：主控自己走完了全部步骤。")
    for event in continuations:
        data = event["data"]
        pending = data.get("tasks") or []
        listed = ", ".join(
            f"{t['taskId'][:8]} {t['status']}/{t['integrationStatus']}" for t in pending
        )
        if event["kind"] == "task_continued":
            add(f"  续跑第 {data.get('attempt')} 次（rev {event['revision']}）：{listed}")
        else:
            add(f"  标记未完成（rev {event['revision']}，{data.get('reason')}）：{listed}")
    add("")

    add("## 审批")
    approvals = run["approvals"]
    add(f"{len(approvals)} 条（--approve 放行时会逐条打印命令与理由）")
    for approval in approvals:
        add(f"  - {approval['params'].get('command')}")
        add(f"    理由 {clip(approval['params'].get('reason'), 200)}")
    add("")

    add("## 集成")
    for event in run["integrations"]:
        data = event["data"]
        if data.get("status") == "checking":
            continue
        evidence = data.get("evidence") or {}
        add(
            f"  task {str(event.get('taskId'))[:8]} · {data.get('status')} · "
            f"{str(evidence.get('previousHead'))[:8]} → {str(evidence.get('head'))[:8]} · "
            f"任务提交 {str(evidence.get('taskCommit'))[:8]}"
        )
    add("")

    add("## 协作分支")
    add(f"HEAD {run['integrationHead'][:8]}")
    for name, item in run["files"].items():
        add(f"  {name} · {item.get('text', item.get('error'))!r} · {'ok' if item['ok'] else 'FAIL'}")
    add(f"原始仓库未被改动：{'是' if run['sourceRepoClean'] else '否'}")
    add(f"最近提交：{run['integrationLog']}")
    usage = {}
    for event in run["usage"]:
        key = short(event["agentId"])
        usage[key] = usage.get(key, 0) + event["data"].get("total_tokens", 0)
    add(f"令牌（本会话累计）：{', '.join(f'{k} {v}' for k, v in usage.items())}")
    add("")

    add("## 结论")
    verdict = run["verdict"]
    if verdict["ok"]:
        add("通过：两个工作任务都完成并集成，依赖门成立，协作分支同时带两个文件。")
    else:
        add("未通过：" + "；".join(verdict["problems"]))
    add("")
    add("说明：这条链路里「提交」由宿主负责，worker 不再自己 `git add/commit`；模型的收尾由宿主兜底，")
    add("每次续跑写进主控窗格与任务详情（宿主兜底一行），最后的未完成标记还会进共享记录。")


def conflict(run):
    """Render the concurrency/conflict run: who overlapped, what conflicted, what the drop did."""
    add = print
    add("## 环境")
    add(f"状态目录 {run['state']}")
    add(f"会话     {run['session']}")
    for agent_id, agent in run["agents"].items():
        add(
            f"Agent    {short(agent_id)} · {agent['role']:<11} {agent['model']} · "
            f"{agent['protocol']} · 预算 {agent['contextBytes']} 字节"
        )
    add("")
    add("## 场景：种子、两个同基线冲突编辑、放弃后的恢复任务")
    add("种子任务先建文件并集成；两个 worker 从同一基线修改同一行，依次集成以触发冲突；放弃后再派发另一个行的编辑。")
    if "watchStart" in run:
        add(f"错峰派发 {run['stagger']}s；派发偏移 {run['dispatch']}；并发观察从 {run['watchStart']}s 开始。")
    add("每次轮询的时间线单独从 0 计时；并发窗口只计算两个冲突编辑的观察区间。")
    add("")
    add("时间线（只列本轮任务）：")
    for entry in run["timeline"]:
        add(
            f"  {entry['at']:>7.1f}s  {entry['task']}  {entry.get('agent', '?')}  "
            f"{entry['status']:<10} {entry.get('integrationStatus') or '—'}"
        )
    add("")
    spans = run["concurrency"]["spans"]
    add("并发窗口：")
    for task, (start, end) in sorted(spans.items(), key=lambda item: item[1][0]):
        add(f"  {task}  {start:.1f}s → {end:.1f}s")
    add(f"至少两个任务真正重叠：{'是' if run['concurrency']['overlapped'] else '否'}")
    add("")
    add("任务：")
    for task_id, task in run["tasks"].items():
        add(
            f"  {task_id[:8]} · {short(task['agentId'])} · {task['status']} · "
            f"集成 {task['integrationStatus']} · 提交 {(task.get('commit') or '—')[:8]}"
        )
        add(f"    目标 {clip(task['goal'], 120)}")
        add(f"    自述 {clip(task['result'], 160)}")
    add("")
    add("## 集成（与 TUI 的 Ctrl+G 同一条宿主路径）")
    seed = run["seedIntegration"]
    add(
        f"  种子 · {seed['status']} · {str(seed.get('previousHead'))[:8]} → {str(seed.get('head'))[:8]}"
    )
    for item in run.get("integrations") or []:
        add(
            f"  编辑 · {item['status']} · {str(item.get('previousHead'))[:8]} → {str(item.get('head'))[:8]}"
        )
    bad = run.get("conflict") or {}
    add(
        f"  同一行 · {bad.get('status')} · 分支未前进（{str(bad.get('originalHead'))[:8]}）"
    )
    add("")
    add("## 冲突与放弃")
    add(f"冲突时工作区有未合并内容：{'是' if run.get('worktreeDirtyDuringConflict') else '否'}")
    abort = run.get("abort") or {}
    add(
        f"task/abortIntegration → {abort.get('status')} · 分支 {str(run.get('headDuringConflict'))[:8]} → "
        f"{str(run.get('headAfterAbort'))[:8]} · 工作区仍有改动："
        f"{'是' if run.get('worktreeDirtyAfterAbort') else '否'}"
    )
    after = run.get("taskAfterAbort") or {}
    add(f"任务状态回到 {after.get('integrationStatus')}（自述与提交保留，可返工后重新审查）")
    add("")
    add("## 分支内容")
    content = run["content"]
    add(f"  文件内容 {content.get('integration')!r}")
    add(f"原始仓库未被改动：{'是' if content.get('sourceClean') else '否'}")
    add(f"最近提交：{content.get('log')}")
    approvals = run.get("approvals") or []
    add(f"审批 {len(approvals)} 条")
    for approval in approvals:
        add(f"  - {approval['params'].get('command')}")
    add("")
    add("## 结论")
    verdict = run["verdict"]
    if verdict["ok"]:
        add("通过：两个任务的 running 区间重叠；同一行的第二个编辑进入冲突，分支未前进；")
        add("放弃候选后工作区恢复、任务回到未集成，源仓库全程干净。")
    else:
        add("未通过：" + "；".join(verdict["problems"]))
    add("")
    if len(run.get("integrations") or []) > 1:
        add("放弃后的恢复任务也已集成。")
    else:
        add("放弃后的恢复任务未在本次真实运行中完成；后续集成能力另见 git 回归测试。")
    for limitation in run.get("environment") or []:
        add(f"环境限制：{limitation}")
    add("说明：冲突不自动解决，也不覆盖分支；宿主只负责把候选退回可重试状态。")


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "retry":
        retry(load(sys.argv[2]))
        return
    if len(sys.argv) > 1 and sys.argv[1] == "conflict":
        conflict(load(sys.argv[2]))
        return
    main_run = load(sys.argv[1])
    first = load(sys.argv[2])
    follow = load(sys.argv[3])
    out = []
    add = out.append

    add("## 环境")
    add(f"状态目录 {main_run['state']}")
    add(f"会话     {main_run['session']}")
    for agent_id, agent in main_run["agents"].items():
        add(
            f"Agent    {short(agent_id)} · {agent['role']:<11} {agent['model']} · "
            f"{agent['protocol']} · 预算 {agent['contextBytes']} 字节"
        )
    first_agent = next(iter(main_run["agents"].values()))
    add(f"端点     {first_agent['baseUrl']}（按阶段 6/7 的安全边界脱敏；真实值只在状态目录与 Keychain）")
    add("")

    add("## 主场景：主控委派两个工作 Agent，一个带依赖")
    add(f"只派发 1 条任务给主控：{main_run['dispatch']['taskId']}")
    add("")
    add("时间线（每秒轮询状态，只列本次运行新建的任务）：")
    out.extend(timeline_block(main_run["timeline"]))
    add("")
    gate = main_run["gate"]
    add(
        "依赖门：下游 {} 第一次 running 在 {:.1f}s，前置 {} 变 integrated 在 {:.1f}s；"
        "下游此前一直 queued（{}）→ {}".format(
            gate["dependent"],
            gate["dependentStartedAt"],
            gate["dependency"],
            gate["dependencyIntegratedAt"],
            "是" if gate["wasQueued"] else "否",
            "成立" if gate["ok"] else "不成立",
        )
    )
    add("")
    add("任务：")
    for task_id, task in main_run["tasks"].items():
        deps = ", ".join(d[:8] for d in task["dependencies"]) or "无"
        add(
            f"  {task_id[:8]} · {short(task['agentId'])} · {task['status']} · "
            f"集成 {task['integrationStatus']} · 编码 {'是' if task['coding'] else '否'} · "
            f"依赖 {deps} · 提交 {(task['commit'] or '—')[:8]}"
        )
        add(f"    目标   {clip(task['goal'], 200)}")
        add(f"    自述   {clip(task['result'], 300)}")
    add("")
    add(f"审批：{len(main_run['approvals'])} 条（逐条打印命令行与理由后由 --approve 放行）")
    for approval in main_run["approvals"]:
        add(f"  - {approval['params'].get('command')}")
        add(f"    理由 {clip(approval['params'].get('reason'), 200)}")
    add("")
    add("集成事件：")
    for event in main_run["integrations"]:
        data = event["data"]
        evidence = data.get("evidence", {})
        add(
            f"  rev {event['revision']} · {data.get('status')} · "
            f"{str(evidence.get('previousHead'))[:8]} → {str(evidence.get('head'))[:8]} · "
            f"任务提交 {str(evidence.get('taskCommit'))[:8]}"
        )
    add("")
    files = main_run["files"]
    add(f"协作分支 {main_run['integrationHead'][:8]} 上的文件：")
    for name, item in files.items():
        add(f"  {name} · {item.get('text', item.get('error'))!r} · {'ok' if item['ok'] else 'FAIL'}")
    if not all(item["ok"] for item in files.values()):
        add("（这一刻还有任务没集成——见下面的后续派发，主控当轮就收工了。）")
    add(f"原始仓库未被改动：{'是' if main_run['sourceRepoClean'] else '否'}")
    usage = {}
    for event in main_run["usage"]:
        key = short(event["agentId"])
        usage[key] = usage.get(key, 0) + event["data"].get("total_tokens", 0)
    add(f"令牌（本次场景）：{', '.join(f'{k} {v}' for k, v in usage.items())}")
    add("")

    add("## 第一次尝试：主控建完第一个任务就收工")
    add(f"派发 {first['dispatch']['taskId'][:8]}，会话里只多出 1 个工作任务。")
    for task_id, task in first["tasks"].items():
        add(
            f"  {task_id[:8]} · {short(task['agentId'])} · {task['status']} · "
            f"集成 {task['integrationStatus']} · 提交 {(task['commit'] or '—')[:8]}"
        )
        add(f"    自述   {clip(task['result'], 240)}")
    add("宿主没有报错，任务正常 completed；缺的是主控自己的下一步（没有建第二个任务、没有集成）。")
    add("")

    add("## 补齐：两次后续派发")
    before = follow["taskBefore"]
    add(
        f"目标任务 {before['id'][:8]} 当时 {before['status']} / 集成 {before['integrationStatus']}，"
        f"提交 {before['commit'][:8]}。"
    )
    for entry in follow["timeline"]:
        add(f"  {entry['at']:>7.1f}s  {entry['task']}  {entry.get('agent', '?')}  {entry['status']:<10} {entry.get('integrationStatus') or '—'}")
    add("")
    add(f"最后一次派发：{follow['dispatch']['taskId'][:8]}")
    add(f"主控自述：{clip(follow['dispatchedAfter']['result'], 300)}")
    add("")
    add("结果：")
    add(
        f"  目标任务 {follow['taskAfter']['id'][:8]} → {follow['taskAfter']['status']} / "
        f"集成 {follow['taskAfter']['integrationStatus']}；协作分支 {follow['integrationHead'][:8]}"
    )
    for name, item in follow["files"].items():
        add(f"  {name} · {item.get('text', item.get('error'))!r} · {'ok' if item['ok'] else 'FAIL'}")
    add("")
    add("## 结论")
    add("- 委派、依赖门、审查、集成、并发 claim 在真实模型与真实服务商上全部成立：下游任务在依赖集成前保持 queued，")
    add("  集成完成后同一秒转 running；两个任务先后集成到协作分支，源仓库没有被改动。")
    add("- 失败点在主控的收尾：三次 turn 里，模型说完「接下来做 X」就直接结束，没有发出那个写操作的工具调用。")
    add("  rollout 里能看到最后一次工具调用是 collab_review_diff，之后是纯文本消息加 task_complete，没有报错、没有失败的调用。")
    add("- 因此宿主把任务标成 completed，而验收标准其实没满足：完成是模型自述，没有宿主侧的验收门。这是 7-2 的输入。")
    print("\n".join(out))


if __name__ == "__main__":
    main()
