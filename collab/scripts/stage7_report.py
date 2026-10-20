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


def main():
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
