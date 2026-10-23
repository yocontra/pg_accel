# Candidate 2 preparation receipt

Source prepared only; no PostgreSQL, GPU, compiler, installation or project Git
command executed. Independent review is required before a runtime handoff.

- `bash -n qualification/run-known-production.sh`: exit 0.
- Python AST parse of bounded-command.py: exit 0.
- Actual `/usr/bin/true` child through bounded-command.py: exit 0.
- Actual `/bin/sh -c 'exit 7'` child through bounded-command.py: preserved exit 7.
- The initial 93-byte socket-directory proposal was rejected before execution;
  shortened sibling `qs18`/`qs19` directories are reserved by the scratch owner.
  The 93-byte path is not used. Final full socket pathname is checked below.

No timeout-path or SQL execution result is claimed. Source references checked:
resident-status column is `relid`; existing EXPLAIN exposes planned lifecycle
calls, verification and physical mode. Read README for missing overall-plan gates.

`candidate2.sha256` identifies the frozen six source files and two notes.
Reviewer feedback strengthened the actual-producing positive query with its
own positive dispatch assertion. The plan owner approved adding a measured
pinned-raw lower-bound negative, without asserting native workspace bytes.

The runtime harness.sha256 is deliberately not created until review approval.
