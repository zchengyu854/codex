#!/usr/bin/env python3
"""Render `evidence/stage-6-real.log` from the machine-readable run record.

The log is derived, never hand-written: it reads the JSON that `stage6_real.py verify` wrote and
replays the interesting fields, then prints the diff attachment verbatim. Redaction happened in the
recorder, so this script prints `baseUrl` as it finds it (a placeholder).
"""
import json
import sys
from pathlib import Path


def wrap(label, value):
    if value in (None, ""):
        return f"{label} —"
    return f"{label} {value}"


def main():
    record = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    out = []
    add = out.append

    add("## 环境")
    add(wrap("状态目录", record["state"]))
    add(wrap("Agent", record["agent"]))
    add(wrap("会话", record["session"]))
    add(wrap("端点", record["baseUrl"]))
    add(wrap("模型", record["model"]))
    add(wrap("协议", f"{record['protocol']} · 上下文预算 {record['contextBytes']} 字节"))
    add("")

    probe = record["probe"]
    add("## 连接探针（agent/testConnection → agent/connectionResult）")
    add(wrap("任务", probe["taskId"]))
    add(wrap("状态", probe["status"]))
    add(wrap("verified", probe["verified"]))
    add(wrap("能力", " / ".join(probe["capabilities"])))
    add(wrap("结果", (probe["result"] or "").strip()))
    add(wrap("审批", "无" if not probe["approvals"] else str(len(probe["approvals"]))))
    add("")

    task = record["task"]
    add("## 编码任务（模型自建文件 → 宿主提交 → 差异附件）")
    add(wrap("任务", task["taskId"]))
    add(wrap("状态", task["status"]))
    add(wrap("候选提交", task["commit"]))
    add(wrap("基线", task["baseCommit"]))
    add(wrap("集成状态", f"{task['integrationStatus']}（轮询发生在 task/integrate 之前；集成后见元数据与详情快照）"))
    add(wrap("工作区", task["worktree"]))
    add("")
    add("审批（逐条原文；无人值守重放用 --approve 自动应答并记录）：")
    if not task["approvals"]:
        add("- 无")
    for approval in task["approvals"]:
        params = approval["params"]
        add(f"- {approval['method']}")
        add(f"  命令 {params.get('command')}")
        add(f"  理由 {params.get('reason')}")
    add("")
    add("差异（`file diff` 附件原文）：")
    diff = task["artifacts"].get("file diff")
    if diff and Path(diff["path"]).exists():
        body = Path(diff["path"]).read_text(encoding="utf-8").rstrip("\n")
        for line in body.splitlines():
            add(f"  {line}")
        add(f"  （{diff['bytes']} 字节）")
    else:
        add("  （附件不在状态目录里了；交互快照仍保留差异预览）")
    add("")
    add("模型自述（原文，未改写）：")
    for line in (task["result"] or "").strip().splitlines():
        add(f"  {line}" if line.strip() else "")
    add("")
    add("附件：")
    for label, item in task["artifacts"].items():
        add(f"- {label} · {item['bytes']} 字节 · {item['path']}")
    add("")

    integrated = record["integrate"]
    add("## 集成（task/integrate）")
    add(wrap("状态", integrated["status"]))
    add(wrap("前一个提交", integrated["previousHead"]))
    add(wrap("集成后提交", integrated["head"]))
    add(wrap("任务提交", integrated["taskCommit"]))
    add(wrap("重新校验", integrated["revalidated"]))
    for check in integrated["checks"]:
        add(f"- {' '.join(check['command'])} · success={check['success']}")
    add("")

    verdict = record["verdict"]
    add("## 结论")
    add(wrap("real_api_tested", verdict["real_api_tested"]))
    add(wrap("问题", "无" if not verdict["problems"] else "; ".join(verdict["problems"])))
    if record.get("redaction"):
        add(wrap("脱敏", record["redaction"]))
    print("\n".join(out))


if __name__ == "__main__":
    main()
