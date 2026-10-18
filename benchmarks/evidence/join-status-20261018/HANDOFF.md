# Terminal run receipts

All work used the exclusive Cargo/GPU slot. No Git operations were performed
by this worker. The three source files remain at the independently reviewed
candidate hashes. All negative-control changes were removed before final runs.

| Run | Terminal session | Exit | Result |
| --- | ---: | ---: | --- |
| PG18 narrow1 | 66795 | 0 | 2 passed |
| PG19 narrow1 | 24774 | 0 | 2 passed |
| PG19 old-aggregate-negative | 83869 | 1 | Expected old equality failure, 1 vs 2 |
| PG19 restored1 | 4038 | 0 | 2 passed |
| PG19 and PG18 status-unit1, sequential | 87313 | 0 | 1 passed on each major |
| PG18 visible proof | 44100 | 0 | Both wrappers, 20,704 kernels, stopped |
| PG19 visible proof | 97720 | 0 | Both wrappers, 20,704 kernels, stopped |

Every per-run source-after check passes, including the intentionally changed
negative-control source. Final candidate and installed-module checks pass.
Formatting exit 0. Final client/server warning/error/fatal scan has zero matches;
`rg` therefore returns 1. Independent `pg_ctl status` returns 3 for each stopped
cluster (its text receipt says no server running), not an unsuccessful shutdown.

Source-only Task1 edits were released after all compiler reads completed and
the full source identity manifest was recorded. GPU/Cargo ownership was released
only after both final visible proof clusters stopped. No runtime was concurrent
with another agent's GPU tests. No Task1 memory-budget edits were compiled into
these installed proof modules.

Candidate SHA256:

- bridge.rs: `6505e439d917f7fd1e9cdacc723962f2eedfdc73a7469ab2b7f8ed91fe07b49d`
- executor/join.rs: `c0de85823c39072f14381b673f3a2bbfbbe47527cc79179e4a146cfecd9499be`
- tests/join_rows.rs: `0794d8d33bb39835231e29fa10d28b3912cbdf2d499e82ad3f38c113ea830b23`

Publication is bounded to this status-accounting repair. See README for exact
filtering, negative-control provenance, native-status versus allocation limits,
and exclusions. Independent final publication review is required before commit.
