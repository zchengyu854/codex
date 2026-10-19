"""Render a text snapshot of the terminal UI by replaying its output from a pseudo-terminal.

The UI draws with cursor-addressed diffs, so the raw byte stream is unreadable. This script boots
the real binary inside a pty, replays the escape sequences onto an in-memory screen grid, and
prints the reconstructed frame. Optionally it sends a scripted key sequence first, so a specific
modal or flow can be captured for review and evidence.

Usage:
  python3 snapshot_terminal.py <tui-binary> --demo
  python3 snapshot_terminal.py <tui-binary> --live --core <core-binary> [--codex codex]
                                              [--keys '^U|\e[C|keep the public API stable']

`--keys` is a `|`-separated sequence of chunks. `^X` sends a control character, `\e` sends Escape
and everything else is typed as UTF-8 text, so `^U` is Ctrl+U and `\e[C` is Right.
"""
import argparse
import fcntl
import json
import os
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata

CONTINUATION = ""  # marks the second cell of a wide glyph


def _utf8_prefix(data):
    """Length of the longest prefix of `data` that does not end mid-character."""
    for back in range(1, 4):
        if len(data) < back:
            break
        byte = data[-back]
        if byte >= 0xC0:
            need = 1 if byte < 0xE0 else (2 if byte < 0xF0 else 3)
            return len(data) - back if back - 1 < need else len(data)
    return len(data)


class Screen:
    def __init__(self, rows, cols):
        self.rows = rows
        self.cols = cols
        self.cells = [[" "] * cols for _ in range(rows)]
        # True for the first cell of a double-width glyph; its neighbour holds CONTINUATION.
        self.wide = [[False] * cols for _ in range(rows)]
        self.row = 0
        self.col = 0
        self.buf = bytearray()
        self.pending = bytearray()

    # ---- input ----------------------------------------------------------------
    def feed(self, data):
        self.buf.extend(data)
        index, size = 0, len(self.buf)
        while index < size:
            byte = self.buf[index]
            if byte == 0x1B:
                end = self._scan_escape(index)
                if end is None:
                    break
                self._flush()
                self._escape(bytes(self.buf[index + 1:end]))
                index = end
            elif byte < 0x20 or byte == 0x7F:
                self._flush()
                self._control(byte)
                index += 1
            else:
                self.pending.append(byte)
                index += 1
        self._flush()
        self.buf = self.buf[index:]

    def _flush(self):
        if not self.pending:
            return
        data = bytes(self.pending)
        cut = _utf8_prefix(data)
        if cut:
            self._put(data[:cut].decode("utf-8", "replace"))
        self.pending = bytearray(data[cut:])

    def _scan_escape(self, index):
        if index + 1 >= len(self.buf):
            return None
        tag = self.buf[index + 1]
        if tag == 0x5B:  # CSI: params then a final byte in 0x40..0x7e
            cursor = index + 2
            while cursor < len(self.buf):
                if 0x40 <= self.buf[cursor] <= 0x7E:
                    return cursor + 1
                cursor += 1
            return None
        if tag in (0x5D, 0x50, 0x58, 0x5E, 0x5F):  # string sequences end with BEL or ST
            cursor = index + 2
            while cursor < len(self.buf):
                if self.buf[cursor] == 0x07:
                    return cursor + 1
                if self.buf[cursor] == 0x1B and cursor + 1 < len(self.buf) and self.buf[cursor + 1] == 0x5C:
                    return cursor + 2
                cursor += 1
            return None
        if tag in (0x28, 0x29, 0x2A, 0x2B, 0x23, 0x20):  # charset and friends take one more byte
            return index + 3 if index + 2 < len(self.buf) else None
        return index + 2

    def _escape(self, seq):
        if not seq or seq[0:1] != b"[":
            return
        body = seq[1:].decode("latin-1")
        final, params = body[-1], body[:-1]
        if params.startswith("?"):
            return
        args = [int(part) for part in params.split(";") if part.isdigit()]

        def arg(position, default):
            return args[position] if len(args) > position else default

        if final in "Hf":
            self.row, self.col = arg(0, 1) - 1, arg(1, 1) - 1
        elif final == "A":
            self.row -= max(1, arg(0, 1))
        elif final == "B":
            self.row += max(1, arg(0, 1))
        elif final == "C":
            self.col += max(1, arg(0, 1))
        elif final == "D":
            self.col -= max(1, arg(0, 1))
        elif final == "E":
            self.row += max(1, arg(0, 1))
            self.col = 0
        elif final == "F":
            self.row -= max(1, arg(0, 1))
            self.col = 0
        elif final == "G":
            self.col = arg(0, 1) - 1
        elif final == "d":
            self.row = arg(0, 1) - 1
        elif final == "J":
            self._erase_display(arg(0, 0))
        elif final == "K":
            self._erase_line(arg(0, 0))
        elif final == "X":
            for offset in range(max(1, arg(0, 1))):
                self._set(self.col + offset, " ")
        elif final == "P":
            self._delete(max(1, arg(0, 1)))
        elif final == "@":
            self._insert(max(1, arg(0, 1)))
        self._clamp()

    def _control(self, byte):
        if byte == 0x0D:
            self.col = 0
        elif byte == 0x0A:
            self.row += 1
            if self.row >= self.rows:
                self._scroll()
        elif byte == 0x08:
            self.col = max(0, self.col - 1)
        elif byte == 0x09:
            self.col = min(self.cols - 1, (self.col // 8 + 1) * 8)
        self._clamp()

    # ---- screen ---------------------------------------------------------------
    def _put(self, text):
        for char in text:
            width = 2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
            if self.col >= self.cols:
                self.col = 0
                self.row += 1
                if self.row >= self.rows:
                    self._scroll()
            self._set(self.col, char)
            if width == 2:
                self.wide[self.row][self.col] = True
                if self.col + 1 < self.cols:
                    self._set(self.col + 1, CONTINUATION)
            self.col += width
        self._clamp()

    def _set(self, col, value):
        """Write one cell, keeping double-width glyphs consistent with their neighbour."""
        if not (0 <= col < self.cols):
            return
        row = self.row
        if value == CONTINUATION:
            # Only ever written together with the leading cell, which just claimed the glyph. A
            # stale leading flag here belongs to a glyph from an earlier frame whose trailing cell
            # this write just took over, so drop both halves of it.
            if self.wide[row][col]:
                self.wide[row][col] = False
                if col + 1 < self.cols and self.cells[row][col + 1] == CONTINUATION:
                    self.cells[row][col + 1] = " "
            self.cells[row][col] = CONTINUATION
            return
        if self.wide[row][col]:
            # Overwriting the leading cell orphans its continuation.
            self.wide[row][col] = False
            if col + 1 < self.cols:
                self.cells[row][col + 1] = " "
        if self.cells[row][col] == CONTINUATION:
            # Overwriting a trailing cell erases the glyph it belonged to.
            if col > 0 and self.wide[row][col - 1]:
                self.wide[row][col - 1] = False
                self.cells[row][col - 1] = " "
        self.cells[row][col] = value

    def _clear(self, row, col):
        self.cells[row][col] = " "
        self.wide[row][col] = False

    def _scroll(self):
        self.cells.pop(0)
        self.cells.append([" "] * self.cols)
        self.wide.pop(0)
        self.wide.append([False] * self.cols)
        self.row = self.rows - 1

    def _erase_line(self, mode):
        if mode == 1:
            span = range(0, min(self.col + 1, self.cols))
        elif mode == 2:
            span = range(self.cols)
        else:
            span = range(self.col, self.cols)
        for col in span:
            self._set(col, " ")

    def _erase_display(self, mode):
        if mode == 1:
            rows = range(0, self.row + 1)
        elif mode == 2:
            rows = range(self.rows)
        else:
            rows = range(self.row, self.rows)
        for row in rows:
            for col in range(self.cols):
                if row != self.row or mode != 0 or col >= self.col:
                    self._clear(row, col)

    def _delete(self, count):
        row, wide = self.cells[self.row], self.wide[self.row]
        del row[self.col:self.col + count]
        del wide[self.col:self.col + count]
        row.extend([" "] * (self.cols - len(row)))
        wide.extend([False] * (self.cols - len(wide)))

    def _insert(self, count):
        row, wide = self.cells[self.row], self.wide[self.row]
        for _ in range(count):
            row.insert(self.col, " ")
            wide.insert(self.col, False)
        del row[self.cols:]
        del wide[self.cols:]

    def _clamp(self):
        self.row = min(max(self.row, 0), self.rows - 1)
        self.col = min(max(self.col, 0), self.cols - 1)

    def render(self):
        lines = []
        for row in self.cells:
            lines.append("".join(cell if cell != CONTINUATION else "" for cell in row).rstrip())
        while lines and not lines[-1]:
            lines.pop()
        return "\n".join(lines)


def seed_session(core, codex, state):
    """Create a real session with one configured agent through the host's line protocol."""
    work = tempfile.mkdtemp(prefix="codex-collab-snapshot-")
    repo = os.path.join(work, "repo")
    os.makedirs(repo)
    for args in (
        ["init"],
        ["-c", "user.name=t", "-c", "user.email=t@localhost", "add", "."],
        ["-c", "user.name=t", "-c", "user.email=t@localhost", "commit", "--allow-empty", "-m", "init"],
    ):
        subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)
    host = subprocess.Popen(
        [core, state, codex], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
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
        "id": "11111111-2222-4333-8444-555555555555", "sessionId": session["id"],
        "role": "coordinator", "model": "planner-model",
        "baseUrl": "https://example.invalid/v1", "protocol": "responses", "contextBytes": 65536,
    })
    host.stdin.close()
    host.wait(timeout=10)
    return session["id"]


def parse_keys(spec):
    """Split the key script into byte chunks: `^X` control chars, `\\e` escape, rest is text."""
    escapes = {"e": 0x1B, "n": 0x0A, "r": 0x0D, "t": 0x09}
    for chunk in ([part for part in spec.split("|")] if spec else []):
        out = bytearray()
        index = 0
        while index < len(chunk):
            char = chunk[index]
            if char == "^" and index + 1 < len(chunk) and chunk[index + 1].isalpha():
                out.append(ord(chunk[index + 1].upper()) - 64)
                index += 2
            elif char == "\\" and index + 1 < len(chunk) and chunk[index + 1] in escapes:
                out.append(escapes[chunk[index + 1]])
                index += 2
            else:
                out.extend(char.encode("utf-8"))
                index += 1
        yield bytes(out)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("tui")
    parser.add_argument("--demo", action="store_true", help="launch the built-in demo session")
    parser.add_argument("--live", action="store_true", help="seed a real session and agent first")
    parser.add_argument("--core", help="codex-collab-core binary (required with --live)")
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--rows", type=int, default=45)
    parser.add_argument("--cols", type=int, default=140)
    parser.add_argument("--wait", type=float, default=1.6, help="seconds to let the first frame settle")
    parser.add_argument("--keys", default="", help="'|'-separated key script applied after the first frame")
    parser.add_argument("--keys-delay", type=float, default=0.45)
    args = parser.parse_args()
    if not args.demo and not args.live:
        parser.error("choose --demo or --live")

    work = tempfile.mkdtemp(prefix="codex-collab-snapshot-")
    state = os.path.join(work, "state")
    command = [args.tui]
    if args.live:
        if not args.core:
            parser.error("--live needs --core")
        session = seed_session(args.core, args.codex, state)
        command += ["--state", state, "--host", args.core, "--session", session, "--codex", args.codex]
    else:
        command += ["--demo"]

    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", args.rows, args.cols, 0, 0))
    process = subprocess.Popen(
        command, stdin=slave, stdout=slave, stderr=slave,
        env={**os.environ, "TERM": "xterm-256color"},
    )
    screen = Screen(args.rows, args.cols)

    def drain(seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.05)[0]:
                try:
                    screen.feed(os.read(master, 65536))
                except OSError:
                    return

    try:
        drain(args.wait)
        for chunk in parse_keys(args.keys):
            os.write(master, chunk)
            drain(args.keys_delay)
        if args.keys:
            drain(args.wait)
        sys.stdout.write(screen.render() + "\n")
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
        os.close(master)
        os.close(slave)


if __name__ == "__main__":
    main()
