# Hybrid exact raster reclassification lookup candidate

2026-10-10. This changes the per-pixel lookup inside the existing resident
reclassification kernel. It is a compiled and host-tested candidate, **not a
GPU-qualified optimization or an expansion of selected raster SQL semantics**.

## Algorithm and boundary

The existing device validation proves unique, ascending integer source rules
within INT32_MIN..UINT32_MAX, with destinations that fit the output pixel type.
The old kernel scanned all rules until a match, up to 64 comparisons per pixel.
The new shared helper keeps that sequential policy for up to 16 rules. Larger
sets check the endpoints, reject out-of-domain misses and binary-search the
remaining interval. Only the predecessor and lower-bound source need testing.

The search compares the original pixel value, not a rounded `value ± epsilon`
threshold. Final matching retains equality or absolute difference <=
1.1920928955078125e-7. Source integers convert exactly to double and are at
least one unit apart, so two rules cannot both satisfy the epsilon test.
Endpoint shortcuts therefore preserve first-match semantics. NaN yields no
match; infinity and unmatched values still yield ordinary zero. The existing
three-argument PostGIS wrapper's source-NODATA handling is untouched.

Input validation, row mapping, output conversion, failure/nonpublication
barriers, workspace, byte accounting, C ABI, rule limits and SQL admission are
unchanged. There is no host pixel-execution fallback. The helper is called
inside the SYCL pixel kernel; its host invocation exists only in tests.
Map algebra, range reclassification, clipping and zonal statistics remain open.

## Correctness and build checks

- `test_raster_lookup` executes 1,300,000 differential checks against the former
  linear matching formula: every rule count 0..64, dense and sparse integer
  domains, raster integer extremes, exact hits, epsilon edges and neighboring
  representable doubles, float32 promotion, signed zero, subnormals, NaN,
  infinity and deterministic random bit patterns. Zero rules is an internal
  helper test, not a newly admitted ABI request.
- The same 1.3M checks pass under AddressSanitizer and UndefinedBehaviorSanitizer.
- LLVM host instrumentation covers both helper functions and all 37 mapped
  lines (100%), 95.24% of regions and 93.33% of branches. This is not device
  coverage or a complete release coverage run.
- Native shared-library/device compilation and both raster test targets build.
  The existing metadata-object macOS deployment-target linker warning remains
  in the log; it is not suppressed.
- CTest passes the host helper but fails `test_raster_resident` with exit 1 at
  `pgaccel_init`: Metal exposes no device. This is a failure, not a passing skip.
- All 104 coverage-tool tests pass after adding the new executable header and
  the 36th CTest. Scope stays 229 Rust files, 18 C++ sources, 100 SQL files,
  462 semantic assertions, 43 semantic families and the unchanged 90% floor.
  Safety-contract and documentation checks are retained alongside the logs.
- The freshly rebuilt library's full 130-export audit passes the raster
  entrypoint and 122 other exports. It still fails the same seven grouped-
  aggregate/join exports as the preceding milestone. No new failure, exemption
  or ABI rebaseline was introduced. The audit exit status is 1, not a release
  pass; `release-audit.json` and the native binary hashes retain that boundary.

No PostgreSQL server or installed extension module was modified. Live SQL
parity, full raster output-byte equivalence on a device, GPU timing, concurrent
behavior and CUDA qualification were not refreshed.

## Host timing diagnostics

`test_raster_lookup --bench` runs one million values per cell, three warmup
pairs and 21 measured pairs in alternating arm order. Both arms consume the
same inputs and compare checksums; the separate full semantic test compares
each result individually. Raw paired timings are retained, not just medians.
These isolate a host lookup loop; they do not measure raster transfer, validation,
pixel decoding, reconstruction or GPU execution.

The first binary-only version regressed first-rule hits severely (12.7x at
64 rules) and small sets. `rejected-binary-source.tar.gz` and
`rejected-binary-bench.jsonl` preserve it; it is not the final candidate.
`host-bench-final.jsonl` is an initial hybrid diagnostic that overlapped the
coverage-tool suite and is not used as the final timing result. The expanded
30-cell `host-bench-isolated.jsonl` ran after that suite finished and before
native rebuilding/auditing, without overlapping build/test work.

In that isolated run, candidate/linear median ratios for 64 rules were 0.874
(first hit), 0.041 (last hit), 0.250 (mixed), 0.024 (outside-range miss), 0.357
(epsilon boundary) and 0.288 (interior miss). Ratios for the 17-rule crossover
were 0.090..0.864. Smaller 1/4/16-rule cases still ranged from 1.021 to 1.284,
so their overhead is explicitly unresolved. GPU behavior can differ materially;
no general speedup or completed performance qualification is claimed.

## Reproduction

```sh
clang++ -O2 -std=c++17 -Wall -Wextra -Werror -I pgaccel-kernels/include \
  pgaccel-kernels/test/test_raster_lookup.cpp -o /tmp/test-raster-lookup
/tmp/test-raster-lookup
/tmp/test-raster-lookup --bench
cmake -S pgaccel-kernels -B pgaccel-kernels/build
cmake --build pgaccel-kernels/build --target test_raster_lookup \
  pgaccel_kernels_shared test_raster_resident --parallel 2
ctest --test-dir pgaccel-kernels/build -R '^test_raster_(lookup|resident)$' --output-on-failure
```

The source snapshot captures the complete dirty worktree. `SHA256SUMS` binds
this evidence bundle. The native export audit result is retained separately;
the broader release remains unqualified. Git metadata is read-only, so these
changes have not been committed or pushed.
