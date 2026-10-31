# Offline raw-integer COUNT matrix checker

Owner: `raw_count_bench`. Files: `check-matrix.py`, `test_check_matrix.py`, this note.
No benchmark source, CLI, build, runtime wrapper, or PostgreSQL changes are owned here.

Run after the engine has finished all eleven lane wrappers:

```
python3 .codex/scratch/raw-integer-count-20261026/check-matrix.py \
  --build-dir .codex/scratch/raw-integer-count-20261026/build18-QUALIFIED_STAGE \
  .codex/scratch/raw-integer-count-20261026/performance18-EXACT_LANE_STAGE ...
```

Supply exactly one directory per lane. The checker never discovers a preferred
run, retries a losing sample, executes a command from evidence, or accesses a
database. Exit 0 means the complete sealed warm matrix passed; exit 1 includes
errors in JSON. This does not replace independent source/safety review, PG18/19
correctness, cancellation/capacity qualification, or cold-cache qualification.

## Matrix

- Mandatory: `raw_int{2,4,8}_count_{star,bool_filter}`, each at 1,000,017 and
  2,000,017 rows: twelve cells, each >=1.15x median warm speedup.
- Dense controls: `dense_int{2,4,8}_count_control`, same two scales, 64 non-NULL
  groups plus NULL; dense physical COUNT mode and at least parity required.
- Released H3 controls: `h3_parent_count_int4_res0` and
  `h3_parent_count_bool_res0`, same scales; resolution-zero typed COUNT/hash
  descriptors and their existing 1.15x floor retained.

Every cell retains five warmups and ten balanced raw pairs. Samples must be
finite and positive. Summary medians and sample CVs are recomputed and compared
to saved summaries. CV >15% is flagged, not smoothed or dropped. Lifecycle and
combined refresh-inclusive speedups are disclosed separately, including losses;
they are not silently substituted for the predeclared warm floor.

Raw cells require exact same-node FactColumn/hash/single COUNT/direct nullable
BOOL FILTER identity, with static and separate actual ANALYZE relation identity
equal. Actual proof requires verified parallel_hash, ephemeral_fused, positive
kernel delta, same-node `GPU Lifecycle Calls Verified: true`, zero stock
fallback, and default extension settings. Duplicate node properties (including
contradictory lifecycle flags) and child-borrowed lifecycle flags are rejected. Every
warmup/measured/lifecycle settings capture is required. Native timing-arm kernel
deltas must all be zero. Lifecycle and all steady samples require complete
dependency stamps, real refresh costs, zero-buffer ephemeral preparation,
positive dispatch/output, and no hidden steady refresh.

Parity files must prove both full EXCEPT ALL directions empty, matching query
identities and exact raw output cardinality. Canonical raw query SQL is checked
against the pre-timing fixture manifest and parity artifact. The manifest must
carry exact scale/group/seed/SQL metadata; source seals bind its generator.

## Engine wrapper interface and trust boundary

The engine's approved `run-benchmark.sh` writes `build-dir.txt` linking the exact
qualified build, and seals its completed evidence with `evidence.sha256` after
writing `terminal.log`. Required inventory includes report/lane/build link,
terminal, harness seal, source/module/harness verification receipts, bench and
device/extension/status/stop logs, and **all** artifact files. Missing required
entries, escaping paths, byte changes, duplicate JSON keys, incomplete pairs,
missing reports/cells, failed terminals, and wrong build links fail closed.

Build source/module/PostgreSQL/harness hashes are checked against current files
within the repository root. The build must use the canonical build18 directory;
runs must use canonical performance18 directories under the same task folder.
The benchmark executable and run/build wrapper identities have exact expected
paths; source manifests must include core qualification inputs. Existing
after-run verification receipts must cover every corresponding sealed input.
The module seal must contain exactly the qualified
`.codex/scratch/uniform-budget-live-20261012/postgres/18.4/` installation's
`lib/pg_accel.dylib`, `bin/postgres`, and `bin/pg_config`; matching basenames from
another prefix do not qualify, even with internally consistent replacement seals.

Hashes establish consistency, **not cryptographic attestation against someone
rewriting both data and seals**. Terminal/cleanup receipts come from the reviewed
wrapper; the checker does not independently observe process history. Its output
includes its own source hash. Keep original failures and all input artifacts.

## Hermetic tests

```
python3 .codex/scratch/raw-integer-count-20261026/test_check_matrix.py
```

Synthetic fixtures are deliberately labelled non-production and never represent
measured qualification. Tests cover complete 22-cell acceptance, refresh losses,
missing/duplicate lanes and cells, wrong lane/scale, report crashes, absent actual
proof, persistent-policy substitution, wrong defaults, nonfinite samples,
changed report bytes, omitted report/artifact seals, wrong build linkage, failed
terminals, sample-derived floor failures, and visible high variance.
