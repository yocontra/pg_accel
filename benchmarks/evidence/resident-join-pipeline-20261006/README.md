# Resident join -> gather -> aggregate execution

This extends the row-mapping producer with a real device-side consumer boundary.
It does **not** add normal SQL planner admission, a production pipeline executor,
a fused join/aggregate kernel or a performance claim.

The gather ABI projects up to sixteen fixed-width columns from either join side
into contiguous resident lanes. One/four/eight-byte payloads preserve exact
bits; source NULLs and unmatched INNER-side rows produce zero bits plus a
canonical NULL byte. A GPU bounds-validation pass precedes output writes.
Only its scalar status returns to the host. Inputs can alias other inputs;
all writable spans must be disjoint from input, metadata and other outputs.

The typed Rust facade retains the original join side row domains, checks every
borrowed input length and reserves the complete output plus 1,160-byte device
workspace before allocating. The workspace contains the error word and bounded
column metadata. Result columns own their device allocations and charge,
independently of the pair buffer. No row-proportional host metadata allocation
or host reconstruction is introduced.

## Execution evidence

The live PostgreSQL test now runs 40 native-SQL oracle cases: five input
pairings (distinct relations, self alias, either side empty, both empty),
one/two key pairs, and inner/left/semi/anti semantics. Each case:

1. Executes the resident row join with exact output capacity.
2. Tests the sixteen-column maximum, wrong-domain rejection and budget unwind.
3. Projects five typed resident columns, including signed INT8 extremes and a
   byte lane, without downloading intermediate rows.
4. Binds two projected INT4 lanes into the existing SPG1 aggregate descriptor.
5. Runs parallel dense SUM/COUNT with the expression
   `sum(la + coalesce(ra, 10)) FILTER (WHERE la >= 0)` (and the matching COUNT)
   and compares native SQL.
6. Only after pipeline execution, downloads projected rows for an independent
   exact multiset comparison, including canonical NULL payload checks.

PG18.4 and PG19beta1 pass this expanded test. Standalone Rust runs on each major
pass 1,214 tests; full native CTest passes 34/34 and strict all-target Clippy
passes on both majors. Coverage inventory includes the new Rust module (224 files,
17 C++ sources, 34 CTests, 100 SQL files / 462 assertions); this is not a fresh
line-coverage measurement. Coverage-tool tests pass 104 cases and documentation
parity passes 21. Raw build, lint, native and checker logs are retained here.
The expanded native join/gather binary reports 3,593 successful checks.

## Found and fixed during validation

The initial native gather and PG18 composition tests passed, but adding the
sixteen-column live test aborted the PG19 backend. The macOS crash report
identified `MTLReportFailure` / `newArgumentEncoderWithBufferIndex`, called from
`encode_arguments_argbuffer` while launching `pgaccel_join_rows_project::$_0`
(the bounds-validation kernel). That kernel captured the full pointer-rich
host descriptor despite needing only the pair pointer and row domains.

Both kernels now capture only their required pointers/scalars. Column metadata
is explicitly copied into the charged fixed-size device workspace, rather than
expanded into kernel arguments. The unchanged sixteen-column limit passes on
both PostgreSQL majors after this change; the native test now checks all
sixteen columns and LEFT padding too. No cache purge, force setting, admission
limit reduction or runtime-library patch was used. The failed live log is
retained, along with an early test-only C++ array-indexing compile failure.

The conservative CPU-work audit still flags the three join exports for proof
review. Updating the ABI manifest to 126 symbols adds exactly the project
entry point; its workspace signature and integrity anchor are explicit.
Checker unit tests are not a waiver of the failing production audit.
The first final checker run also caught a header-inventory ambiguity in the
`sizeof(type)` workspace macro. The public workspace size is now an explicit
1,160-byte constant, tied to the C layout by a compile-time assertion and to
the Rust layout by its ABI test. No header is excluded; the ABI signature hash
is unchanged. The focused native test was rebuilt and rerun after that
declaration-only change. The failed checker log is retained.
The final checker regression run passes all 173 tests; the production audit
still fails on the three explicit join-export proof findings.

## Commands and boundary

All build/native/PostgreSQL test shells used `ulimit -n 4096`.

```sh
just gpu-build
ctest --test-dir pgaccel-kernels/build --output-on-failure --timeout 300
cargo pgrx test --package pg_accel pg19 pg_resident_join_rows_native_parity_and_ownership
cargo pgrx test --package pg_accel pg18 pg_resident_join_rows_native_parity_and_ownership
just test-standalone 18
just test-standalone 19
# Each lint invocation uses its matching PG_CONFIG and PGRX_PG_CONFIG_PATH:
cargo clippy -p pg_accel --no-default-features --features pg18,pg_test --all-targets -- -D warnings
cargo clippy -p pg_accel --no-default-features --features pg19,pg_test --all-targets -- -D warnings
python3 -m unittest scripts.tests.test_coverage_tools
python3 -m unittest scripts.tests.test_cpu_cheat_audit
python3 -m unittest scripts.tests.test_doc_parity
python3 scripts/coverage_tools.py audit-scope --scope coverage/scope.json --repo-root .
python3 scripts/cpu_cheat_audit.py pgaccel-kernels/src/join_rows.cpp
just install-pg-accel 19
just install-pg-accel 18
```

Relation-occurrence identities, pipeline serialization, production residency /
currentness binding, cost admission, bounded streaming/skew handling and static
proof qualification remain open. No SQL support or speedup is credited by this
direct execution test. CUDA and concurrent/over-budget workloads remain
unqualified. Existing row-returning SQL joins still run in PostgreSQL.

Recorded on Apple M2 Max / Metal, macOS 26.4.1, on 2026-10-06. Base commit is
`5c7fbd0a6381f823befd823ec47137a8a602e94d`; existing dirty work is preserved.
The source archive identifies the actual source, not that commit alone.
Normal non-pg_test modules are restored after the live tests; install logs
record their hashes. They match the previous normal modules: these staged
primitives have no production caller yet and are exercised by the native and
pg_test builds. SHA256SUMS binds the retained evidence and source archive.
