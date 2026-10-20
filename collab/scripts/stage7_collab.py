#!/usr/bin/env python3
"""Stage 7-1: a real multi-agent run — delegation, the dependency gate, review and integration.

The key never passes through this script. `prepare` seeds a state directory with three fixed agent
ids (one coordinator, two workers) and prints what to press; the operator saves a credential for
each agent in the TUI with Ctrl+K; `verify` then drives the same state through the host.

  python3 stage7_collab.py prepare --core BIN --base-url URL --model M [--protocol P] --state DIR
  python3 stage7_collab.py verify  --core BIN --state DIR --json OUT [--approve] [--timeout 1200]

`verify` dispatches exactly one task, to the coordinator. Everything else — creating the two worker
tasks, waiting, reviewing diffs and integrating — has to come from the model through the collab
tools, which is the point of the run.
"""
import argparse
import json
import os
import subprocess
import sys
import time

COORDINATOR = "77777777-0000-4000-8000-000000000001"
WORKER_A = "77777777-0000-4000-8000-000000000002"
WORKER_B = "77777777-0000-4000-8000-000000000003"
AGENTS = (
    (COORDINATOR, "coordinator"),
    (WORKER_A, "worker"),
    (WORKER_B, "worker"),
)
ACTIVE = ("queued", "running")
GOAL = (
    "Delegate work to the two worker agents listed in the shared roster, then review and integrate "
    "their results. Use collab_create_task twice: the first worker must create a file named a.txt "
    "containing the single word alpha and the second must create b.txt containing the single word "
    "beta; both tasks are coding tasks. Make the b.txt task depend on the a.txt task so it cannot "
    "start before the a.txt result is integrated. Then use collab_wait_task, read each diff with "
    "collab_review_diff and call collab_integrate for a.txt first and b.txt second, passing the "
    "exact commit you saw. Report both commits when both are integrated."
)
ACCEPTANCE = (
    "Both worker tasks are completed and integrated into the collaboration branch, a.txt contains "
    "alpha, b.txt contains beta, and the b.txt task only started after the a.txt task was integrated."
)


class HostError(Exception):
    pass


class Host:
    """One long-lived host process; every call is a JSON line round-trip."""

    def __init__(self, core, state, codex="codex"):
        self.process = subprocess.Popen(
            [core, state, codex],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )
        self.counter = 0

    def call(self, method, params=None):
        self.counter += 1
        self.process.stdin.write(
            json.dumps({"id": self.counter, "method": method, "params": params or {}}) + "\n"
        )
        self.process.stdin.flush()
        while True:
            line = self.process.stdout.readline()
            if not line:
                raise HostError(f"host closed before answering {method}")
            value = json.loads(line)
            if value.get("id") == self.counter:
                if "error" in value:
                    raise HostError(f"{method}: {value['error']}")
                return value["result"]

    def close(self):
        try:
            self.process.stdin.close()
        except OSError:
            pass
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()


def git(path, *args, check=True):
    return subprocess.run(
        ["git", *args], cwd=path, check=check, capture_output=True, text=True
    ).stdout.strip()


def prepare_repo(state):
    repo = os.path.join(state, "repo")
    if os.path.isdir(os.path.join(repo, ".git")):
        return repo
    os.makedirs(repo, exist_ok=True)
    git(repo, "init")
    with open(os.path.join(repo, "README.md"), "w", encoding="utf-8") as handler:
        handler.write("stage 7 base\n")
    identity = ("-c", "user.name=stage7", "-c", "user.email=stage7@localhost")
    git(repo, "add", "README.md")
    git(repo, *identity, "commit", "-m", "init")
    return repo


def find_session(host, agent_id):
    for session in host.call("session/list"):
        for agent in host.call("agent/list", {"sessionId": session["id"]}):
            if agent["id"] == agent_id:
                return session["id"]
    return None


def prepare(args):
    state = args.state
    os.makedirs(state, exist_ok=True)
    repo = prepare_repo(state)
    host = Host(args.core, state, args.codex)
    try:
        session = find_session(host, COORDINATOR)
        if session is None:
            session = host.call(
                "session/create",
                {
                    "repository": repo,
                    "validation": [["/usr/bin/true"]],
                    "requirement": "保留公开接口：只新建文件，不改既有文件；测试通过",
                },
            )["id"]
        configured = []
        for agent_id, role in AGENTS:
            configured.append(
                host.call(
                    "agent/configure",
                    {
                        "id": agent_id,
                        "sessionId": session,
                        "role": role,
                        "model": args.model,
                        "baseUrl": args.base_url,
                        "protocol": args.protocol,
                        "contextBytes": args.context_bytes,
                    },
                )
            )
    finally:
        host.close()

    print(f"state   {state}")
    print(f"session {session}")
    print(f"repo    {repo}")
    for agent in configured:
        print(f"agent   {agent['role']:<11} {agent['id']}")
    print()
    print("下一步（密钥只经过你自己的终端，脚本不接触）：")
    print(f"  1) {args.tui} --state {state} --host {args.core} --session {session}")
    print("  2) 窗格 1（主控）按 Ctrl+K 写入密钥并回车，等到底部提示已保存")
    print("  3) Tab 到窗格 2、窗格 3（两个工作 Agent），各按一次 Ctrl+K 写入同一个 key")
    print("  4) 按 Ctrl+Q 退出（脚本需要独占状态目录）")
    print(
        f"  5) python3 {os.path.relpath(__file__)} verify --core {args.core} "
        f"--state {state} --json collab/evidence/stage-7-collab.json"
    )


def redacted(url):
    """The repository never carries the operator's real endpoint (stage-6/7 boundary)."""
    return "https://<provider>/v1" if url.startswith("https://") else "<loopback>"


def poll_run(host, session, args, evidence):
    """Watch the whole session until every task is terminal, recording a status timeline."""
    deadline = time.monotonic() + args.timeout
    timeline = []
    seen = {}
    approvals = []
    decided = set()
    started = time.monotonic()
    while True:
        if args.approve:
            for approval in host.call("approval/list"):
                if approval["id"] in decided:
                    continue
                decided.add(approval["id"])
                approvals.append(
                    {
                        "taskId": approval["taskId"],
                        "method": approval["method"],
                        "params": approval["params"],
                    }
                )
                print(
                    f"approving {approval['method']} "
                    f"{json.dumps(approval['params'], ensure_ascii=False)[:300]}"
                )
                host.call(
                    "approval/decide", {"approvalId": approval["id"], "accept": True}
                )
        else:
            pending = [
                a
                for a in host.call("approval/list")
                if a.get("decision") is None and a["id"] not in decided
            ]
            if pending:
                first = pending[0]
                raise HostError(
                    "a task is waiting for approval: "
                    f"{first['method']} "
                    f"{json.dumps(first['params'], ensure_ascii=False)[:300]}; "
                    "rerun with --approve or approve it in the TUI"
                )
        tasks = host.call("task/list", {"sessionId": session})
        for task in tasks:
            key = task["id"]
            state = (task["status"], task.get("integrationStatus"))
            if seen.get(key) == state:
                continue
            seen[key] = state
            timeline.append(
                {
                    "at": round(time.monotonic() - started, 1),
                    "task": key[:8],
                    "agent": task["agentId"][:8],
                    "status": task["status"],
                    "integrationStatus": task.get("integrationStatus"),
                }
            )
            print(
                f"{timeline[-1]['at']:>6.1f}s {key[:8]} "
                f"{task['agentId'][:8]} {task['status']} "
                f"{task.get('integrationStatus')}"
            )
        if tasks and all(task["status"] not in ACTIVE for task in tasks):
            return timeline, approvals
        if time.monotonic() >= deadline:
            for task in tasks:
                if task["status"] in ACTIVE:
                    print(f"timeout: cancelling {task['id'][:8]}")
                    try:
                        host.call("task/cancel", {"taskId": task["id"]})
                    except HostError as error:
                        print(f"cancel failed: {error}")
            return timeline, approvals
        time.sleep(args.interval)


def verify(args):
    state = args.state
    host = Host(args.core, state, args.codex)
    evidence = {"state": state, "agents": {}}
    problems = []

    def note(problem):
        problems.append(problem)
        print(f"PROBLEM: {problem}")

    try:
        session = find_session(host, COORDINATOR)
        if session is None:
            raise SystemExit(
                f"{COORDINATOR} 不在 {state} 里；先运行 prepare 并用 Ctrl+K 保存密钥。"
            )
        roster = host.call("agent/list", {"sessionId": session})
        for agent in roster:
            evidence["agents"][agent["id"]] = {
                "role": agent["role"],
                "model": agent["model"],
                "protocol": agent["protocol"],
                "baseUrl": redacted(agent["baseUrl"]),
                "contextBytes": agent["contextBytes"],
            }
        evidence["session"] = session
        print(f"会话 {session[:8]} · {len(roster)} 个 Agent")

        coordinator = host.call(
            "task/create",
            {
                "sessionId": session,
                "agentId": COORDINATOR,
                "goal": GOAL,
                "acceptance": ACCEPTANCE,
                "dependencies": [],
                "coding": False,
            },
        )
        evidence["dispatch"] = {"taskId": coordinator["id"], "goal": GOAL, "acceptance": ACCEPTANCE}
        print(f"派发 {coordinator['id'][:8]} 给主控")

        timeline, approvals = poll_run(host, session, args, evidence)
        evidence["timeline"] = timeline
        evidence["approvals"] = approvals

        tasks = host.call("task/list", {"sessionId": session})
        detail = {}
        for task in tasks:
            detail[task["id"]] = host.call("task/get", {"taskId": task["id"]})
        evidence["tasks"] = detail

        events = host.call("event/list", {"sessionId": session, "after": 0, "limit": 1000})
        evidence["integrations"] = [
            {"revision": e["revision"], "taskId": e.get("taskId"), "data": e["data"]}
            for e in events
            if e["kind"] == "integration"
        ]
        evidence["messages"] = [
            {"revision": e["revision"], "agentId": e.get("agentId"), "text": e["data"].get("text")}
            for e in events
            if e["kind"] == "public_message" and (e["data"].get("text") or "").strip()
        ]
        evidence["usage"] = [
            {"agentId": e.get("agentId"), "data": e["data"]}
            for e in events
            if e["kind"] == "usage"
        ]

        workers = [t for t in detail.values() if t["agentId"] != COORDINATOR]
        coding = [t for t in workers if t["coding"]]
        if len(coding) != len(workers):
            note(
                f"{len(workers) - len(coding)} worker task(s) are not coding, "
                "so integration cannot gate the dependent task"
            )
        if len(workers) != 2:
            note(f"expected two worker tasks, saw {len(workers)}")
        if len({t["agentId"] for t in workers}) != len(workers):
            note("both worker tasks were assigned to the same agent")
        dependent = [t for t in workers if t["dependencies"]]
        if len(dependent) != 1:
            note(f"expected exactly one dependent worker task, saw {len(dependent)}")
        integrated = [
            t for t in workers if t["integrationStatus"] == "integrated"
        ]
        if len(integrated) != len(workers):
            note(f"only {len(integrated)}/{len(workers)} worker tasks are integrated")

        # The gate: the dependent task may not run before its dependency is integrated.
        gate = {"ok": False}
        if dependent:
            task = dependent[0]
            dependency = task["dependencies"][0]
            started = next(
                (e["at"] for e in timeline if e["task"] == task["id"][:8] and e["status"] == "running"),
                None,
            )
            released = next(
                (
                    e["at"]
                    for e in timeline
                    if e["task"] == dependency[:8] and e["integrationStatus"] == "integrated"
                ),
                None,
            )
            waited = any(
                e["task"] == task["id"][:8] and e["status"] == "queued" for e in timeline
            )
            gate = {
                "dependent": task["id"][:8],
                "dependency": dependency[:8],
                "dependentStartedAt": started,
                "dependencyIntegratedAt": released,
                "wasQueued": waited,
                "ok": started is not None and released is not None and started >= released and waited,
            }
            if not gate["ok"]:
                note(f"dependency gate not observed: {gate}")
        else:
            note("no dependent task, so the gate was not exercised")
        evidence["gate"] = gate

        # The collaboration branch, not the source repo, must carry both files.
        integration_worktree = os.path.join(state, "worktrees", session, "integration")
        files = {}
        for name, expected in (("a.txt", "alpha"), ("b.txt", "beta")):
            path = os.path.join(integration_worktree, name)
            try:
                with open(path, encoding="utf-8") as handler:
                    body = handler.read().strip()
                files[name] = {"text": body, "ok": body == expected}
                if body != expected:
                    note(f"{name} in the integration worktree is {body!r}, expected {expected!r}")
            except OSError as error:
                files[name] = {"error": str(error), "ok": False}
                note(f"{name} missing from the integration worktree: {error}")
        evidence["files"] = files
        evidence["integrationHead"] = git(integration_worktree, "rev-parse", "HEAD", check=False)
        evidence["sourceRepoClean"] = (
            git(os.path.join(state, "repo"), "status", "--porcelain", check=False) == ""
        )
        if not evidence["sourceRepoClean"]:
            note("the source repository was modified in place")
        evidence["integrationLog"] = git(
            integration_worktree, "log", "--oneline", "-4", check=False
        ).splitlines()
    finally:
        host.close()

    evidence["verdict"] = {"ok": not problems, "problems": problems}
    with open(args.json, "w", encoding="utf-8") as handler:
        json.dump(evidence, handler, ensure_ascii=False, indent=2)
        handler.write("\n")
    print(f"wrote {args.json}")
    if problems:
        print("FAIL: " + "; ".join(problems))
        return 1
    print(
        "PASS: two worker tasks integrated, dependency gate held, collaboration branch has both files"
    )
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    setup = sub.add_parser("prepare", help="seed a state directory with three agents")
    setup.add_argument("--core", required=True)
    setup.add_argument("--tui", default="collab/target/debug/codex-collab-tui")
    setup.add_argument("--state", required=True)
    setup.add_argument("--base-url", required=True)
    setup.add_argument("--model", required=True)
    setup.add_argument("--protocol", default="chat_completions")
    setup.add_argument("--context-bytes", type=int, default=128_000)
    setup.add_argument("--codex", default="codex")
    setup.set_defaults(handler=prepare)

    check = sub.add_parser("verify", help="run the delegation scenario and record evidence")
    check.add_argument("--core", required=True)
    check.add_argument("--state", required=True)
    check.add_argument("--json", default="collab/evidence/stage-7-collab.json")
    check.add_argument("--timeout", type=float, default=1200)
    check.add_argument("--interval", type=float, default=1.5)
    check.add_argument("--approve", action="store_true")
    check.add_argument("--codex", default="codex")
    check.set_defaults(handler=verify)
    args = parser.parse_args()
    sys.exit(args.handler(args))


if __name__ == "__main__":
    main()
