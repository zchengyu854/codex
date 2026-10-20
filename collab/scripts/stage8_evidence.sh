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
rm -f "$evidence/stage-8-code.json" "$evidence/stage-8-summary.json"
{
    echo '# stage 8 local code-project verification (not a real-provider run)'
    date
    echo '## workspace including installed-Codex and real-host tests'
    CODEX_COLLAB_EVIDENCE="$evidence/stage-8-code.json" \
        cargo test --locked --manifest-path "$manifest" --workspace -- --include-ignored
    echo '## clippy'
    cargo clippy --locked --manifest-path "$manifest" --workspace --all-targets -- -D warnings
    echo '## format'
    cargo fmt --manifest-path "$manifest" -p codex-collab-core -p codex-collab-tui -- --check
    echo 'format ok'
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
summary = {
    "stage": "8", "status": "8-1 complete; 8-2 and 8-3 pending",
    "mode": run["mode"], "real_provider_tested": False,
    "baseline_project_tests": run["baseline_tests"], "final_project_tests": run["final_tests"],
    "reviewed_integrations": len(integrated), "dependency_gate": "both dependencies integrated before dependent starts",
    "source_repository_clean": run["source_repository_clean"],
    "regressions": ["each feature rejects an incorrect implementation", "real project tests reject a candidate without advancing the branch"],
    "suite": {"passed": sum(n for n, _, _ in counts), "failed": 0, "ignored": 0,
              "clippy": "passed", "format": "passed"},
    "evidence": ["stage-8-code.json", "stage-8-tests.log"],
    "limits": ["mock upstream scripts the model decisions; this does not prove real model performance",
               "code conflict rework and cancellation/restart on this project remain for 8-2",
               "real-provider verification remains for 8-3; no paid calls were made"],
}
(root / "stage-8-summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
print("PASS: 8-1 code project, dependency gate, three reviewed integrations and negative checks")
print("wrote", root / "stage-8-summary.json")
PYTHON
echo "wrote $log and $evidence/stage-8-code.json"
