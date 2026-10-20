#!/usr/bin/env python3
"""Stage 6-4 failure matrix, entirely on loopback: no real provider, no real credential.

Covers the branches the design asks to record honestly:

  * the address rule (HTTPS, or HTTP on loopback; no credentials/query/fragment)
  * a Keychain set/delete round-trip on a temporary agent id with a dummy secret
  * upstream 401 (no retry), 429 (bounded retries), transport failure, and a missing credential

The dummy secret is written through `agent/setCredential` under AGENT (a throwaway id), deleted in
the same run, and never printed. The operator's real Keychain item is not touched.
"""
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

AGENT = "33333333-4444-4555-8666-777777777777"
TERMINAL = ("completed", "failed", "cancelled", "interrupted", "blocked")
REQUESTS = {401: 1, 429: 3}


class HostError(Exception):
    pass


class Host:
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


class FakeUpstream:
    """Answer every POST with one status, counting how many the gateway sent."""

    def __init__(self, status):
        self.status = status
        self.requests = 0
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                length = int(self.headers.get("content-length", 0))
                self.rfile.read(length)
                outer.requests += 1
                body = b'{"error":{"message":"stage6 fake upstream"}}'
                self.send_response(outer.status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def base_url(self):
        return f"http://127.0.0.1:{self.server.server_address[1]}/v1"

    def close(self):
        self.server.shutdown()
        self.server.server_close()


def git(repo, *args):
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, capture_output=True, text=True
    ).stdout.strip()


def configure(host, session, base_url, protocol="chat_completions"):
    return host.call(
        "agent/configure",
        {
            "id": AGENT,
            "sessionId": session,
            "role": "coordinator",
            "model": "stage6-probe-model",
            "baseUrl": base_url,
            "protocol": protocol,
            "contextBytes": 65536,
        },
    )


def run_task(host, session, goal, timeout=120):
    task = host.call(
        "task/create",
        {
            "sessionId": session,
            "agentId": AGENT,
            "goal": goal,
            "acceptance": "not applicable; this task is expected to fail at the upstream",
            "dependencies": [],
            "coding": False,
        },
    )
    deadline = time.monotonic() + timeout
    while True:
        current = host.call("task/get", {"taskId": task["id"]})
        if current["status"] in TERMINAL:
            return current["status"], current.get("result") or ""
        if time.monotonic() >= deadline:
            host.call("task/cancel", {"taskId": task["id"]})
            return "timeout", "task did not reach a terminal state"
        time.sleep(1)


def main():
    core = sys.argv[1]
    state = tempfile.mkdtemp(prefix="codex-collab-errors-")
    repo = os.path.join(state, "repo")
    os.makedirs(repo)
    git(repo, "init")
    identity = ("-c", "user.name=stage6", "-c", "user.email=stage6@localhost")
    git(repo, *identity, "add", ".")
    git(repo, *identity, "commit", "--allow-empty", "-m", "init")

    evidence = {"agent": AGENT, "checks": {}}
    problems = []
    host = Host(core, state)
    try:
        session = host.call(
            "session/create", {"repository": repo, "validation": [["/usr/bin/true"]]}
        )["id"]

        # Address rule: non-loopback HTTP is refused, loopback HTTP and HTTPS are accepted.
        rejected = {}
        for label, url in (
            ("http_public", "http://example.com/v1"),
            ("query", "https://example.invalid/v1?x=1"),
            ("credentials", "https://user:pass@example.invalid/v1"),
        ):
            try:
                configure(host, session, url)
                rejected[label] = {"rejected": False, "accepted": url}
                problems.append(f"{label} was accepted by agent/configure")
            except HostError as error:
                rejected[label] = {"rejected": True, "error": str(error)}
        configure(host, session, "http://127.0.0.1:1/v1")
        configure(host, session, "https://example.invalid/v1")
        evidence["checks"]["address_rule"] = {
            "rejected": rejected,
            "accepted": ["http loopback", "https"],
        }

        # A dummy credential under the throwaway agent id, so the gateway can be reached.
        saved = host.call("agent/setCredential", {"agentId": AGENT, "key": "stage6-dummy-key"})
        evidence["checks"]["keychain_set"] = saved

        for status in (401, 429):
            upstream = FakeUpstream(status)
            try:
                configure(host, session, upstream.base_url)
                task_status, result = run_task(
                    host, session, f"Upstream {status} failure-classification check."
                )
                evidence["checks"][f"upstream_{status}"] = {
                    "taskStatus": task_status,
                    "upstreamRequests": upstream.requests,
                    "result": result,
                }
                if task_status != "failed":
                    problems.append(f"{status}: task ended {task_status}")
                expected = REQUESTS[status]
                if upstream.requests != expected:
                    problems.append(
                        f"{status}: upstream saw {upstream.requests} requests, expected {expected}"
                    )
            finally:
                upstream.close()

        # A closed loopback port: the transport fails and is never replayed.
        configure(host, session, "http://127.0.0.1:1/v1")
        task_status, result = run_task(host, session, "Transport failure check.")
        evidence["checks"]["transport"] = {"taskStatus": task_status, "result": result}
        if task_status != "failed":
            problems.append(f"transport: task ended {task_status}")

        deleted = host.call("agent/deleteCredential", {"agentId": AGENT})
        evidence["checks"]["keychain_delete"] = deleted
        task_status, result = run_task(host, session, "Missing credential check.")
        evidence["checks"]["no_credential"] = {"taskStatus": task_status, "result": result}
        if "Keychain credential unavailable" not in result:
            problems.append("missing credential did not surface as Keychain credential unavailable")
    finally:
        host.close()

    evidence["verdict"] = {"ok": not problems, "problems": problems}
    print(json.dumps(evidence, ensure_ascii=False, indent=2))
    output = os.path.join(os.path.dirname(__file__), "..", "evidence", "stage-6-errors.json")
    with open(output, "w", encoding="utf-8") as handler:
        json.dump(evidence, handler, ensure_ascii=False, indent=2)
        handler.write("\n")
    print(f"wrote {os.path.normpath(output)}", file=sys.stderr)
    if problems:
        print("FAIL: " + "; ".join(problems), file=sys.stderr)
        return 1
    print("PASS: address rule, Keychain round-trip, 401, 429, transport and missing credential")
    return 0


if __name__ == "__main__":
    sys.exit(main())
