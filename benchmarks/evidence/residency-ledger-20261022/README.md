# Maintained production residency-ledger qualification

The maintained harness now includes each owner's integer and NUMERIC workspace
and output capacities alongside raw/derived residency. Expected global bytes are
computed independently from owner receipts, never by subtracting observed global
bytes. Checked integer arithmetic and database/PID/phase identities reject
incomplete or inconsistent receipts. Backend absence and owned-client shutdown
are bounded; timeout is not accepted as the intentional fatal-client result.

All 15 accounting and real-child-process helper tests pass. The unchanged final
harness passes on fresh PG18.4 and PG19beta1 production candidate11 clusters,
sequentially on Apple M2 Max with hardware-derived limits and zero installed test
functions. Both final runtime evidence reviews approved this bounded result.

Both runs preserve the prepared integer workload, trigger fingerprint repair
and +1000 result, concurrent invalidation, cross-database same-OID isolation,
NUMERIC exact native parity and explicit dispatch, warm pool hits, forced B exit,
slot reuse, normal A exit and final zero global ownership. All eight independent
equations pass: ready, trigger_refreshed, refreshed, numeric_a, numeric_b,
b_exited, slot_reused and all_exited. Raw components and separate observer PIDs
are retained in each `artifacts/ownership.jsonl`; `accounting.log` and
`observer.tsv` retain the evaluated equations and actual reads.

The original relation-only equation fails against **each final run's actual
ready receipts**: expected 10,000,000 versus observed 10,013,775, omitting 13,775
bytes of integer idle pools. These exit-1 negative controls are receipt-backed
counterfactuals, **not executions of the old complete script**.

## Identities and reproduction

`source.sha256` binds the four maintained files; their source is carried by the
commit. Each major retains installed-module, scoped-source and before/after
harness/module checks. The production module hashes are:

- PG18: `181d4fbe1b9b1b79f0cf98ea8e22092649dbb1d98924f93e6181e53039391a31`
- PG19: `b35df129493533c50eda522f7e91c6e5b1e0e774b58b3f6dfa7b67962d3a7e44`

The installed-source manifests identify eight Task1 implementation/test files
plus SQL121, not a complete repository build. PG19 installation preceded the
separate join-test correction; PG18 followed it. Both production builds exclude
`pg_test`, so that test-only difference does not change the production engine.
The verified source snapshot is retained in scratch under
`residency-ledger-20261018/installed-sources/candidate11/`; it is not duplicated
here. These runtime identities must not be confused with later pgrx installs.

`run-maintained-final.sh` preserves the actual owned-prefix runner, including
fresh-path guards, nonempty-PGOPTIONS rejection, production/GPU checks and bounded
stop/status checks. It ran with arguments `18`, then `19`; both exited zero.
It deliberately refuses to reuse its existing directories. No admission-cost or
row-threshold override was added. No original PG17 service was touched. Stop
receipts report no running server; owned postmaster.pid files were also checked
absent before releasing the runtime slot.

Repository-root verification commands:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts/tests/test_residency_ledger_accounting.py -v
bash -n scripts/residency_ledger_integration.sh
shasum -a 256 -c benchmarks/evidence/residency-ledger-20261022/source.sha256
python3 benchmarks/evidence/residency-ledger-20261022/check-obsolete-equation.py \
  benchmarks/evidence/residency-ledger-20261022/pg18/artifacts/ownership.jsonl
python3 benchmarks/evidence/residency-ledger-20261022/check-obsolete-equation.py \
  benchmarks/evidence/residency-ledger-20261022/pg19/artifacts/ownership.jsonl
```

The final two commands must exit 1 at `OBSOLETE_RELATION_ONLY_EQUATION_FAILED`.
Verify the package with `shasum -a 256 -c SHA256SUMS` from this directory.

## Warnings and limits

PG18 emitted one PostgreSQL one-shot dispatch warning: **5,518,967us against the
unchanged 5000ms threshold**, recorded in its client and server logs. PG19 had no
PostgreSQL WARNING/ERROR/PANIC. Both runs contain the intentional administrator
FATAL for B (psql exit 2), and each server log contains **42 LLVM SDK-link
warnings**. Full raw logs retain them; this is not a warning-free result.

Pool telemetry is capacity, not a new general exact-charge API. These controlled
idle fixtures satisfy the independent ownership equation; they do not prove
active-output protection, concurrent throughput, latency, full suites, hosted CI
or release readiness. Neither final run reproduced a delayed backend-exit race:
both absence polls observed zero on their first read. No binaries, databases,
caches or redundant earlier candidate8/9 runtime logs are included.
