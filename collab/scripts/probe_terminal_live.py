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


def with_host(calls):
    """Run a short host session, then return its results in order.

    Each call reopens the state directory; only one host may own it at a time, so this is used
    before the TUI starts and after it exits.
    """
    host = subprocess.Popen(
        [core, state, codex],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        text=True, bufsize=1,
    )
    counter = 0
    results = []

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

    try:
        for method, params in calls:
            results.append(call(method, params))
    finally:
        host.stdin.close()
        host.wait(timeout=10)
    return results


def seed():
    """Create a session and one agent through the host's line protocol."""
    session = with_host(
        [("session/create", {"repository": repo, "validation": [["/usr/bin/true"]]})]
    )[0]
    with_host(
        [
            (
                "agent/configure",
                {
                    "id": AGENT,
                    "sessionId": session["id"],
                    "role": "coordinator",
                    "model": MODEL,
                    "baseUrl": "https://example.invalid/v1",
                    "protocol": "responses",
                    "contextBytes": 65536,
                },
            )
        ]
    )
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


def text_now():
    """Strip escapes and the inline caret, which the cell-diff stream repeats per keystroke."""
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    return text.replace("❙", "")


def wait_for(needle, seconds=4.0):
    """Drain until `needle` shows up; the UI polls the host, so a fixed sleep is flaky."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        drain(0.1)
        if needle in text_now():
            return text_now()
    raise AssertionError(f"{needle!r} did not render:\n{text_now()[-2000:]!r}")


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
    os.write(master, b"\x05")  # Ctrl+E toggles it back off, so the editor starts from "关"
    drain(0.3)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "按普通任务提交" in text, (
        f"coding toggle did not turn off:\n{text[-2000:]!r}"
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

    # Ctrl+A on the seeded coordinator prefills the existing agent (edit path).
    output.clear()
    os.write(master, b"\x01")  # Ctrl+A
    drain(0.5)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "配置 Agent" in text, f"agent form missing:\n{text[-2000:]!r}"
    assert MODEL in text, f"existing model was not prefilled:\n{text[-2000:]!r}"
    assert "主控（coordinator）" in text, "coordinator role missing from the agent form"
    os.write(master, b"\x1b")  # Esc cancels
    drain(0.3)

    # Tab to a worker pane and add a second agent through the form: the client generates the
    # UUID, the host validates the endpoint and the pane refreshes with the new model.
    output.clear()
    os.write(master, b"\t")  # Tab focuses worker pane 2
    drain(0.2)
    os.write(master, b"\x01")  # Ctrl+A
    drain(0.5)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "配置 Agent" in text, f"worker agent form missing:\n{text[-2000:]!r}"
    assert "工作（worker）" in text, "worker role missing from the agent form"
    os.write(master, b"worker-model")
    os.write(master, b"\t")  # model -> baseUrl
    os.write(master, b"https://example.invalid/v1")
    os.write(master, b"\t")  # baseUrl -> protocol
    os.write(master, b"\t")  # protocol -> contextBytes (prefilled)
    os.write(master, b"\r")  # submit agent/configure
    drain(1.2)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "worker-model" in text, f"configured worker model not rendered:\n{text[-2000:]!r}"
    assert "65536" in text, f"context budget not rendered:\n{text[-2000:]!r}"

    # Ctrl+T queues a capability probe for the focused agent. Without a real credential the task
    # cannot verify; the client must still report that the probe was queued.
    output.clear()
    os.write(master, b"\x14")  # Ctrl+T
    drain(1.0)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "连接探针任务" in text or "连接自检" in text, (
        f"connection probe was not queued:\n{text[-2000:]!r}"
    )

    os.write(master, b"\x0f")  # Ctrl+O opens the task output view for the probe task
    drain(0.8)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "任务输出" in text, f"task output view missing:\n{text[-2000:]!r}"
    os.write(master, b"\x1b")  # Esc closes it
    drain(0.3)

    # Ctrl+U publishes a user constraint; it must reach event/publish and land in the shared log.
    output.clear()
    os.write(master, b"\x15")  # Ctrl+U
    drain(0.4)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "发布用户约束" in text, f"publish form missing:\n{text[-2000:]!r}"
    assert "requirement" in text and "suggestion" in text, "kind selector missing"
    os.write(master, b"\x1b[C\x1b[C")  # Right twice: requirement -> user_override -> suggestion
    drain(0.3)
    os.write(master, b"keep the public API stable")
    drain(0.2)
    os.write(master, b"\r")  # submit event/publish
    drain(1.0)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "已发布" in text, f"publication feedback missing:\n{text[-2000:]!r}"

    # The constraint must land in the shared log under the selected kind (suggestion -> "[建议]",
    # which only matches if spaces in the text did not cycle the kind). ratatui emits only changed
    # cells, so spaces are absent from the byte stream: match on non-space runs.
    output.clear()
    os.write(master, b"\x1bOR")  # F3 shows the shared records panel
    drain(0.6)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "[建议]" in text and "keep" in text and "stable" in text, (
        f"published constraint is not in the shared log:\n{text[-2000:]!r}"
    )
    os.write(master, b"\x1bOR")  # F3 hides it again
    drain(0.3)

    # Ctrl+F opens the task editor. Enter inserts a newline instead of dispatching, the coding
    # switch is visible, and Ctrl+S sends the full parameter set (goal, acceptance, coding).
    output.clear()
    os.write(master, b"\x06")  # Ctrl+F
    drain(0.7)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    assert "编排任务" in text, f"task editor missing:\n{text[-2000:]!r}"
    assert "任务目标" in text and "验收标准" in text, (
        f"task editor fields missing:\n{text[-2000:]!r}"
    )
    assert "Ctrl+S" in text, "the task editor does not advertise Ctrl+S"
    os.write(master, b"implement retry with backoff")
    os.write(master, b"\r")  # Enter is a newline in the goal, not a dispatch
    os.write(master, b"second line")
    drain(0.3)
    # ratatui writes only changed cells, and the inline caret changes one cell per keystroke, so
    # drop the caret marker before matching contiguous text.
    text = wait_for("second line")
    assert "已提交任务" not in text, "Enter must not dispatch from the editor"
    os.write(master, b"\t")  # goal -> acceptance
    os.write(master, b"tests pass and cancellation is covered")
    os.write(master, b"\t")  # acceptance -> dependencies
    drain(0.2)
    os.write(master, b"\t")  # dependencies -> coding
    os.write(master, b" ")  # space toggles the coding switch
    drain(0.3)
    text = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output)).decode("utf-8", "replace")
    text = text.replace("❙", "")
    assert "编码任务" in text, f"coding switch missing:\n{text[-2000:]!r}"
    os.write(master, b"\x13")  # Ctrl+S dispatches
    wait_for("任务已提交")


    os.write(master, b"\x11")  # Ctrl+Q
    drain(0.3)
    os.write(master, b"\r")  # Enter confirms quit
    process.wait(timeout=10)
    assert process.returncode == 0, f"exit code {process.returncode}"
    drain(0.3)
    assert termios.tcgetattr(slave) == before, "terminal input modes were not restored"
    assert b"\x1b[?1049l" in output, "alternate screen was not restored"

    # Read the persisted task back: the editor's parameters must have reached `task/create`
    # verbatim, including the newline and the coding flag.
    tasks = with_host([("task/list", {"sessionId": session_id})])[0]
    edited = [
        task for task in tasks
        if task.get("goal") == "implement retry with backoff\nsecond line"
    ]
    assert edited, f"the edited task was not persisted: {tasks!r}"
    task = edited[0]
    assert task["acceptance"] == "tests pass and cancellation is covered", task
    assert task["coding"] is True, task
    assert task["dependencies"] == [], "no dependency was selected"
    print("PASS: live session/agent rendering, coding toggle, review routing, compaction and create forms, agent configuration, connection probe, task output view, user constraint publication, task editor with newline/acceptance/coding, masked credential form, confirmed quit, terminal restoration")
finally:
    if process.poll() is None:
        process.kill()
        process.wait()
    os.close(master)
    os.close(slave)
