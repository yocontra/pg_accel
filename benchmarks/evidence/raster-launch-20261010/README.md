# Launch-specialized raster lookup candidate

2026-10-10. Follow-up to `raster-lookup-20261010`: remove the per-pixel
linear/indexed policy branch by choosing one of two SYCL kernels per launch.
This is **not GPU performance qualification or live PostgreSQL parity**.

## Runtime change

Rule sets of at most 16 retain the linear lookup; 17..64 use endpoint checks
and binary search. Both helpers preserve the original FLT_EQ formula. Pixel
mapping, decoding, NODATA treatment, output conversion, chunk limits and the
validation failure barrier are identical in the two kernel bodies. The rule
validation still proves increasing, unique integer sources in
INT32_MIN..UINT32_MAX before lookup. ABI and SQL admission are unchanged.

No host pixel fallback, additional supported SQL shape or CPU runtime path
was introduced. The host helper invocations below are test-only.

## Verification

- All 1,300,000 host differential comparisons pass, including the same suite
  under ASan/UBSan. The oracle is the former linear formula, not either helper.
- LLVM host coverage reports all three helper functions and all 37 mapped
  lines executed, 95% regions and 92.86% branches. This is not device coverage.
- Native shared-library/device compilation and both raster test targets pass.
  The existing metadata-object macOS deployment-target warning is retained.
- Added resident-device coverage exercises 16/17/64 rules over three integer
  domains, every rule's exact/epsilon/adjacent-double matches, misses, signed
  zero, subnormal, NaN and infinity. It compares complete INT16 output bytes
  to an independent sequential oracle across multiple 127-pixel launches,
  checks unchanged input bytes, duplicate-rule nonpublication and IS_NODATA
  handling. These new device cases compile but **have not executed here**.
- CTest passes `test_raster_lookup`. `test_raster_resident` fails with exit 1
  during `pgaccel_init` because no SYCL GPU is visible; CTest exits 8. It is
  not a passing skip and does not qualify the new device cases.
- Safety-contract checks, documentation checks (21 unit tests), coverage-scope
  audit and whitespace checks pass. Scope remains 229 Rust files, 18 C++
  sources, 36 CTests, 100 SQL files, 462 assertions and 43 semantic families
  with the unchanged 90% threshold.
- The fresh full native export audit passes 123 of 130 exports, including the
  raster entrypoint. It exits 1 with the same seven grouped-aggregate/join
  findings as the preceding bundle; no new exemption or ABI rebaseline was
  added. The release audit is still failing. This static result does not
  replace execution of the output-byte tests on a GPU.

## Host timing diagnostic

`host-bench.jsonl` retains 30 cells, each with one million values, three warmup
pairs and 21 measured pairs in alternating order. The candidate helper is
chosen outside the timed loop. Both arms use the same explicitly non-inlined
function-pointer timing driver; this prevents asymmetric driver inlining.
Checksums must agree in every pair. Full per-value comparison is performed by
the separate semantic suite.

The 1/4/16-rule candidate/linear median ratios range from 0.978 to 1.016.
For 17 rules they range from 0.116 to 0.732. At 64 rules:

| Input pattern | Candidate / linear median | Isolated repeat |
| --- | ---: | ---: |
| First-rule hit | 0.745 | 0.752 |
| Last-rule hit | 0.039 | 0.039 |
| Mixed hits/misses | 0.262 | 0.264 |
| Outside-range miss | 0.029 | 0.029 |
| Epsilon boundary | 0.352 | 0.353 |
| Interior miss | 0.284 | 0.285 |

The independent repeat, `host-bench-repeat.jsonl`, completed with exit 0 after
the native export audit terminated, without overlapping build/test work. Its
1/4/16-rule ratios range from 0.988 to 1.007, and its 17-rule ratios range from
0.116 to 0.748. All 30 cells and all 21 measured pairs per cell are retained.

These are host lookup-loop diagnostics, not GPU or query speedups. They omit
transfers, device validation, pixel decoding and PostgreSQL reconstruction.
The driver changed from the preceding bundle, so compare paired arms within
this run, not absolute timings between bundles. Older evidence is preserved.

## Reproduction and remaining gates

```sh
clang++ -O2 -std=c++17 -Wall -Wextra -Werror -I pgaccel-kernels/include \
  pgaccel-kernels/test/test_raster_lookup.cpp -o /tmp/test-raster-lookup
/tmp/test-raster-lookup
/tmp/test-raster-lookup --bench
cmake --build pgaccel-kernels/build --target test_raster_lookup \
  pgaccel_kernels_shared test_raster_resident --parallel 2
ctest --test-dir pgaccel-kernels/build -R '^test_raster_(lookup|resident)$' --output-on-failure
```

No installed PostgreSQL module or server was modified. Live PG18/19 parity,
device output equivalence and device timing remain required. The raster TODO
qualification stays unchecked; broader raster and release work is not done.
Git metadata remains read-only, so these changes are not committed or pushed.

The complete dirty-worktree source snapshot and SHA256SUMS bind this bundle.
