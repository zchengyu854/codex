#!/usr/bin/env bash
# Stage 6-4 replay: real provider call + the two live screen snapshots + the derived log.
#
# This is the only stage 6 script that spends money: `verify` runs `agent/testConnection` and one
# small coding task against the configured provider. It is therefore not part of
# `stage6_evidence.sh`; run it by hand when the operator has a credential in place.
#
#   STATE=/path/to/state stage6_real_evidence.sh              # prepare check, verify, snapshots, log
#   STATE=/path/to/state stage6_real_evidence.sh --reuse       # no provider calls, rebuild the log
#   STATE=/path/to/state STAGE6_APPROVE=1 stage6_real_evidence.sh
#
# The key never passes through this script or its environment: the operator writes it in the TUI
# with Ctrl+K, the host reads it straight from the Keychain. `--reuse` needs the JSON and the
# attachment files to still exist in STATE.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
manifest="$root/collab/Cargo.toml"
core="$root/collab/target/debug/codex-collab-core"
tui="$root/collab/target/debug/codex-collab-tui"
evidence="$root/collab/evidence"
state="${STATE:?set STATE to the stage 6 state directory}"
session="${SESSION:?set SESSION to the session id inside STATE}"
reuse=0
[[ "${1:-}" == "--reuse" ]] && reuse=1
approve=()
[[ "${STAGE6_APPROVE:-0}" == "1" ]] && approve=(--approve)

if ((reuse == 0)); then
    cargo build --locked --manifest-path "$manifest" -p codex-collab-core -p codex-collab-tui >/dev/null
    python3 "$root/collab/scripts/stage6_real.py" verify \
        --core "$core" --state "$state" --interval 2 --json "$evidence/stage-6-real.json" \
        "${approve[@]}"
    python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --live --core "$core" \
        --state "$state" --session "$session" --rows 34 --cols 132 --keys '^D' \
        >"$evidence/stage-6-real-detail-screen.txt"
    python3 "$root/collab/scripts/snapshot_terminal.py" "$tui" --live --core "$core" \
        --state "$state" --session "$session" --rows 34 --cols 132 --keys '^O' \
        >"$evidence/stage-6-real-output-screen.txt"
fi

{
    echo "# stage 6-4 real provider verification"
    date
    echo "# 端点按阶段 6 安全边界脱敏；密钥只经 TUI 的 Ctrl+K 进入 Keychain，脚本与日志都不接触它"
    echo
    python3 "$root/collab/scripts/stage6_real_report.py" "$evidence/stage-6-real.json"
    echo
    echo "## 界面快照（Ctrl+D 任务详情 / Ctrl+O 任务输出，34x132，真实宿主）"
    echo
    cat "$evidence/stage-6-real-detail-screen.txt"
    echo
    cat "$evidence/stage-6-real-output-screen.txt"
    echo
    echo "## 失败分支（本机回环，无真实服务商调用）"
    echo "见 evidence/stage-6-errors.json：地址规则、Keychain 临时账户往返、401、429、传输失败、缺凭证。"
} >"$evidence/stage-6-real.log"
echo "wrote $evidence/stage-6-real.log"
