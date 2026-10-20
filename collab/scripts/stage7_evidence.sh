#!/usr/bin/env bash
# Regenerate the stage 7-2 evidence log: workspace tests, TUI opt-in tests, lints, both pty probes
# and the demo frame. The real provider runs are separate (`stage7_real_evidence.sh`) so this script
# never spends money and never needs a credential. Run from anywhere; paths resolve relative to the
# script, and the first failing step aborts with the partial log left behind.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
manifest="$root/collab/Cargo.toml"
log="$root/collab/evidence/stage-7-tests.log"
tui="$root/collab/target/debug/codex-collab-tui"
core="$root/collab/target/debug/codex-collab-core"

# The pty probes drive the real binaries, so build them before capturing the log.
cargo build --locked --manifest-path "$manifest" -p codex-collab-core -p codex-collab-tui >/dev/null

{
    echo "# stage 7 real multi-agent collaboration: test and probe log"
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
    echo "## screen snapshot (demo, 34x132)"
    # Demo mode over the real binaries: no host, no credential, so this frame is always reproducible.
    python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --demo --rows 34 --cols 132 \
        | tee "$root/collab/evidence/stage-7-demo-screen.txt"
} >"$log" 2>&1
echo "wrote $log"
