# Join workspace metadata boundary — static and GPU qualification pending

2026-10-10 UTC (2026-10-09 Arizona). This checkpoint separates the two public
workspace queries from GPU dispatch without changing their signatures, layout
arithmetic, validation order or error contracts. It does not enable SQL joins.

`join_rows_layout.cpp` contains the two query exports. Its internal helper header
is shared with the execution source, so querying and allocating cannot drift
between duplicate layout implementations. No SYCL header or GPU library is
needed to compile and link the query translation unit.

## Verified boundary

The new `test_join_rows_layout` host CTest builds the real query source, not a
mock or a CPU execution backend. It reserves inaccessible virtual row storage
with `PROT_NONE`; touching row values or NULL bytes faults. Its independent
byte-accounting oracle covers empty inputs, power-of-two hash boundaries,
256-way prefix boundaries through four levels, INT32_MAX rows, 1–4 mixed integer
keys, all four join kinds and all NULL-sidecar presence combinations. Limits of
1 and INT32_MAX produce the same queried size. A callback aborts if invoked.
Queries must preserve their input descriptors and rejected outputs.

Negative cases cover versions, tags, limits, inactive keys, widths, lengths,
misalignment, address overflow, NULL parameters, output aliases with resident
inputs/request/control, and recovery after rejected requests.

- Standalone Clang build: **40,389 checks passed**, `-Wall -Wextra -Werror`.
- AddressSanitizer + UndefinedBehaviorSanitizer: **40,389 passed**.
- Same matrix linked to the rebuilt native shared library: **40,389 passed**.
- CMake host CTest passes. The standalone executable links only libc++ and
  libSystem; it cannot discover a GPU, dispatch work or call a GPU allocator.
- Native build and PG18/19 core suites pass: **1,249 tests on each major**.
- Safety contracts and documentation parity (21 tests) pass.
- Analyzer regression suite: **180 passed**. Coverage-tool suite: **104 passed**.
- Actual join-source audit: **fails on seven exports**, now across the execution
  and layout translation units. Header-helper effects remain unresolved; source
  separation has not approved those effects or closed any production proof.
- All 130 ABI exports remain inventoried, with unchanged manifest SHA-256
  `9813eb27ab109110f8856b326f278edf741f2d5a5226e1b72de7ff66459c154f`.
- Coverage scope passes at the unchanged 90% threshold, without exclusions:
  228 Rust files, 18 C++ sources, 35 CTests, 100 SQL files and 462 assertions
  across 43 semantic families. The reviewed inventory addition is one source,
  one executable helper header and one CTest; no previous members were removed.
- The GPU join CTest still **fails with exit 77**, because no device is visible.
  That is not a passing skip. No live PostgreSQL tests or performance runs were
  attempted, and no extension module was installed or server restarted.
  Installed PG18/19 module hashes match the prior count-path checkpoint.

This separation and runtime evidence are prerequisites for a conservative
metadata-only helper-chain proof. They do not provide that static proof. No
export was added to a metadata/lifecycle exemption, no audit requirement was
weakened, and no join qualification checkbox was checked. The prior parser fix
also remains in this checkpoint; it distinguishes member reads from genuine
`using`-declaration shadowing.

The full TODO objective, normal SQL admission, GPU cancellation/parity tests,
cost calibration and performance qualification remain open. Git metadata is
read-only in this session, so this is an uncommitted dirty-worktree checkpoint,
not a pushed change or release certification.

`source-snapshot.tar.gz` and `SHA256SUMS` bind the source and retained logs.
`layout-helper-ast.json.gz` is Clang's typed helper syntax tree, collected to
support the next proof implementation; dumping an AST is not a passing audit.
