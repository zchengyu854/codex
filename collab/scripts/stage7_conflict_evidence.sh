#!/usr/bin/env bash
# Stage 7-3 replay: concurrency, a clean same-file merge, a real conflict and the drop that
# unblocks it. Spends money only without `--reuse`, and the key never passes through the script.
#
#   STATE=/path/to/state SESSION=<id> stage7_conflict_evidence.sh --approve
#   STATE=/path/to/state SESSION=<id> stage7_conflict_evidence.sh --reuse   # rebuild log and snapshots
#
# A state directory that already ran this scenario needs a fresh file name:
#
#   FILE=shared3.txt stage7_conflict_evidence.sh --approve
#
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
manifest="$root/collab/Cargo.toml"
core="$root/collab/target/debug/codex-collab-core"
tui="$root/collab/target/debug/codex-collab-tui"
evidence="$root/collab/evidence"
state="${STATE:?set STATE to the stage 7 state directory}"
session="${SESSION:?set SESSION to the session id inside STATE}"
json="$evidence/stage-7-conflict.json"
file="${FILE:-shared.txt}"
reuse=0
approve=0
for arg in "$@"; do
    case "$arg" in
        --reuse) reuse=1 ;;
        --approve) approve=1 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

if ((reuse == 0)); then
    cargo build --locked --manifest-path "$manifest" -p codex-collab-core -p codex-collab-tui >/dev/null
    # The TUI must be closed: the host owns the state directory while the run is in flight.
    args=(--core "$core" --state "$state" --json "$json" --session "$session" --file "$file")
    ((approve == 1)) && args+=(--approve)
    python3 "$root/collab/scripts/stage7_conflict.py" "${args[@]}"
fi

python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --live --core "$core" \
    --state "$state" --session "$session" --rows 34 --cols 132 \
    >"$evidence/stage-7-conflict-screen.txt"
python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --live --core "$core" \
    --state "$state" --session "$session" --rows 34 --cols 132 --keys '\eOR' \
    >"$evidence/stage-7-conflict-shared-screen.txt"
# The contested task is the latest task on worker 2 (pane 3).
python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --live --core "$core" \
    --state "$state" --session "$session" --rows 34 --cols 132 --keys '\e3|^D' \
    >"$evidence/stage-7-conflict-detail-screen.txt"
python3 "$root/collab/scripts/redact_endpoint.py" \
    "$evidence/stage-7-conflict-screen.txt" \
    "$evidence/stage-7-conflict-shared-screen.txt" \
    "$evidence/stage-7-conflict-detail-screen.txt"

{
    echo "# stage 7 concurrency and integration conflict"
    date
    echo "# 端点按阶段 6/7 的安全边界脱敏；密钥只经 TUI 的 Ctrl+K 进入 Keychain"
    echo
    python3 "$root/collab/scripts/stage7_report.py" conflict "$json"
    echo
    echo "## 界面快照（34x132，真实宿主，放弃冲突之后）"
    echo
    cat "$evidence/stage-7-conflict-screen.txt"
    echo
    cat "$evidence/stage-7-conflict-shared-screen.txt"
    echo
    cat "$evidence/stage-7-conflict-detail-screen.txt"
} >"$evidence/stage-7-conflict.log"
echo "wrote $evidence/stage-7-conflict.log"
