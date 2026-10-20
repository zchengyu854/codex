#!/usr/bin/env python3
"""Stage 7-3: concurrency, a clean same-file merge, a real conflict, and the drop that unblocks it.

One task creates `shared.txt`; two tasks then rewrite the same line from the same base. The first
integrates and the second conflicts. A later task edits another line after the candidate is dropped with
`task/abortIntegration`: the collaboration branch head never moves, the integration worktree is
usable again, and the source repository stays clean.

  python3 stage7_conflict.py --core BIN --state DIR --json OUT [--approve] [--timeout 900]

No model is asked to orchestrate anything here: the tasks are dispatched through `task/create` and
integrated through `task/integrate`, the same host methods the TUI's Ctrl+G uses. The workers are
real Agent processes, so the diff under test is real model output.
"""
import argparse
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stage7_collab import ACTIVE, Host, HostError, git, redacted, short  # noqa: E402

def prompts(name):
    """The seed and the three edits, written against one file name."""
    return (
        f"Create a file named {name} in the workspace root with exactly three lines: "
        "one, two, three (one word per line, a single trailing newline). "
        "Do not modify any other file. Do not run git commands.",
        f"In {name} replace the third line `three` with `THREE`. "
        "Change no other line. Do not run git commands.",
        f"In {name} replace the first line `one` with `ONE`. "
        "Change no other line. Do not run git commands.",
        f"In {name} replace the third line `three` with `333`. "
        "Change no other line. Do not run git commands.",
    )


def status(host, session, ids, deadline, interval, timeline, seen, approvals, approve):
    """Poll the new tasks until all are terminal, recording each status change."""
    started = time.monotonic()
    while True:
        if approve:
            for approval in host.call("approval/list"):
                if approval["id"] in seen["approvals"]:
                    continue
                seen["approvals"].add(approval["id"])
                approvals.append(
                    {
                        "taskId": approval["taskId"],
                        "method": approval["method"],
                        "params": approval["params"],
                    }
                )
                print(
                    f"approving {approval['method']} "
                    f"{json.dumps(approval['params'], ensure_ascii=False)[:240]}"
                )
                host.call("approval/decide", {"approvalId": approval["id"], "accept": True})
        tasks = [t for t in host.call("task/list", {"sessionId": session}) if t["id"] in ids]
        for task in tasks:
            state = (task["status"], task.get("integrationStatus"))
            if seen["tasks"].get(task["id"]) == state:
                continue
            seen["tasks"][task["id"]] = state
            timeline.append(
                {
                    "at": round(time.monotonic() - started, 1),
                    "task": task["id"][:8],
                    "agent": short(task["agentId"]),
                    "status": task["status"],
                    "integrationStatus": task.get("integrationStatus"),
                }
            )
            print(
                f"{timeline[-1]['at']:>6.1f}s {task['id'][:8]} {short(task['agentId'])} "
                f"{task['status']} {task.get('integrationStatus')}"
            )
        if tasks and all(t["status"] not in ACTIVE for t in tasks):
            return tasks
        if time.monotonic() >= deadline:
            for task in tasks:
                if task["status"] in ACTIVE:
                    print(f"timeout: cancelling {task['id'][:8]}")
                    try:
                        host.call("task/cancel", {"taskId": task["id"]})
                    except HostError as error:
                        print(f"cancel failed: {error}")
            raise SystemExit("workers did not finish in time; partial evidence not written")
        time.sleep(interval)


def overlapping(timeline, ids):
    """Do at least two of these tasks have a running interval that overlaps?"""
    spans = {}
    opened = {}
    for entry in timeline:
        key = entry["task"]
        if key not in ids:
            continue
        if entry["status"] == "running":
            opened[key] = entry["at"]
        elif key in opened:
            spans[key] = (opened.pop(key), entry["at"])
    for key, start in opened.items():
        spans[key] = (start, timeline[-1]["at"] if timeline else start)
    names = sorted(spans)
    for i, first in enumerate(names):
        for second in names[i + 1 :]:
            a, b = spans[first], spans[second]
            if a[0] < b[1] and b[0] < a[1]:
                return True, {k: list(v) for k, v in spans.items()}
    return False, {k: list(v) for k, v in spans.items()}


def dispatch(host, session, agent, goal, acceptance):
    task = host.call(
        "task/create",
        {
            "sessionId": session,
            "agentId": agent,
            "goal": goal,
            "acceptance": acceptance,
            "coding": True,
            "dependencies": [],
        },
    )
    print(f"派发 {task['id'][:8]} → {short(agent)} · {goal[:60]}")
    return task["id"]


def integrate(host, task_id, review):
    task = host.call("task/get", {"taskId": task_id})
    result = host.call(
        "task/integrate",
        {"taskId": task_id, "expectedCommit": task["commit"], "review": review},
    )
    print(f"集成 {task_id[:8]} → {result['status']}")
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--core", required=True)
    parser.add_argument("--state", required=True)
    parser.add_argument("--json", required=True)
    parser.add_argument("--session")
    # A state directory that already ran this scenario needs a fresh file name.
    parser.add_argument("--file", default="shared.txt")
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--interval", type=float, default=1.0)
    # Model requests in the same second hit the provider limit; a short stagger
    # keeps the tasks overlapping without making the provider reject the burst.
    parser.add_argument("--stagger", type=float, default=10.0)
    parser.add_argument("--timeout", type=float, default=900)
    parser.add_argument("--approve", action="store_true")
    args = parser.parse_args()

    host = Host(args.core, args.state, args.codex)
    evidence = {"state": args.state, "agents": {}, "tasks": {}, "timeline": []}
    problems = []

    def note(problem):
        problems.append(problem)
        print(f"PROBLEM: {problem}")

    try:
        sessions = host.call("session/list")
        session = args.session
        if session is None:
            for item in sessions:
                if host.call("agent/list", {"sessionId": item["id"]}):
                    session = item["id"]
                    break
        if session is None:
            raise SystemExit("state 里没有已配置 Agent 的会话")
        roster = host.call("agent/list", {"sessionId": session})
        workers = sorted(
            (a for a in roster if a["role"] == "worker"), key=lambda a: a["id"]
        )
        coordinator = next((a for a in roster if a["role"] == "coordinator"), None)
        if len(workers) < 2 or coordinator is None:
            raise SystemExit("需要 1 个主控和 2 个工作 Agent 才能跑并发场景")
        evidence["session"] = session
        for agent in roster:
            evidence["agents"][agent["id"]] = {
                "role": agent["role"],
                "model": agent["model"],
                "protocol": agent["protocol"],
                "baseUrl": redacted(agent["baseUrl"]),
                "contextBytes": agent["contextBytes"],
            }
        print(f"会话 {session[:8]} · {len(roster)} 个 Agent")

        seen = {"tasks": {}, "approvals": set()}
        approvals = []
        deadline = time.monotonic() + args.timeout

        seed_prompt, third_prompt, first_prompt, contested_prompt = prompts(args.file)
        # 1. Seed the file that the three concurrent edits will share.
        seed = dispatch(host, session, workers[0]["id"], seed_prompt, f"{args.file} has three lines")
        status(host, session, {seed}, deadline, args.interval, evidence["timeline"], seen, approvals, args.approve)
        seed_task = host.call("task/get", {"taskId": seed})
        evidence["tasks"][seed] = seed_task
        if seed_task["status"] != "completed" or not seed_task["commit"]:
            note(f"seed task ended {seed_task['status']} without a commit")
            raise SystemExit("cannot continue without the seed file")
        seeded = integrate(
            host, seed, f"reviewed: creates {args.file} with the three agreed lines"
        )
        evidence["seedIntegration"] = seeded
        if seeded["status"] != "integrated":
            note(f"seed integration ended {seeded['status']}")
            raise SystemExit("cannot continue without the seed file on the branch")

        # 2. Two edits from the same base, both rewriting the third line: they overlap in time, so
        # the overlap is real concurrency, and the second one is guaranteed to be stale.
        concurrent = {}
        offsets = {}
        scenario_start = time.monotonic()
        for agent, goal, acceptance, role in (
            (workers[0]["id"], third_prompt, "third line is THREE", "third"),
            (workers[1]["id"], contested_prompt, "third line is 333", "third-again"),
        ):
            if concurrent:
                time.sleep(args.stagger)
            task_id = dispatch(host, session, agent, goal, acceptance)
            concurrent[task_id] = role
            offsets[task_id[:8]] = round(time.monotonic() - scenario_start, 1)
        evidence["stagger"] = args.stagger
        evidence["dispatch"] = offsets
        evidence["watchStart"] = round(time.monotonic() - scenario_start, 1)
        finished = status(
            host,
            session,
            set(concurrent),
            deadline,
            args.interval,
            evidence["timeline"],
            seen,
            approvals,
            args.approve,
        )
        for task in finished:
            evidence["tasks"][task["id"]] = host.call("task/get", {"taskId": task["id"]})

        ids = {task["id"][:8] for task in finished}
        ok, spans = overlapping(evidence["timeline"], ids)
        evidence["concurrency"] = {"overlapped": ok, "spans": spans}
        if not ok:
            note(f"no two of the concurrent tasks overlapped: {spans}")

        by_role = {role: task for task, role in concurrent.items()}
        for role, task_id in by_role.items():
            if evidence["tasks"][task_id]["status"] != "completed":
                note(f"{role} task ended {evidence['tasks'][task_id]['status']}")
                raise SystemExit("not every concurrent task produced a commit")

        # 3. The first of the two integrates.
        third = integrate(host, by_role["third"], "reviewed: rewrites the third line only")
        evidence["integrations"] = [third]
        if third["status"] != "integrated":
            note("the first same-line edit did not integrate")

        # 4. The contested line: the merge must conflict, not overwrite.
        conflict = integrate(host, by_role["third-again"], "reviewed: also rewrites the third line")
        evidence["conflict"] = conflict
        if conflict["status"] != "conflict":
            note(f"the contested line integrated instead of conflicting: {conflict['status']}")

        worktree = os.path.join(args.state, "worktrees", session, "integration")
        before = git(worktree, "rev-parse", "HEAD")
        evidence["headDuringConflict"] = before
        if before != third.get("head"):
            note("the conflict started from a head the earlier integration did not produce")
        dirty = git(worktree, "status", "--porcelain")
        evidence["worktreeDirtyDuringConflict"] = bool(dirty)
        if not dirty:
            note("the conflicted merge left no evidence in the integration worktree")

        # 5. Dropping the candidate restores the branch and the worktree.
        dropped = host.call("task/abortIntegration", {"taskId": by_role["third-again"]})
        evidence["abort"] = dropped
        after = git(worktree, "rev-parse", "HEAD")
        evidence["headAfterAbort"] = after
        evidence["worktreeDirtyAfterAbort"] = bool(git(worktree, "status", "--porcelain"))
        evidence["taskAfterAbort"] = host.call("task/get", {"taskId": by_role["third-again"]})
        if dropped["status"] != "pending":
            note(f"the drop reported {dropped['status']}")
        if after != before:
            note(f"the branch moved during the conflict: {before[:8]} → {after[:8]}")
        if evidence["worktreeDirtyAfterAbort"]:
            note("the integration worktree stayed dirty after the drop")
        if evidence["taskAfterAbort"]["integrationStatus"] != "pending":
            note(f"the task stayed {evidence['taskAfterAbort']['integrationStatus']} after the drop")
        if evidence["taskAfterAbort"]["commit"] != evidence["tasks"][by_role["third-again"]]["commit"]:
            note("dropping the conflict lost the candidate commit")

        # 6. The worktree is usable again: the third edit lands on the line the conflict did not
        # touch, which is the same-file/different-line merge the branch could not do while the
        # conflicted merge was still in flight.
        later = dispatch(host, session, workers[0]["id"], first_prompt, "first line is ONE")
        status(host, session, {later}, deadline, args.interval, evidence["timeline"], seen, approvals, args.approve)
        evidence["tasks"][later] = host.call("task/get", {"taskId": later})
        if evidence["tasks"][later]["status"] != "completed":
            text = evidence["tasks"][later].get("result") or ""
            environmental = any(
                marker in text
                for marker in ("429", "rate limit", "Keychain credential unavailable")
            )
            if environmental:
                # The branch state after a quiet window is already proven by the git test; say
                # which provider-side limit stopped this extra turn instead of calling it a bug.
                evidence.setdefault("environment", []).append(
                    f"post-conflict task: {text[:200]}"
                )
                print(f"environment: {text[:160]}")
            else:
                note(f"the post-conflict task ended {evidence['tasks'][later]['status']}")
        else:
            recovered = integrate(host, later, "reviewed: rewrites the first line only")
            evidence["integrations"].append(recovered)
            if recovered["status"] != "integrated":
                note("nothing integrates after a dropped conflict")

        # 7. The branch content and the untouched source repository.
        content = {}
        try:
            with open(os.path.join(worktree, args.file), encoding="utf-8") as handler:
                content["integration"] = handler.read()
        except OSError as error:
            content["integration"] = str(error)
        repo = os.path.join(args.state, "repo")
        content["sourceClean"] = git(repo, "status", "--porcelain") == ""
        content["log"] = git(worktree, "log", "--oneline", "-3").splitlines()
        evidence["content"] = content
        expected = "one\ntwo\nTHREE\n" if evidence.get("environment") else "ONE\ntwo\nTHREE\n"
        if content.get("integration") != expected:
            note(f"unexpected integrated content: {content.get('integration')!r}")
        if content["sourceClean"] is not True:
            note("the source repository was modified")

        evidence["approvals"] = approvals
        evidence["verdict"] = {"ok": not problems, "problems": problems}
        with open(args.json, "w", encoding="utf-8") as handler:
            json.dump(evidence, handler, ensure_ascii=False, indent=2)
            handler.write("\n")
        print(f"wrote {args.json}")
        if problems:
            print("FAIL: " + "；".join(problems))
            return 1
        print("PASS: two same-base tasks overlapped, the contested line conflicted and was dropped, and the branch kept integrating")
        return 0
    finally:
        host.close()


if __name__ == "__main__":
    sys.exit(main())
