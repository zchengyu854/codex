"""Smoke-test the demo in a real pseudo-terminal; accepts its binary path."""
import fcntl
import os
import pty
import select
import struct
import subprocess
import sys
import termios
import time

binary = sys.argv[1]
help_result = subprocess.run([binary, '--help'], capture_output=True)
assert help_result.returncode == 0 and b'--demo' in help_result.stdout
pipe_result = subprocess.run([binary, '--demo'], capture_output=True)
assert pipe_result.returncode != 0 and b'\x1b[' not in pipe_result.stdout
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 45, 140, 0, 0))
before = termios.tcgetattr(slave)
output = bytearray()


def drain(seconds=0.3):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if select.select([master], [], [], 0.05)[0]:
            output.extend(os.read(master, 65536))



process = subprocess.Popen(
    [binary, '--demo'], stdin=slave, stdout=slave, stderr=slave,
    env={**os.environ, 'TERM': 'xterm-256color'},
)
try:
    drain(0.6)
    for name in (b'planner-model', b'code-model', b'review-model', b'fast-model'):
        assert name in output, name
    os.write(master, b'\t\x1b[200~smoke-message\x1b[201~')
    drain()
    assert b'smoke-message' in output, 'the bracketed paste did not reach the input line'
    # Two Left presses put the caret before the trailing "ge"; the typed X must land there, and Enter
    # then echoes the edited line, so the demo proves caret editing and not just append-only input.
    os.write(master, b'\x1b[D\x1b[DX')
    drain()
    os.write(master, b'\r')
    drain()
    assert b'smoke-messaXge' in output, 'caret editing did not change the dispatched line'
    os.write(master, b'\x03')
    drain()
    os.write(master, b'\x11')
    drain()
    os.write(master, b'\r')
    process.wait(timeout=5)
    drain()
    assert process.returncode == 0
    assert termios.tcgetattr(slave) == before, 'terminal input modes were not restored'
    assert b'\x1b[?1049l' in output, 'alternate screen was not restored'
    assert b'\x1b[?2004l' in output, 'bracketed paste was not disabled'
    print('PASS: four panes, paste/send, caret editing, stop, confirmed quit, terminal restoration, non-TTY rejection')
finally:
    if process.poll() is None:
        process.kill()
        process.wait()
    os.close(master)
    os.close(slave)
