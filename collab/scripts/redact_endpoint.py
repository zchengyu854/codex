#!/usr/bin/env python3
"""Rewrite a real endpoint inside captured frames to `https://<provider>/v1`, keeping the width.

The frames are fixed-width evidence: swapping the host for a shorter placeholder would shift every
box border to its left. Padding the placeholder to the exact original length keeps every other cell
where it was, so the redacted frame still lines up with what the operator saw.

    python3 redact_endpoint.py FILE [FILE...]
"""
import re
import sys

# Everything up to whitespace or a frame border: that is what a terminal cell run looks like.
URL = re.compile(r"https?://[^\s│|]+")
PLACEHOLDER = "https://<provider>/v1"


def swap(match):
    found = match.group(0)
    if len(found) < len(PLACEHOLDER):
        raise SystemExit(f"cannot redact {found!r} without shifting the frame")
    return PLACEHOLDER + " " * (len(found) - len(PLACEHOLDER))


def main():
    total = 0
    for path in sys.argv[1:]:
        with open(path, encoding="utf-8") as handler:
            text = handler.read()
        text, count = URL.subn(swap, text)
        with open(path, "w", encoding="utf-8") as handler:
            handler.write(text)
        total += count
        print(f"{path}: {count} endpoint(s) redacted")
    if total == 0:
        print("no endpoint found; the frames carry no provider address")
    return 0


if __name__ == "__main__":
    sys.exit(main())
