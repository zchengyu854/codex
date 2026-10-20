#!/usr/bin/env bash
# Regenerate the stage 6 evidence log: workspace tests, TUI opt-in tests, lints, both pty probes and
# the task-editor screen snapshot. Run from anywhere; paths resolve relative to this script. Failing
# steps abort and leave the partial log in place.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
manifest="$root/collab/Cargo.toml"
log="$root/collab/evidence/stage-6-tests.log"
tui="$root/collab/target/debug/codex-collab-tui"
core="$root/collab/target/debug/codex-collab-core"

# The pty probes drive the real binaries, so build them before capturing the log.
cargo build --locked --manifest-path "$manifest" -p codex-collab-core -p codex-collab-tui >/dev/null

{
    echo "# stage 6 parameter coverage tests"
    date
    echo "## workspace tests"
    cargo test --locked --manifest-path "$manifest" --workspace
    echo "## tui opt-in tests"
    cargo test --locked --manifest-path "$manifest" -p codex-collab-tui -- --include-ignored
    echo "## clippy"
    cargo clippy --locked --manifest-path "$manifest" -p codex-collab-core -p codex-collab-tui \
        --all-targets -- -D warnings
    echo "## format"
    if cargo fmt --manifest-path "$manifest" -p codex-collab-core -p codex-collab-tui -- --check; then
        echo "format ok"
    else
        echo "format FAILED"
        exit 1
    fi
    echo "## pty demo probe"
    python3 "$root/collab/scripts/probe_terminal.py" "$tui"
    echo "## pty live probe"
    python3 "$root/collab/scripts/probe_terminal_live.py" "$tui" "$core"
    echo "## screen snapshot (task editor, live, 40x132)"
    # Ctrl+T queues a probe task first, so the dependency list has a real row; the editor then gets a
    # two-line goal and an acceptance line. Focus stays on the acceptance field so the inlined caret
    # is part of the evidence.
    python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --live --core "$core" --rows 40 \
        --cols 132 --keys '^T|^F|implement retry with backoff|\r|second line|\t|tests pass and cancellation is covered' \
        | tee "$root/collab/evidence/stage-6-task-screen.txt"
    echo "## screen snapshot (task detail, live, 34x132)"
    # A real task/get round-trip: Ctrl+T queues the capability probe, Ctrl+D opens its detail.
    python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --live --core "$core" --rows 34 \
        --cols 132 --keys-delay 0.9 --keys '^T|^D' \
        | tee "$root/collab/evidence/stage-6-detail-screen.txt"
    echo "## screen snapshot (session picker, live, 30x132, three sessions)"
    # Three real sessions on one repository, each with a published user constraint, so the picker
    # shows multiple rows, the current marker and the per-session requirement summary.
    python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --live --core "$core" --rows 30 \
        --cols 132 --sessions 3 --keys '^L' \
        | tee "$root/collab/evidence/stage-6-sessions-screen.txt"
} >"$log" 2>&1
echo "wrote $log"
