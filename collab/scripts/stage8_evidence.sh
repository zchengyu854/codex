#!/usr/bin/env bash
# Stage 8 local verification. Real Codex processes talk only to local mock upstreams;
# no Keychain credential or paid provider is used. Requires Python 3, Git and installed Codex.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
manifest="$root/collab/Cargo.toml"
evidence="$root/collab/evidence"
log="$evidence/stage-8-tests.log"

cargo build --locked --manifest-path "$manifest" -p codex-collab-core -p codex-collab-tui >/dev/null
# Do not leave an earlier successful JSON looking like the result of a failed replay.
rm -f "$evidence/stage-8-code.json" "$evidence/stage-8-summary.json" \
    "$evidence/stage-8-failure.json" "$evidence/stage-8-conflict.json" "$evidence/stage-8-interruption.json"
{
    echo '# stage 8 local code-project verification (not a real-provider run)'
    date
    echo '## workspace including installed-Codex and real-host tests'
    CODEX_COLLAB_EVIDENCE="$evidence/stage-8-code.json" CODEX_COLLAB_RECOVERY_EVIDENCE="$evidence" \
        cargo test --locked --manifest-path "$manifest" --workspace -- --include-ignored
    echo '## clippy'
    cargo clippy --locked --manifest-path "$manifest" --workspace --all-targets -- -D warnings
    echo '## format'
    cargo fmt --manifest-path "$manifest" -p codex-collab-core -p codex-collab-tui -- --check
    echo 'format ok'
    echo '## pty demo probe'
    python3 "$root/collab/scripts/probe_terminal.py" "$root/collab/target/debug/codex-collab-tui"
    echo '## pty live probe'
    python3 "$root/collab/scripts/probe_terminal_live.py" "$root/collab/target/debug/codex-collab-tui" "$root/collab/target/debug/codex-collab-core"
    echo '## screen snapshot (demo, 34x132)'
    python3 "$root/collab/scripts/snapshot_terminal.py" "$root/collab/target/debug/codex-collab-tui" \
        --demo --rows 34 --cols 132 | tee "$evidence/stage-8-demo-screen.txt"
} >"$log" 2>&1
python3 - "$evidence" <<'PYTHON'
import json
from pathlib import Path
import re
import sys
root = Path(sys.argv[1])
run = json.loads((root / "stage-8-code.json").read_text())
assert run["verdict"]["ok"] and not run["real_provider_tested"]
assert run["baselineCheck"]["exitCode"] == run["finalCheck"]["exitCode"] == 0
integrated = [e for e in run["events"] if e["kind"] == "integration" and e["data"]["status"] == "integrated"]
assert len(integrated) == 3
assert all(e["data"]["evidence"]["checks"][0]["exitCode"] == 0 for e in integrated)
counts = [tuple(map(int, row)) for row in re.findall(
    r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;",
    (root / "stage-8-tests.log").read_text(),
)]
assert counts and all(failed == ignored == 0 for _, failed, ignored in counts)
failure = json.loads((root / "stage-8-failure.json").read_text())
conflict = json.loads((root / "stage-8-conflict.json").read_text())
interruption = json.loads((root / "stage-8-interruption.json").read_text())
assert all(r["verdict"]["ok"] and r["sourceClean"] for r in (failure, conflict, interruption))
assert failure["failure"]["checks"][0]["exitCode"] == 1
assert failure["replacementIntegration"]["status"] == "integrated"
assert conflict["conflict"]["status"] == "conflict"
assert conflict["abort"]["head"] == conflict["firstIntegration"]["head"]
assert conflict["reworkTask"]["baseCommit"] == conflict["firstIntegration"]["head"]
assert conflict["reworkIntegration"]["status"] == "integrated"
assert interruption["processGroupStopped"] and not interruption["replayed"]
assert interruption["committedTaskBeforeRecovery"]["commit"] == interruption["committedTaskAfterRecovery"]["commit"]
assert interruption["abortInterruptedIntegration"]["status"] == "pending"
summary = {
    "stage": "8", "status": "8-1 and 8-2 complete locally; 8-3 pending",
    "mode": run["mode"], "real_provider_tested": False,
    "baseline_project_tests": run["baseline_tests"], "final_project_tests": run["final_tests"],
    "reviewed_integrations": len(integrated), "dependency_gate": "both dependencies integrated before dependent starts",
    "source_repository_clean": run["source_repository_clean"],
    "regressions": ["each feature rejects an incorrect implementation", "real project tests reject a candidate without advancing the branch"],
    "recovery": {"failed_candidate_reworked": True, "code_conflict_reworked": True,
                 "cancelled_process_group_stopped": True, "committed_work_preserved_on_restart": True,
                 "crash_recovery_mode": "persisted crash-state simulation", "automatic_replay": False},
    "suite": {"passed": sum(n for n, _, _ in counts), "failed": 0, "ignored": 0,
              "clippy": "passed", "format": "passed", "pty_demo": "passed", "pty_live": "passed"},
    "evidence": ["stage-8-code.json", "stage-8-failure.json", "stage-8-conflict.json", "stage-8-interruption.json", "stage-8-tests.log", "stage-8-demo-screen.txt"],
    "limits": ["mock upstream scripts the model decisions; this does not prove real model performance",
               "cancellation uses a fixture App Server; restart uses persisted crash states, not a forced production-host crash",
               "real-provider verification remains for 8-3; no paid calls were made"],
}
(root / "stage-8-summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
print("PASS: 8-1/8-2 code project, dependency gate, failure/conflict rework, cancellation and restart")
print("wrote", root / "stage-8-summary.json")
PYTHON
echo "wrote $log and $evidence/stage-8-code.json"
