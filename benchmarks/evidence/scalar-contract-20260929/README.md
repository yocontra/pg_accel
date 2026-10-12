# Typed scalar-program foundation (not SQL admission)

SPG1 adds a bounded, pointer-free program contract and a device evaluator for
nested INT4/Boolean expressions. It is separate from the legacy expression VM:
overflow and division-by-zero are distinct failures rather than an uncertain
Boolean result. No planner, descriptor, residency or C ABI binding is enabled
by this work. AQS6 and the installed extension modules are unchanged.

The Rust validator proves stack types on every control-flow edge, requires
identical types/depth at merges, rejects unreachable instructions, allows only
forward jumps and bounds programs to 64 inputs, 256 instructions and 32 stack
slots. Decoding rejects noncanonical operands and unknown types/opcodes.
Programs cannot loop. Literal values include both INT4 endpoints and typed NULL.

The internal device evaluator supports checked +, -, *, /, %, unary negation,
comparisons, three-valued Boolean operations, NULL tests and lazy conditional,
COALESCE and Boolean branches. It avoids C++ signed-overflow/division UB,
including PostgreSQL's zero result for INT_MIN % -1. Input binding is lazy.
Executed-access checks defend bounds/types; they do not replace full host
validation of inactive paths. There is no production CPU fallback.

## Verification

- 1,164 standalone Rust tests pass, including canonical decoding, malformed
  wire mutation, exact limits, branch merges and independent interpreter tests.
- The shared `pgaccel-kernels/test/scalar_cases.txt` corpus contains 108 cases.
  Each passes Rust's full validator/reference interpreter and native PostgreSQL
  18.4/19beta1 result/SQLSTATE checks. `verification/native-oracle.sql` contains
  the actual native queries, with pg_accel disabled and transaction-local data.
- The new `test_scalar` CTest passes 759 host/device cases on Metal, including
  that shared corpus, arithmetic endpoint cross-products, exact/over-limit
  capacities, invalid bindings and skipped-input checks. Outputs are poisoned
  before the kernel, uploaded through device allocations and downloaded for
  exact comparison. A missing dispatch cannot pass on prefilled expected data.
- The complete serial native CTest suite passes 33/33, including the new test.
- Both-major Clippy, formatting, documentation parity and scope checks pass.
  All 154 coverage-helper tests pass after updating the explicit native-test
  count pin from 32 to 33 and requiring the new test/header by name. The initial
  stale-pin failure is retained alongside the successful full rerun.
  The release inventory includes the new executable header, Rust file and
  CTest: 221 owned Rust files, 33 native tests. SQL remains 90 files/418
  assertions; no new SQL support is claimed by expanding the native inventory.

The initial native oracle correctly exposed a fixture mistake: a literal
`NULL + expression` can be folded to NULL by PostgreSQL before execution. The
runtime strictness case now reads NULL from a row instead; native PostgreSQL
then evaluates the other argument and raises division-by-zero. The original
failed oracle log is retained. Future lowering must consume PostgreSQL's
already-simplified expressions, not reinterpret pre-planning SQL text.

## Remaining integration

PostgreSQL tree lowering, logical/runtime contract integration, resident
input/program ownership and budget/currentness proofs, aggregate/FILTER error
ordering, cost admission, selected SQL regression tests and measured performance
remain unfinished. The unchecked integration item in TODO.md is intentional.
This is local dirty-tree Metal foundation evidence, not a new released SQL
capability, a performance win, CUDA validation or a completed expression engine.

`source-snapshot.tar.gz` retains the exact new source and integration files;
`verification/` includes base commit, source/binary hashes, artifact hashes,
native device identity (Apple M2 Max/Metal), raw device output and test logs.
