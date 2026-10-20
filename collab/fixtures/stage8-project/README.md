# Stage 8 synthetic code project

Copy `base/` into a fresh Git repository. Run `python3 -B -m unittest discover -s tests -v`.
The baseline has four tests. Each independent change adds two tests; report adds three.

- `changes/slug/`: normalize repeated whitespace, tabs, newlines and Unicode titles.
- `changes/total/`: accept integer strings and reject invalid values.
- `changes/report/`: combine both changes in a tested command-line report.

Each change overlays files relative to the project root. The report task depends on both other
changes being integrated. The mock upstream supplies these patches through real Codex shell
calls; a real model must implement the same behavior itself. No external Python packages or
network access are needed by this project.
