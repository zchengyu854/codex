#!/usr/bin/env bash
# Regenerate the stage 5 evidence log: workspace tests, TUI opt-in tests, lints, pty probes and
# screen snapshots. Run from anywhere; paths resolve relative to this script. Failing steps abort and
# leave the partial log in place.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
manifest="$root/collab/Cargo.toml"
log="$root/collab/evidence/stage-5-tests.log"
tui="$root/collab/target/debug/codex-collab-tui"
core="$root/collab/target/debug/codex-collab-core"

# The pty probes drive the real binaries, so build them before capturing the log.
cargo build --locked --manifest-path "$manifest" -p codex-collab-core -p codex-collab-tui >/dev/null

{
    echo "# stage 5 contract coverage tests"
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
    echo "## screen snapshot (demo, 40x132)"
    python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --demo --rows 40 --cols 132
    echo "## screen snapshot (task output view, live, 40x132)"
    python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --live --core "$core" --rows 40 \
        --cols 132 --keys '^T|||||^O'
} >"$log" 2>&1
echo "wrote $log"
