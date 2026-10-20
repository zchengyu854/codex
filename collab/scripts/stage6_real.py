#!/usr/bin/env python3
"""Stage 6-4: verify a real provider and the Keychain credential through the host.

The API key never passes through this script. `prepare` seeds a state directory with a fixed agent
id and prints the TUI command; the operator saves the credential there with Ctrl+K; `verify` then
drives the same state through the host, which reads the credential from the Keychain itself.

  python3 stage6_real.py prepare --core BIN --base-url URL --model M --protocol P
  python3 stage6_real.py verify  --core BIN [--timeout 600] [--json OUT]
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile
import time

AGENT_ID = "11111111-2222-4333-8444-555555555555"
DEFAULT_STATE = os.path.join(tempfile.gettempdir(), "codex-collab-stage6-live")
TERMINAL = ("completed", "failed", "cancelled", "interrupted", "blocked")


class HostError(Exception):
    pass


class Host:
    """One long-lived host process, so a probe can be polled across many calls."""

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


def git(repo, *args):
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, capture_output=True, text=True
    ).stdout.strip()


def prepare_repo(state):
    repo = os.path.join(state, "repo")
    if os.path.isdir(os.path.join(repo, ".git")):
        return repo
    os.makedirs(repo, exist_ok=True)
    git(repo, "init")
    identity = ("-c", "user.name=stage6", "-c", "user.email=stage6@localhost")
    git(repo, *identity, "add", ".")
    git(repo, *identity, "commit", "--allow-empty", "-m", "init")
    return repo


def find_agent(host, agent_id):
    """Return (session id, agent) for the seeded agent, or (None, None)."""
    for session in host.call("session/list"):
        for agent in host.call("agent/list", {"sessionId": session["id"]}):
            if agent["id"] == agent_id:
                return session["id"], agent
    return None, None


def prepare(args):
    state = args.state
    os.makedirs(state, exist_ok=True)
    repo = prepare_repo(state)
    host = Host(args.core, state, args.codex)
    try:
        session, agent = find_agent(host, AGENT_ID)
        if session is None:
            sessions = host.call("session/list")
            if sessions:
                session = sessions[0]["id"]
            else:
                session = host.call(
                    "session/create",
                    {"repository": repo, "validation": [["/usr/bin/true"]]},
                )["id"]
        configured = host.call(
            "agent/configure",
            {
                "id": AGENT_ID,
                "sessionId": session,
                "role": "coordinator",
                "model": args.model,
                "baseUrl": args.base_url,
                "protocol": args.protocol,
                "contextBytes": args.context_bytes,
            },
        )
        events = host.call(
            "event/list", {"sessionId": session, "after": 0, "limit": 1000}
        )
        if not any(event["kind"] == "requirement" for event in events):
            host.call(
                "event/publish",
                {
                    "sessionId": session,
                    "kind": "requirement",
                    "data": "真实上游验证：保留公开接口，测试通过",
                },
            )
    finally:
        host.close()

    print(f"state   {state}")
    print(f"session {session}")
    print(f"repo    {repo}")
    print(
        f"agent   {configured['id']} · {configured['model']} · "
        f"{configured['protocol']} · {configured['baseUrl']}"
    )
    print()
    print("下一步（密钥只经过你自己的终端）：")
    print(f"  1) {args.tui} --state {state} --host {args.core} --session {session}")
    print("  2) 按 Ctrl+K 输入 API Key 并回车，等到底部提示已保存")
    print("  3) 按 Ctrl+Q 退出")
    print(f"  4) python3 {os.path.relpath(__file__)} verify --core {args.core} --state {state}")
    if agent is not None:
        print()
        print("（该 Agent 已存在，本次只更新了模型/端点/协议。）")


def poll_task(host, task_id, timeout, interval, label):
    deadline = time.monotonic() + timeout
    last = None
    while True:
        task = host.call("task/get", {"taskId": task_id})
        if task["status"] != last:
            print(f"{label}: {task['status']}")
            last = task["status"]
        if task["status"] in TERMINAL or time.monotonic() >= deadline:
            return task
        time.sleep(interval)


def read_artifacts(host, session, task_id):
    events = host.call(
        "event/list", {"sessionId": session, "after": 0, "limit": 1000}
    )
    found = {}
    for event in events:
        if event["kind"] != "artifact" or event.get("taskId") != task_id:
            continue
        data = event["data"]
        path = data.get("path")
        if not path:
            continue
        try:
            with open(path, encoding="utf-8") as handler:
                text = handler.read()
        except OSError as error:
            text = f"<unreadable: {error}>"
        found[data.get("label", "attachment")] = {
            "path": path,
            "bytes": data.get("bytes", 0),
            "text": text,
        }
    return found


def verify(args):
    state = args.state
    host = Host(args.core, state, args.codex)
    evidence = {"state": state, "agent": AGENT_ID}
    problems = []
    try:
        session, agent = find_agent(host, AGENT_ID)
        if session is None:
            raise SystemExit(
                f"{AGENT_ID} 不在 {state} 里；先运行 prepare 并用 Ctrl+K 保存密钥。"
            )
        evidence.update(
            {
                "session": session,
                "baseUrl": agent["baseUrl"],
                "model": agent["model"],
                "protocol": agent["protocol"],
                "contextBytes": agent["contextBytes"],
            }
        )
        print(
            f"会话 {session[:8]} · Agent {AGENT_ID[:8]} · {agent['model']} · "
            f"{agent['protocol']} · {agent['baseUrl']}"
        )

        probe = host.call("agent/testConnection", {"agentId": AGENT_ID})
        result = poll_task(host, probe["id"], args.timeout, args.interval, "probe")
        verified = host.call("agent/connectionResult", {"taskId": probe["id"]})
        evidence["probe"] = {
            "taskId": probe["id"],
            "status": result["status"],
            "verified": verified["verified"],
            "capabilities": verified["capabilities"],
            "result": result.get("result"),
        }
        print(f"probe verified={verified['verified']} caps={verified['capabilities']}")
        if not verified["verified"]:
            problems.append(
                "connection probe did not verify text+stream+function_tools"
            )

        task = host.call(
            "task/create",
            {
                "sessionId": session,
                "agentId": AGENT_ID,
                "goal": (
                    "Create a file named hello.txt in the current working directory "
                    "containing exactly one line: collab ok"
                ),
                "acceptance": (
                    "hello.txt exists in the task worktree with the single line "
                    "collab ok, and the task commits it."
                ),
                "dependencies": [],
                "coding": True,
            },
        )
        done = poll_task(host, task["id"], args.timeout, args.interval, "coding task")
        files = read_artifacts(host, session, done["id"])
        diff = files.get("file diff", {})
        evidence["task"] = {
            "taskId": done["id"],
            "status": done["status"],
            "worktree": done.get("worktree"),
            "baseCommit": done.get("baseCommit"),
            "commit": done.get("commit"),
            "integrationStatus": done.get("integrationStatus"),
            "result": done.get("result"),
            "artifacts": {
                label: {"path": item["path"], "bytes": item["bytes"]}
                for label, item in files.items()
            },
        }
        evidence["diff"] = {
            "present": bool(diff),
            "hello": "hello.txt" in diff.get("text", ""),
            "added": "+collab ok" in diff.get("text", ""),
            "bytes": diff.get("bytes", 0),
        }
        print(
            f"task status={done['status']} commit={done.get('commit')} "
            f"diff={'yes' if diff else 'no'}"
        )
        if done["status"] != "completed":
            problems.append(f"coding task ended {done['status']}")
        if not diff:
            problems.append("no file diff attachment was published")
        elif not evidence["diff"]["hello"]:
            problems.append("the file diff does not mention hello.txt")

        if done["status"] == "completed" and done.get("commit"):
            try:
                integrated = host.call(
                    "task/integrate",
                    {
                        "taskId": done["id"],
                        "expectedCommit": done["commit"],
                        "review": "stage 6-4 real provider verification",
                    },
                )
                evidence["integrate"] = integrated
                print(f"integrate {json.dumps(integrated, ensure_ascii=False)}")
            except HostError as error:
                evidence["integrate"] = {"error": str(error)}
                print(f"integrate failed: {error}")
    finally:
        host.close()

    evidence["verdict"] = {
        "real_api_tested": not problems,
        "problems": problems,
    }
    print()
    print(json.dumps(evidence, ensure_ascii=False, indent=2))
    if args.json:
        with open(args.json, "w", encoding="utf-8") as handler:
            json.dump(evidence, handler, ensure_ascii=False, indent=2)
            handler.write("\n")
        print(f"wrote {args.json}", file=sys.stderr)
    if problems:
        print("FAIL: " + "; ".join(problems), file=sys.stderr)
        return 1
    print("PASS: real provider call, Keychain credential and a committed coding task")
    return 0


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("prepare", "verify"))
    parser.add_argument("--core", required=True)
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--state", default=DEFAULT_STATE)
    parser.add_argument("--tui", default="collab/target/debug/codex-collab-tui")
    parser.add_argument("--base-url", default="https://api.agnes-ai.cn/v1")
    parser.add_argument("--model", default="agnes-3.0-flash")
    parser.add_argument(
        "--protocol", default="chat_completions", choices=("responses", "chat_completions")
    )
    parser.add_argument("--context-bytes", type=int, default=128000)
    parser.add_argument("--timeout", type=float, default=600)
    parser.add_argument("--interval", type=float, default=2)
    parser.add_argument("--json", help="write the verification evidence here")
    args = parser.parse_args()
    return prepare(args) if args.mode == "prepare" else verify(args)


if __name__ == "__main__":
    sys.exit(main() or 0)
