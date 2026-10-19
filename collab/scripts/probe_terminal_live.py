"""Smoke-test live mode: seed a real session+agent with the host, then render it in the TUI.

Usage: python3 probe_terminal_live.py <tui-binary> <core-binary> [codex-binary]
"""
import fcntl
import json
import os
import pty
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time

tui = sys.argv[1]
core = sys.argv[2]
codex = sys.argv[3] if len(sys.argv) > 3 else "codex"

work = tempfile.mkdtemp(prefix="codex-collab-live-")
repo = os.path.join(work, "repo")
state = os.path.join(work, "state")
os.makedirs(repo)
for args in (
    ["init"],
    ["-c", "user.name=t", "-c", "user.email=t@localhost", "add", "."],
    ["-c", "user.name=t", "-c", "user.email=t@localhost", "commit", "--allow-empty", "-m", "init"],
):
    subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)

AGENT = "11111111-2222-4333-8444-555555555555"
MODEL = "planner-model"


def seed():
    """Drive the host's line protocol directly to create a session and one agent."""
    host = subprocess.Popen(
        [core, state, codex],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        text=True, bufsize=1,
    )
    counter = 0

    def call(method, params):
        nonlocal counter
        counter += 1
        host.stdin.write(json.dumps({"id": counter, "method": method, "params": params}) + "\n")
        host.stdin.flush()
        while True:
            line = host.stdout.readline()
            assert line, f"host closed before answering {method}"
            value = json.loads(line)
            if value.get("id") == counter:
                assert "error" not in value, f"{method}: {value.get('error')}"
                return value["result"]

    session = call("session/create", {"repository": repo, "validation": [["/usr/bin/true"]]})
    call("agent/configure", {
        "id": AGENT, "sessionId": session["id"], "role": "coordinator", "model": MODEL,
        "baseUrl": "https://example.invalid/v1", "protocol": "responses", "contextBytes": 65536,
    })
    host.stdin.close()
    host.wait(timeout=10)
    return session["id"]


session_id = seed()
print(f"seeded session {session_id[:8]} with agent model {MODEL}")

master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 140, 0, 0))
before = termios.tcgetattr(slave)
output = bytearray()


def drain(seconds=0.4):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if select.select([master], [], [], 0.05)[0]:
            output.extend(os.read(master, 65536))


process = subprocess.Popen(
    [tui, "--state", state, "--host", core, "--session", session_id, "--codex", codex],
    stdin=slave, stdout=slave, stderr=slave,
    env={**os.environ, "TERM": "xterm-256color"},
)
try:
    drain(1.5)
    # ratatui jumps the cursor over the second cell of wide (CJK) glyphs, so strip escape
    # sequences before matching multibyte text.
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    if os.environ.get("PROBE_DEBUG"):
        print(text[-2000:])
    assert MODEL in text, f"real model name not rendered:\n{text[-2000:]!r}"
    assert "主控" in text, "coordinator pane missing"
    assert session_id[:8] in text, "session label missing"
    assert "Responses" in text, "protocol label missing"

    # Credential entry must be masked and must never echo the typed key.
    os.write(master, b"\x0b")  # Ctrl+K opens the credential form on the focused agent
    drain(0.4)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "设置 API Key" in text, f"credential form missing:\n{text[-2000:]!r}"
    os.write(master, b"topsecret-value")
    drain(0.3)
    assert b"topsecret-value" not in output, "credential was echoed to the terminal"
    os.write(master, b"\x1b")  # Esc cancels without saving
    drain(0.3)

    # The coding toggle updates the footer and the feedback line.
    os.write(master, b"\x05")  # Ctrl+E
    drain(0.3)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "后续输入将作为编码任务提交" in text, (
        f"coding toggle not reflected:\n{text[-2000:]!r}"
    )

    # Ctrl+G routes to the review flow; the seeded session has no task yet.
    os.write(master, b"\x07")  # Ctrl+G
    drain(0.4)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "可审查的任务" in text or "正在读取任务与差异" in text, (
        f"review routing failed:\n{text[-2000:]!r}"
    )

    # Ctrl+P probes the session revision and opens the compaction form.
    os.write(master, b"\x10")  # Ctrl+P
    drain(0.6)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "压缩上下文" in text, f"compaction form missing:\n{text[-2000:]!r}"
    assert "会话版本" in text, "compaction form is missing the probed revision"
    os.write(master, b"\x1b")  # Esc cancels
    drain(0.3)

    # Ctrl+N opens the interactive session creation form.
    os.write(master, b"\x0e")  # Ctrl+N
    drain(0.4)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "新建协作会话" in text, f"create form missing:\n{text[-2000:]!r}"
    os.write(master, b"\x1b")  # Esc cancels
    drain(0.3)

    os.write(master, b"\x11")  # Ctrl+Q
    drain(0.3)
    os.write(master, b"\r")  # Enter confirms quit
    process.wait(timeout=10)
    assert process.returncode == 0, f"exit code {process.returncode}"
    drain(0.3)
    assert termios.tcgetattr(slave) == before, "terminal input modes were not restored"
    assert b"\x1b[?1049l" in output, "alternate screen was not restored"
    print("PASS: live session/agent rendering, coding toggle, review routing, compaction and create forms, masked credential form, confirmed quit, terminal restoration")
finally:
    if process.poll() is None:
        process.kill()
        process.wait()
    os.close(master)
    os.close(slave)
