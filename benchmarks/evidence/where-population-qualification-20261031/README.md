# Completed WHERE population and benchmark-contract qualification

Checkpoint `279b9ed397d89c5c734d99a8403263137f2ceacc` adds read-only, completed-execution native-v2 population evidence to EXPLAIN ANALYZE and registers the frozen 32-cell WHERE benchmark. This archive qualifies correctness and observability, not performance. No 32-cell timing results are included.

`qualification.patch` is the exact eleven-file change from `3811ef08840f6fbb7755b55f217fc1d7edd3b3b8` to that checkpoint. Validation4 binds those files and four Cargo inputs. Population evidence requires canonical WHERE, completed adaptive GPU execution, no pending partition, a known whole population, an equal successful-leaf sum, and at least one completed leaf. It is rescan-local, includes natural one-shot execution, and is omitted for incomplete or non-WHERE execution. Dispatch, costs, ABI and population publication are unchanged.

## Verified results

- Validation4: 626 benchmark tests and 1,366 extension unit tests passed, including the new driver and positive warm/Both gate cases. Formatting, strict benchmark/extension lint and PG18/19 test compilation passed.
- PG18: all nine raw tests passed in 113.12 seconds; all 22 H3 tests passed in 170.27 seconds.
- PG19: all nine raw tests passed in 110.14 seconds; all 22 H3 tests passed in 162.00 seconds.
- The live queue finished with primary, secondary and terminal status zero. Exact named inventories, source, module, protected-file and harness checks passed. Full parity, actual dispatch, lifecycle, cancellation and memory checks were retained alongside the new population assertions. Test capacity limits exercise correctness; they are not natural-cap performance evidence.

## Preserved failures and corrections

Validation1 compiled the benchmark but failed six tests: the new Both fixture summarized all measured phases instead of only warm samples; the H3 registry test assumed the older contract; four generic structural tests requested unsupported scales. Corrections retain production gates and exercise both frozen WHERE scales without dropping legacy checks. Validation2 passed 625 tests, then exposed the existing SQL tokenizer's requirement for a space between the new table name and opening parenthesis; the SQL-only correction adds that space. Validation3 stopped at strict lint; an equivalent Rust oracle `is_multiple_of` expression resolved it without changing generated SQL. Validation4 passed the complete unchanged validation scope, with lint moved before the long test suite. Every failed run remains included.

## Archive and baseline-reference scope

`copy-inventory.json` identifies 115 byte-exact copied artifacts totaling 619,033 bytes, including validation1–4, all four live suites, the queue, source/harness manifests, scripts and historical review material. Compiled binaries, PGDATA and JIT caches are excluded. Historical manifests intentionally reference earlier source versions; path-based commands require the original pinned environment.

`baseline-reference/` is separate evidence for a retained production artifact, not a new baseline benchmark. Its current installed and built extension hashes both match `dde4b9950928f3f9fd74588e3460d584a9625ca400ced179cb1f8c46b34c2b7e`. The root Git audit independently compared all 363 source-manifest entries against commit `0af1949e3ef5b1e4c3dd8802f3a5011c4164e0bc`; all matched. This establishes the recorded source match and current artifact identity, not complete compiler provenance or new performance results. Build-input/recipe checks and the six fixed current-versus-baseline controls remain required.

The exact 32-cell artifact checker is still being qualified separately and is intentionally excluded from this completed archive. Full WHERE timings, the unchanged released 22-cell matrix, G4096 controls, six baseline comparisons, static source proof and release gates remain open.
