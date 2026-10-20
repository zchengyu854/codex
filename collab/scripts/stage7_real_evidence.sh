#!/usr/bin/env bash
# Stage 7 replay against a real provider: one delegation run, four live frames, the derived log.
#
# Without --reuse this script spends money, as does stage7_conflict_evidence.sh. The operator
# writes credentials per agent with Ctrl+K; only the host reads them from Keychain.
#
#   STATE=/path/to/state SESSION=<id> STAGE7_APPROVE=1 stage7_real_evidence.sh
#   STATE=/path/to/state SESSION=<id> stage7_real_evidence.sh --reuse   # no provider calls
#
# `--reuse` only rebuilds the log from the JSON and frames that are already there. A state directory
# that already ran a pair of files needs fresh names, otherwise the integration branch holds them
# and the workers produce empty commits:
#
#   FIRST=e.txt FIRST_WORD=epsilon SECOND=f.txt SECOND_WORD=zeta stage7_real_evidence.sh
#
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
manifest="$root/collab/Cargo.toml"
core="$root/collab/target/debug/codex-collab-core"
tui="$root/collab/target/debug/codex-collab-tui"
evidence="$root/collab/evidence"
state="${STATE:?set STATE to the stage 7 state directory}"
session="${SESSION:?set SESSION to the session id inside STATE}"
json="${JSON:-$evidence/stage-7-guard.json}"
first="${FIRST:-a.txt}"
first_word="${FIRST_WORD:-alpha}"
second="${SECOND:-b.txt}"
second_word="${SECOND_WORD:-beta}"
reuse=0
[[ "${1:-}" == "--reuse" ]] && reuse=1
approve=()
[[ "${STAGE7_APPROVE:-0}" == "1" ]] && approve=(--approve)

if ((reuse == 0)); then
    cargo build --locked --manifest-path "$manifest" -p codex-collab-core -p codex-collab-tui >/dev/null
    # TUI must be closed: the host owns the state directory while the run is in flight.
    python3 "$root/collab/scripts/stage7_collab.py" verify \
        --core "$core" --state "$state" --json "$json" \
        --first-file "$first" --first-word "$first_word" \
        --second-file "$second" --second-word "$second_word" \
        "${approve[@]}"
    python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --live --core "$core" \
        --state "$state" --session "$session" --rows 34 --cols 132 \
        >"$evidence/stage-7-guard-screen.txt"
    python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --live --core "$core" \
        --state "$state" --session "$session" --rows 34 --cols 132 --keys '\eOR' \
        >"$evidence/stage-7-guard-shared-screen.txt"
    # Pane 1 detail: how often the host had to nudge the coordinator.
    python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --live --core "$core" \
        --state "$state" --session "$session" --rows 34 --cols 132 --keys '^D' \
        >"$evidence/stage-7-guard-detail-screen.txt"
    # Pane 3 detail: the dependent task, its base commit and its integration state.
    python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --live --core "$core" \
        --state "$state" --session "$session" --rows 34 --cols 132 --keys '\t|\t|^D' \
        >"$evidence/stage-7-guard-task-screen.txt"
    python3 "$root/collab/scripts/redact_endpoint.py" \
        "$evidence/stage-7-guard-screen.txt" \
        "$evidence/stage-7-guard-shared-screen.txt" \
        "$evidence/stage-7-guard-detail-screen.txt" \
        "$evidence/stage-7-guard-task-screen.txt"
fi

{
    echo "# stage 7 real multi-agent collaboration"
    date
    echo "# 端点按阶段 6/7 的安全边界脱敏；密钥只经 TUI 的 Ctrl+K 进入 Keychain，脚本与日志都不接触它"
    echo
    python3 "$root/collab/scripts/stage7_report.py" retry "$json"
    echo
    echo "## 界面快照（34x132，真实宿主，最终状态）"
    echo
    cat "$evidence/stage-7-guard-screen.txt"
    echo
    cat "$evidence/stage-7-guard-shared-screen.txt"
    echo
    cat "$evidence/stage-7-guard-detail-screen.txt"
    echo
    cat "$evidence/stage-7-guard-task-screen.txt"
} >"$evidence/stage-7-guard.log"
echo "wrote $evidence/stage-7-guard.log"
