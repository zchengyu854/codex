#!/usr/bin/env python3
"""Prepare and verify the bounded real-model stage-8 project and conflict scenarios.

Credentials stay in Keychain. The script seeds only a synthetic Python project; `verify` dispatches
one coordinator task and records model-created task, integration, validation, usage and approval data.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import stage7_collab as stage7  # noqa: E402

FIXTURE = os.path.normpath(
    os.path.join(os.path.dirname(__file__), "../fixtures/stage8-project")
)
CHECK = ["/usr/bin/python3", "-B", "-m", "unittest", "discover", "-s", "tests", "-v"]

Host = stage7.Host
COORDINATOR = stage7.COORDINATOR
AGENTS = stage7.AGENTS


def prepare(args):
    os.makedirs(args.state, exist_ok=True)
    repo = os.path.join(args.state, "repo")
    if os.path.exists(os.path.join(repo, ".git")):
        raise SystemExit(f"refusing to reuse an existing repository: {repo}")
    shutil.copytree(os.path.join(FIXTURE, "base"), repo)
    subprocess.run(["git", "init", repo], check=True, capture_output=True)
    subprocess.run(["git", "-C", repo, "add", "."], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            repo,
            "-c",
            "user.name=stage8",
            "-c",
            "user.email=stage8@localhost",
            "commit",
            "-m",
            "stage 8 baseline",
        ],
        check=True,
        capture_output=True,
    )
    host = Host(args.core, args.state, args.codex)
    try:
        session = stage7.find_session(host, COORDINATOR)
        if session is None:
            session = host.call(
                "session/create",
                {
                    "repository": repo,
                    "validation": [CHECK],
                    "requirement": "Preserve source checkout; preserve existing tests; implement and test only requested behavior.",
                },
            )["id"]
        agents = [
            host.call(
                "agent/configure",
                {
                    "id": aid,
                    "sessionId": session,
                    "role": role,
                    "model": args.model,
                    "baseUrl": args.base_url,
                    "protocol": args.protocol,
                    "contextBytes": args.context_bytes,
                },
            )
            for aid, role in AGENTS
        ]
    finally:
        host.close()
    print(f"state {args.state}\nsession {session}\nrepo {repo}")
    for agent in agents:
        print(f"agent {agent['role']} {agent['id']}")
    print(f"\n{args.tui} --state {args.state} --host {args.core} --session {session}")
    print("在主控和两个 worker 窗格分别按 Ctrl+K 保存凭证；完成后按 Ctrl+Q 退出 TUI。")


def project_goal():
    return """Implement the complete stage-8 synthetic Python project through collaboration tools in this one turn. Do not stop after delegation.
Create two independent coding worker tasks: (1) improve slug(text) to trim, lowercase, and collapse all whitespace to hyphens, preserving Unicode; add tests; (2) make total(values) accept integers and integer strings and raise ValueError for invalid strings; add tests. Assign different workers. Then create a coding report task depending on BOTH task ids; it adds report.py, a CLI combining slug and total, and tests. Do not write patches yourself. Wait until dependencies are integrated before the report task runs. For each task, wait, inspect the exact diff, and integrate only after review. Preserve all existing tests. After that scenario passes, create two coding worker tasks from the same current base, assigned to different workers, both changing the single return expression in total.py to equivalent implementations: one uses a generator expression with int(), the other uses map(int, ...). Wait for both, integrate the generator version, then integrate the map version and confirm it conflicts. Explicitly call task/abortIntegration for that conflict, preserving its candidate. Then create a NEW coding task that references the conflicted task id in its goal and changes total.py to the map implementation, review and integrate that new task. Finish only after the project suite passes and every worker task except the deliberately dropped conflict candidate is integrated. Do not write patches yourself."""


def run(args):
    host = Host(args.core, args.state, args.codex)
    started = time.monotonic()
    evidence = {
        "mode": "real_provider_stage8_project",
        "real_provider_tested": False,
        "task_dispatched": False,
        "state": args.state,
        "startedAt": datetime.now(timezone.utc).isoformat(),
        "agents": {},
        "problems": [],
    }
    try:
        session = stage7.find_session(host, COORDINATOR)
        if session is None:
            raise SystemExit(
                "no prepared stage-8 session; run prepare and save credentials first"
            )
        roster = host.call("agent/list", {"sessionId": session})
        evidence["session"] = session
        for a in roster:
            evidence["agents"][a["id"]] = {
                "role": a["role"],
                "model": a["model"],
                "protocol": a["protocol"],
                "baseUrl": stage7.redacted(a["baseUrl"]),
            }
        repo = os.path.join(args.state, "repo")
        baseline = subprocess.run(CHECK, cwd=repo, capture_output=True, text=True)
        evidence["baseline"] = {
            "exitCode": baseline.returncode,
            "stdout": baseline.stdout,
            "stderr": baseline.stderr,
        }
        if baseline.returncode:
            raise RuntimeError("synthetic project baseline check failed")
        before = {t["id"] for t in host.call("task/list", {"sessionId": session})}
        evidence["priorTaskIds"] = sorted(before)
        goal = project_goal()
        dispatched = host.call(
            "task/create",
            {
                "sessionId": session,
                "agentId": COORDINATOR,
                "goal": goal,
                "acceptance": "The dependency gate held, five coding tasks are integrated, one same-base candidate conflicted and was explicitly dropped, a new task references and repairs it, and trusted project tests pass.",
                "dependencies": [],
                "coding": False,
            },
        )
        evidence["task_dispatched"] = True
        evidence["dispatch"] = {"taskId": dispatched["id"], "goal": goal}
        timeline, approvals = stage7.poll_run(host, session, args, evidence)
        if evidence.get("taskLimitExceeded"):
            evidence["problems"].append(
                "task limit exceeded; active tasks were cancelled"
            )
        evidence["timeline"] = [
            e for e in timeline if e["task"] not in {i[:8] for i in before}
        ]
        evidence["approvals"] = approvals
        tasks = [
            t
            for t in host.call("task/list", {"sessionId": session})
            if t["id"] not in before
        ]
        evidence["tasks"] = {
            t["id"]: host.call("task/get", {"taskId": t["id"]}) for t in tasks
        }
        ids = set(evidence["tasks"])
        events = host.call(
            "event/list", {"sessionId": session, "after": 0, "limit": 1000}
        )
        evidence["integrations"] = [
            {"revision": e["revision"], "taskId": e.get("taskId"), "data": e["data"]}
            for e in events
            if e["kind"] == "integration" and e.get("taskId") in ids
        ]
        evidence["usage"] = [
            {"agentId": e.get("agentId"), "taskId": e.get("taskId"), "data": e["data"]}
            for e in events
            if e["kind"] == "usage" and e.get("taskId") in ids
        ]
        evidence["completedModelResponses"] = len(evidence["usage"])
        evidence["real_provider_tested"] = bool(evidence["usage"])
        evidence["providerResponseObserved"] = evidence["real_provider_tested"]
        coordinator = evidence["tasks"].get(dispatched["id"])
        if coordinator and coordinator["status"] != "completed":
            evidence["problems"].append(
                f"coordinator ended {coordinator['status']}: {coordinator.get('result', '')}"
            )
        workers = [t for t in evidence["tasks"].values() if t["agentId"] != COORDINATOR]
        dependent = [t for t in workers if len(t["dependencies"]) == 2]
        conflict_ids = {
            e["taskId"]
            for e in evidence["integrations"]
            if e.get("data", {}).get("status") == "conflict"
        }
        dropped = [
            t
            for t in workers
            if t["id"] in conflict_ids and t["integrationStatus"] == "pending"
        ]
        integrated = [t for t in workers if t["integrationStatus"] == "integrated"]
        conflict_peers = [
            t
            for t in workers
            if dropped
            and t["id"] != dropped[0]["id"]
            and t["baseCommit"] == dropped[0]["baseCommit"]
        ]
        if (
            len(workers) != 6
            or len(dependent) != 1
            or len(integrated) != 5
            or len(dropped) != 1
            or len(conflict_peers) != 1
            or conflict_peers[0]["integrationStatus"] != "integrated"
            or any(not t["coding"] for t in workers)
        ):
            evidence["problems"].append(
                "expected five integrated coding workers, one dropped conflict candidate, and one two-dependency report task"
            )
        rework = [t for t in workers if dropped and dropped[0]["id"] in t["goal"]]
        evidence["conflictRework"] = {
            "droppedTask": dropped[0]["id"] if dropped else None,
            "reworkTasks": [t["id"] for t in rework],
            "reworkIntegrated": len(rework) == 1
            and rework[0]["integrationStatus"] == "integrated",
            "conflictRecorded": any(
                e.get("data", {}).get("status") == "conflict"
                for e in evidence["integrations"]
            ),
            "dropRecorded": any(
                e.get("data", {}).get("status") == "pending"
                for e in evidence["integrations"]
            ),
        }
        if (
            not evidence["conflictRework"]["conflictRecorded"]
            or not evidence["conflictRework"]["dropRecorded"]
            or not evidence["conflictRework"]["reworkIntegrated"]
        ):
            evidence["problems"].append(
                "real conflict, explicit drop, or task-referencing rework was not recorded"
            )
        gate = {"ok": False}
        if dependent:
            t = dependent[0]
            start = next(
                (
                    e["at"]
                    for e in evidence["timeline"]
                    if e["task"] == t["id"][:8] and e["status"] == "running"
                ),
                None,
            )
            releases = [
                next(
                    (
                        e["at"]
                        for e in evidence["timeline"]
                        if e["task"] == dep[:8]
                        and e["integrationStatus"] == "integrated"
                    ),
                    None,
                )
                for dep in t["dependencies"]
            ]
            gate = {
                "task": t["id"][:8],
                "startedAt": start,
                "dependencyIntegratedAt": releases,
                "ok": start is not None
                and all(r is not None and start >= r for r in releases),
            }
        evidence["gate"] = gate
        if not gate["ok"]:
            evidence["problems"].append(
                "the two-dependency release gate was not observed"
            )
        integration = os.path.join(args.state, "worktrees", session, "integration")
        # Run the fixture's trusted tests only after the complete model scenario produced an integrated project.
        if len(integrated) == 5 and len(dropped) == 1:
            with tempfile.TemporaryDirectory(prefix="stage8-oracle-") as tmp:
                oracle = os.path.join(tmp, "project")
                shutil.copytree(
                    integration, oracle, ignore=shutil.ignore_patterns(".git")
                )
                shutil.rmtree(os.path.join(oracle, "tests"))
                for name in ("slug", "total", "report"):
                    shutil.copytree(
                        os.path.join(FIXTURE, "changes", name, "tests"),
                        os.path.join(oracle, "tests"),
                        dirs_exist_ok=True,
                    )
                checked = subprocess.run(
                    CHECK, cwd=oracle, capture_output=True, text=True
                )
                evidence["oracle"] = {
                    "command": CHECK,
                    "exitCode": checked.returncode,
                    "stdout": checked.stdout,
                    "stderr": checked.stderr,
                }
            if checked.returncode:
                evidence["problems"].append(
                    "trusted fixture oracle rejected the integrated code"
                )
        else:
            evidence["oracle"] = {
                "skipped": "the complete model scenario did not produce its expected integrated tasks"
            }
        evidence["sourceRepoClean"] = (
            subprocess.run(
                ["git", "status", "--porcelain"],
                cwd=repo,
                capture_output=True,
                text=True,
                check=True,
            ).stdout
            == ""
        )
        if not evidence["sourceRepoClean"]:
            evidence["problems"].append("source repository was modified")
    except (Exception, SystemExit) as error:
        evidence["runtimeError"] = str(error)
        evidence["problems"].append("run ended before all evidence could be collected")
    finally:
        evidence["elapsedSeconds"] = round(time.monotonic() - started, 1)
        evidence["finishedAt"] = datetime.now(timezone.utc).isoformat()
        host.close()
    evidence["verdict"] = {
        "ok": not evidence["problems"],
        "problems": evidence["problems"],
    }
    os.makedirs(os.path.dirname(os.path.abspath(args.json)), exist_ok=True)
    with open(args.json, "w", encoding="utf-8") as f:
        json.dump(evidence, f, ensure_ascii=False, indent=2)
        f.write("\n")
    print(f"wrote {args.json}")
    if evidence["problems"]:
        print("FAIL: " + "; ".join(evidence["problems"]))
        return 1
    print(
        "PASS: real model project run, dependency gate, trusted oracle and clean source"
    )
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("prepare")
    p.add_argument("--core", required=True)
    p.add_argument("--state", required=True)
    p.add_argument("--base-url", required=True)
    p.add_argument("--model", required=True)
    p.add_argument("--protocol", default="chat_completions")
    p.add_argument("--context-bytes", type=int, default=128_000)
    p.add_argument("--tui", default="collab/target/debug/codex-collab-tui")
    p.add_argument("--codex", default="codex")
    p.set_defaults(handler=prepare)
    p = sub.add_parser("verify")
    p.add_argument("--core", required=True)
    p.add_argument("--state", required=True)
    p.add_argument("--json", default="collab/evidence/stage-8-real.json")
    p.add_argument("--timeout", type=float, default=1200)
    p.add_argument("--interval", type=float, default=1.5)
    p.add_argument("--max-tasks", type=int, default=7)
    p.add_argument("--approve", action="store_true")
    p.add_argument("--codex", default="codex")
    p.set_defaults(handler=run)
    args = parser.parse_args()
    sys.exit(args.handler(args))


if __name__ == "__main__":
    main()
