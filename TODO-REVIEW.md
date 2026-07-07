# TODO-REVIEW — Project Review Findings (2026-07-06)

Action items from a full project review (5 parallel reviewers: engine core, FFI/planner,
GPU bridge/kernels, bench/tooling, architecture/docs). Ranked by severity. Every item
cites `file:line` verified during the review. Check off as fixed; delete when done.

Legend: **P0** = wrong results / credibility-enders. **P1** = crashes, UB, safety-rule
violations. **P2** = architecture & methodology debt. **P3** = tooling, docs, hygiene.

---

## P0 — Silently wrong query results (production disqualifiers)

- [ ] **Resident caches have zero invalidation — DML after load returns stale results.**
  No relcache/syscache/xact callbacks anywhere in `pg_accel/src` (grep for
  `RelcacheCallback|XactCallback` is empty). Thread-local device caches at
  `pg_accel/src/engine/olap_cache.rs:1985-1991` are keyed only by rel OID/shape; only
  manual `pg_accel_clear_*` (`olap_cache.rs:4419-4442`) exists. Fix: register relcache
  invalidation callbacks keyed on cached rel OIDs, or verify relfilenode/snapshot
  freshness at execution and hard-fail on mismatch.

- [ ] **NULL rows pass WHERE filters in fused aggregation.** `fused_eval_cmp` returns
  `true` (row passes) for SQL NULL, for non-fast-extractable columns, and for
  unsupported templates (`pg_accel/src/engine/executor/agg/execute.rs:5097-5131`, `_ => true`
  at `:4788`), feeding aggregate inclusion at `:4772-4783` with no re-check. Result:
  `COUNT(*) WHERE x < 5` counts rows where `x IS NULL`. Fix: NULL = filter-fail per SQL
  semantics; slot-deform fallback (not blanket pass) for unextractable rows.

- [ ] **Preceding varlena/NULL columns corrupt extraction — values silently become NULL.**
  `try_fast_read_heap` conflates real NULL, preceding-NULL rows, and un-fast-extractable
  layouts into one `None` (`pg_accel/src/engine/materialize/tuple_extract.rs:1090-1095`);
  arena extractors mark all of them NULL (`executor/vectorized_scan.rs:398-523`), and the
  fused agg loop silently drops them (`executor/agg/execute.rs:4029-4071`), corrupting
  SUM/AVG/COUNT. Fix: distinguish via `heap_attr_is_null_pub` (`tuple_extract.rs:865`)
  and add the slot fallback that `push_heap_value` already has (`execute.rs:608-621`).

- [ ] **PostGIS raster pixel-type decode table is wrong — shifted type codes.**
  `PixelType::from_code` (`pg_accel/src/adapters/extractors/raster.rs:75-88`) skips the
  2-bit/4-bit codes so every wider type shifts: real 8BUI decodes as UInt16, 32BSI
  decodes as a Float32 bit-pattern, 32BF is rejected. Unit tests can't catch it — the
  test builder `pixel_type_to_code` (`raster.rs:721-733`) mirrors the same wrong table.
  Fix: match `librtcore.h` `rt_pixtype` (0=1BB, 1=2BUI, 2=4BUI, 3=8BSI, 4=8BUI, 5=16BSI,
  6=16BUI, 7=32BSI, 8=32BUI, 9=32BF, 10=64BF); add a round-trip test against a raster
  produced by actual PostGIS.

- [ ] **Window functions read every datum as raw f64 bits with no type dispatch.**
  Garbage results for int4/int8/float4 columns
  (`pg_accel/src/engine/executor/window/functions.rs:79-83, 117-120`); partition
  boundaries compare raw `datum.value()` — pointer compare for by-ref types, `-0.0 != 0.0`
  (`executor/window/frame.rs:44-52`). Fix: dispatch on typid like
  `tuple_extract::extract_f64`; compare typed values.

- [ ] **Window SUM over an all-NULL partition returns 0.0 instead of SQL NULL.**
  `unwrap_or(0.0)` with `is_null=false` at
  `pg_accel/src/engine/executor/window/mod.rs:217-225, 372-379`. Fix: track a Sum null
  mask as Lag/Lead already do (`:226-241`).

- [ ] **Preagg dimension build: NULL keys join as key 0; duplicate keys silently keep
  last row; unchecked attno indexing.**
  (`pg_accel/src/engine/executor/preagg/mod.rs:333-337, 342-364`). SQL NULL must never
  match a join key. Fix: skip NULL-key inserts, error (or multi-map) on duplicates,
  bounds-check + translate relation attno → slot index (fact side already does, `:482-514`).

- [ ] **Raster ops return the input raster as the "result" on parse/extract failure.**
  clip/reclass/resample/slope/aspect/hillshade arms push the original raster with
  `is_null=false` on failure (`pg_accel/src/engine/dispatch/raster.rs:686-693, 745,
  815-826, 869, 1111-1118, 1358-1370`) while mapalgebra correctly errors (`:619-623`).
  Fix: error on failure; never return input as output.

- [ ] **GPU result-lane lengths validated only with `debug_assert` — release builds
  silently drop groups.** Short `count_by_group` truncates the emission loop
  (`pg_accel/src/engine/executor/olap.rs:1900-1917, 2075, 2097, 2149-2173, 2452`).
  Fix: promote to runtime `pgrx::error!`.

- [ ] **Async kernel failures produce PGACCEL_OK + zeroed results.** Bare `.wait()`
  instead of `.wait_and_throw()` at `pgaccel-kernels/src/expr_eval.cpp:1082,1127` and
  `pgaccel-kernels/src/expr_templates.cpp:2412,2518,2582,2628,2736` — async launch
  failures never reach the handler; the function memcpys a zero-initialized device
  buffer and returns OK (the exact failure mode documented at
  `device_manager.cpp:204-217`). Same bug in `alloc_helper.h:24-29,38-41` (unchecked H2D
  copy → kernel reads uninitialized device memory). Fix: `wait_and_throw()` everywhere a
  dispatch completes; map exceptions to status codes.

- [ ] **Constant qual args decoded by assumed type — typid discarded.**
  `f64::from_bits(d.value())` / `as i32` on constants whose type OID is ignored
  (`pg_accel/src/engine/dispatch/raster.rs:1193-1194, 1245-1247, 1291-1294`,
  `dispatch/spatial.rs:321`, `dispatch/h3.rs:95, 400, 429, 461, 707`). An int4 constant
  reinterpreted as f64 bits passes finite guards and yields wrong slope/hillshade/dwithin
  results. Fix: check typid; defer or convert on mismatch.

- [ ] **Grouped-AVG gate can be bypassed — AVG nested under CASE/COALESCE/ArrayExpr is
  invisible** to the 7-node-tag whitelist walker
  (`pg_accel/src/engine/ffi/planner_hooks/agg.rs:104-155`); the shared injector would
  emit raw SUM for AVG (latent while the generic path is gated off). Fix: use
  `pg_sys::expression_tree_walker` or recurse through all List-bearing nodes.

- [ ] **`-ffast-math` applied to kernels whose contracts require strict FP.**
  `pgaccel-kernels/CMakeLists.txt:68-92` opts out six files but not `window.cpp`
  (Kahan compensation at `window.cpp:435-443` can be algebraically deleted by
  reassociation), not `reduce.cpp` (NaN-propagation contract at `pgaccel_ffi.h:241-244`
  is void under `-ffinite-math-only`), not `olap_ssbm.cpp` (fmin/fmax NaN reliance at
  `olap_ssbm.cpp:4163-4164`). Fix: add them to the strict-FP list or scope per-kernel.

- [ ] **`strip_child_cpu_quals` clears `bitmapqualorig` on lossy bitmap scans** —
  removes PG's lossy-page recheck with no assertion that the custom path re-evaluates
  the exact stripped quals (`pg_accel/src/engine/ffi/custom_scan/mod.rs:1654-1668,
  1954-1962`). Fix: keep `bitmapqualorig` intact, or prove/assert the invariant.

- [ ] **Numeric overflow clamped to wrong values in query output.**
  `i32::try_from(x).unwrap_or(i32::MAX)` (`pg_accel/src/engine/executor/olap.rs:1092,
  1229, 1394, 2596`; `olap_cache.rs:826, 1462`); `count_by_group` is `Vec<u32>` and wraps
  past 2^32 rows/group (`olap.rs:831, 2114-2145`). Fix: error on overflow; widen to u64.

## P0 — Benchmark integrity & credibility ("nailed on HN" items)

- [ ] **Remove the SSBM benchmark recognizer from the planner.** `ssbm_q1.rs` hardcodes
  benchmark table names (`"ssbm_lineorder"`, schema-unqualified match at
  `ssbm_q1.rs:2104-2108`) and attnos (`LO_DISCOUNT_ATTNO: i32 = 12`,
  `ssbm_q1.rs:27-60`), injected ahead of generic paths
  (`planner_hooks/mod.rs:232-245`). The SSBM identity pierces all four layers: FFI
  entrypoints `pgaccel_expr_template_ssbm_q1..q4_*` (`pgaccel_expr.h:362-464`), the
  4,422-line `pgaccel-kernels/src/olap_ssbm.cpp`, executor enums `SsbmQ1RevenueSpec` /
  `SsbmQ2Variant` (`executor/olap.rs:29-37`). The README headline numbers (11.52x
  geomean, 70.8x Q3.1 — README.md:122-141) are produced by benchmark-named code paths.
  Fix: fold Q1–Q4 into the general star-groupagg matcher (catalog-driven shape
  recognition by OID/type, not name/attno); benchmark names appear only in `pg_accel_bench`.

- [ ] **The generic product is welded shut: `gpu_resident_pipeline_required()` returns
  hardcoded `true`** (`planner_hooks/mod.rs:364-367`), making ~10k+ lines of generic
  planner code (agg/window/preagg/sort/join injection, `mod.rs:777-3658`,
  `rel_pathlist.rs:2570+`, all of `join_pathlist.rs`) unreachable in every
  configuration. Decide the product: make the gate a real runtime policy with a path to
  re-enabling host-staged admission, or delete/feature-gate the dead lanes and document
  that current dispatch is resident-cache-only.

- [ ] **The bench harness fabricates decline evidence and then verifies it.** When the
  static matrix expects a decline and no Custom Scan appears, the runner appends a
  synthetic `"pg_accel benchmark threshold decline reason: <expected>"` line to the plan
  snippet (`pg_accel_bench/src/runner.rs:4095-4107`), and
  `native_decline_reason_verified` accepts that injected line as verified evidence
  (`pg_accel_bench/src/report.rs:2089-2096`). Fix: only accept
  `pg_accel planner rejection reason:` lines sourced from
  `pg_accel_last_planner_rejection_reason()` (`runner.rs:4117`); render unconfirmed
  expectations as "expected, unconfirmed".

- [ ] **Resident lanes pre-load caches off the clock while the PG baseline pays scan I/O
  on the clock**, and both flow into the same headline geomean with no marker
  (`pg_accel_bench/src/runner.rs:1625-1750`; geomean at `report.rs:3112-3118`). Fix: add
  a resident-cache split to the headline (like the Dispatch Source split at
  `report.rs:3238`) or report cache-load time as a column.

- [ ] **Cold+warm iterations are pooled into one sample** — medians, paired-t p-values,
  and CIs computed over a bimodal mixture in `CacheMode::Both`
  (`runner.rs:1386-1419`); the comment claims the renderer separates them
  (`runner.rs:1374-1376`) but none does. And the h3/raster ship gates *require* that
  mode (`report.rs:2068-2087, 2105-2111`), so "warm speedup ≥ threshold" was never
  evaluated on warm-only samples. Fix: compute separate warm/cold stats keyed off
  `IterationResult.cache_purge`; gate on the warm subset.

- [ ] **"Cold" isn't cold.** OS page cache is purged but 8 GB shared_buffers never
  evicted (`runner.rs:1106-1137`); the second-ordered mode inherits the first's warm
  cache and `prime_workload_accel_backend` re-reads whole tables after the purge
  (`runner.rs:1463-1479, 1503-1521`); cold mode also forces warmup=0 so backend init +
  Metal JIT lands in the timing (`runner.rs:1421-1425`). Fix: restart or verify
  fixture >> shared_buffers; purge per-measurement; one untimed init round.

- [ ] **Hand-tuned planner costs to win `add_path`.** Explicit `total_cost * 0.7`
  undercut (`planner_hooks/srf_target_list.rs:555`); magic `cost_per_row` constants
  0.00003–0.00009 across recognizers (`ssbm_q1.rs:197,292,389,493`,
  `resident_groupagg.rs:150`, `resident_star_groupagg.rs:107`,
  `resident_h3_groupagg.rs:100`). Fix: derive from the cost model / DeviceLimits.

- [ ] **Any user table named `ssbm_lineorder` gets planner-blackholed.** Recognizers
  claim the lane (short-circuiting all other injection) even when the resident cache is
  not loaded (`ssbm_q1.rs:142-176`, `resident_star_groupagg.rs:63-91`). Fix: claim only
  when the cache is loaded, keyed by relation OID registered at load time.

- [ ] **Plan snippets truncated to 30 lines before classification** — a Custom Scan
  deeper than 30 lines is misclassified as declined (and per the injection bug above,
  gets a synthetic reason) (`runner.rs:4077-4087, 4127`). Fix: scan full EXPLAIN output
  before truncating for display.

- [ ] **The only mechanism that makes acceleration fire is undocumented, bench-only
  surface.** Sole callers of `pg_accel_load_resident_*` are the bench harness and
  plan-shape tests (`pg_accel_bench/src/runner.rs:1643-1731`); absent from README docs.
  Fix: document the resident-cache SQL API (semantics, staleness contract, memory cost)
  or mark it superuser/bench-internal.

- [ ] **README Quick Start cannot work.** It shows `ST_Contains` "automatically
  accelerated" (README.md:69-82) while the same README says the PostGIS allowlist is
  empty and spatial is quarantined (README.md:214,229,249). The supported-operations
  matrix (README.md:180-189) lists nine strategies as working while the ship-gate table
  (README.md:150-162) shows sort/window/spatial/raster/hashjoin planner-declined and
  297/450 rows native. Fix: quick start uses a query that actually dispatches (with the
  resident-load prerequisite); matrix distinguishes "kernel exists" from "selectable".

- [ ] **CHANGELOG cites ~20 commits that don't exist** (history is one squashed commit)
  (`CHANGELOG.md:83-165`), and workspace claims `version = "1.0.0"` (`Cargo.toml:6`)
  with a failing ship gate and no released changelog entry. Fix: strip dead SHAs or
  preserve pre-squash history in a ref; version 0.x until `just release-checklist-audit`
  passes.

- [ ] **README contradicts itself on recheck semantics**: "uncertain rows are rechecked
  on the CPU using the original PostgreSQL function" (README.md:203-206) vs "Uncertain
  GPU classifications are rejected, not rechecked on CPU" (README.md:178,249). Pick one
  semantics, state it once.

## P1 — Crashes, UB, and safety-rule violations

- [ ] **No try/catch at `extern "C"` boundary in expr_eval** — `sycl::malloc_shared` /
  `parallel_for` / `wait()` can throw; escaping C++ exception through the C ABI =
  `std::terminate` = backend SIGABRT, leaking staged buffers
  (`pgaccel-kernels/src/expr_eval.cpp:1046-1135`). Fix: wrap in try/catch returning
  `PGACCEL_ERROR`; RAII the staging.

- [ ] **Misaligned `HeapTupleHeader` references — Rust UB.** Arena packs tuples at
  unaligned cumulative `t_len` offsets; `header()` casts `&arena[offset]` and
  `&*hdr` forms a misaligned reference (`executor/vectorized_scan.rs:172-177, 371-374`;
  `tuple_extract.rs:897`), plus unaligned i64/f64 loads. Fix: MAXALIGN entry offsets;
  8-aligned backing storage.

- [ ] **Fieldless `#[repr(i32)]` enum as FFI return type is UB on unknown values.**
  Every extern in `pg_accel/src/gpu/bridge.rs` returns `PgaccelStatus`
  (`gpu/types.rs:21-31`) — any out-of-range i32 from C is instant UB. Fix: declare
  externs `-> i32`, convert via fallible `from_raw`.

- [ ] **~800 `unsafe` blocks with zero `// SAFETY:` comments in the four resident/SSBM
  planner files** (grep counts 339/264/132/63 in `ssbm_q1.rs`, `resident_groupagg.rs`,
  `resident_star_groupagg.rs`, `resident_h3_groupagg.rs`; e.g. raw derefs at
  `ssbm_q1.rs:2037-2052`), plus ~365 more across engine (e.g.
  `executor/agg/execute.rs:549`, `executor/olap.rs:1989-2621`,
  `executor/vectorized_scan.rs:610-623`). Rule #5 wholesale violation. Fix: backfill;
  enable `clippy::undocumented_unsafe_blocks`.

- [ ] **Crate-wide `#![allow]` of ~40 lints** including `clippy::missing_safety_doc`,
  `cast_possible_truncation`, `cast_sign_loss`, `float_cmp`
  (`pg_accel/src/lib.rs:26-62`) — the broad-lint-disable pattern the repo's own
  anti-cheat rules ban; it's what lets the SAFETY-comment gap compile clean. Fix: move
  justifiable allows to item scope with reasons.

- [ ] **Kernel dispatch failures disguised as declines** — non-Ok `PgaccelStatus` mapped
  to `Deferred` for st_area/st_length/st_distance
  (`dispatch/spatial.rs:607-609, 731-733, 754-756, 858-860, 974-976`); uniform collapse
  to `all_uncertain(n)`/`None` with no logging or counter across
  `gpu/three_layer.rs:442-582`, `gpu/h3.rs:22-50`, `gpu/reduce.rs:14`; C++ catch blocks
  map everything to `PGACCEL_ERROR_NO_DEVICE` with no logging
  (`bbox_ops.cpp:143-146`, `fused_ops.cpp:400-404`, `h3_ops.cpp:566-567`,
  `window.cpp:657-760`). Rule #4. Fix: log concrete status at error level, bump a
  per-domain failure counter, return distinct status codes (decline ≠ OOM ≠ crash).

- [ ] **GPU kernel branch disabled via `usize::MAX` threshold** —
  `RESIDENT_DENSE_GROUPED_F64_PREDICATE_WIDE_MIN_ROWS: usize = usize::MAX`
  (`executor/olap.rs:17`, twin at `olap_cache.rs:2419`) makes the predicate-wide kernel
  (`olap.rs:1794-1818`) unreachable — the exact pattern banned by anti-cheat rule #9.
  Fix: fix the kernel and restore a real threshold, or delete the branch with a tracked issue.

- [ ] **Crash-workaround thresholds institutionalized in the cost model** —
  `gpu_hash_agg_unsafe_input_rows = 100_000`, spatial "unsafe band" 80k–150k ("many
  fixtures crash at the 100K scale"), hash-join build clamped at 99,999
  (`engine/cost/device_limits.rs:321, 389-391, 417-421`; enforced at
  `cost/formulas.rs:121, 523-525`). Fix: file issues, fix the kernels, surface decline
  reasons meanwhile.

- [ ] **Hidden CPU fallback in hash_agg**: terminal path `agg_hash` assigns groups via a
  host `std::unordered_map` loop (`pgaccel-kernels/src/hash_agg.cpp:2616-2678`,
  dispatched `:3448`); on Metal both GPU grouping paths are quarantined (`:1810`,
  `:3395-3398`), so "GpuHashAgg" groups on CPU. Also host O(n) compaction/RLE in
  `pgaccel_hash_count_i64_execute` (`:3472-3506`) vs the fail-closed contract in
  `pgaccel_hash_agg.h:91-93`. Rule #11. Fix: Metal-safe GPU grouping, or decline so the
  planner keeps the query native.

- [ ] **`catch_unwind` swallowing pgrx panics in planner-hook-adjacent code** — a pgrx
  panic wraps a PG ERROR whose backend state must be handled via `PgTryBuilder`
  (`FlushErrorState`); bare catch-and-continue can leave error/memory-context state
  inconsistent (`engine/registry.rs:265-268, 377-395`). Fix: `PgTryBuilder`.

- [ ] **Private-data list codec silently zero-fills out-of-bounds reads** — truncated or
  version-skewed `custom_private` deserializes into zeroed attnos/OIDs/counts
  (`custom_scan/private_data/list_codec.rs:16-19, 87-89`); node cells cast to Integer
  without checking node tag (`custom_scan/mod.rs:4025-4040`, `list_codec.rs:73-81`);
  one shared `EXEC_METHODS` for eight scan flavors means a bad strategy int silently
  selects the wrong executor arm (`custom_scan/mod.rs:334`). Fix: strict decode,
  tag checks, strategy check in `begin_custom_scan`.

- [ ] **Signal handlers reset to SIG_DFL process-wide during device enumeration** —
  SIGTERM/SIGQUIT in that window bypasses PG's die/quickdie
  (`pgaccel-kernels/src/device_manager.cpp:170-178, 251-256`). Fix: `sigprocmask`
  block instead, or narrow to the signals the driver needs.

- [ ] **Fork-safety depends entirely on Rust-side call discipline** — fork detection
  only in `pgaccel_init` (`device_manager.cpp:144-161`); every per-file `get_queue()`
  merely null-checks `g_queue` (`mem_pool.cpp:79-95`, `bbox_ops.cpp:22-30`, ~15 files).
  Fix: fold PID check into a shared `get_queue()`.

- [ ] **Missing `check_for_interrupts!` in long loops** — cache-load SPI over 100M+ rows
  (`olap_cache.rs:2606-2650, 3154, 3601, 3712, 3834`), per-row raster kernel loops
  (`dispatch/raster.rs:556-1379`), preagg dim build (`preagg/mod.rs:299-367`);
  sort_scan checks only once per up-to-1M-row batch (`executor/sort_scan.rs:137, 190`).
  Fix: standard 8192-row cadence.

- [ ] **NaN passes GUC clamps into planner cost math** —
  `clamp_soft_fp64_cost_multiplier` returns NaN unchanged (`pg_accel/src/lib.rs:120-135`);
  same hole in `cost_multiplier` (`engine/gucs.rs:151-160`). Fix:
  `if !raw.is_finite() { return default }`.

- [ ] **n² blowup in spatial three-layer dispatch** — row-wise op needs n diagonal pairs
  but launches full n×n kernel and allocates three n²×2 u32 host buffers uncapped
  (`gpu/three_layer.rs:624-737`; ~2.4 GB at n=10k); also one kernel launch per row on the
  non-constant polygon path (`:459-487`). Fix: diagonal/pairwise kernel entry; cap and
  decline like `spatial.rs:32`.

- [ ] **`num_rows` vs batch length unvalidated at expr FFI** — if caller passes
  `num_rows < batch.num_rows`, C writes past the results Vec
  (`gpu/expr.rs:226-243, 249-265`). Fix: derive length from `batch.num_rows`.

- [ ] **Unsound lifetime laundering in raster dispatch** — detoasted varlena borrow
  returned as `&'static [u8]` / `&'static str` (`dispatch/raster.rs:89-102, 147-150`).
  Fix: bind lifetime to a borrowed arg or take a closure.

- [ ] **Thread budget: unchecked i32 math can wrap negative; panics swallowed silently
  in `before_shmem_exit`; `MAX_BACKENDS = 256` below common `max_connections`; crashed
  backends never reclaimed** (`engine/thread_budget.rs:22, 113-115, 166-186, 262-283`).
  Fix: checked math, log caught panics, size from `MaxBackends`.

- [ ] **fp64 type trap at spatial FFI** — Rust externs hardcode `*const f32` but expose
  `use_fp64: bool`; passing `true` reads f64 through half-sized buffers
  (`gpu/bridge.rs:76-107` vs `pgaccel_ffi.h:338-357`). Fix: mirror `c_void` or split
  `_f32`/`_f64` externs.

- [ ] **Struct layout parity unenforced for most shared FFI structs** — only
  platform_caps/device_info have two-sided size pins (`pgaccel_ffi.h:812-822`);
  `pgaccel_val`, `pgaccel_expr_instruction`, `pgaccel_batch`, `pgaccel_agg_col`,
  `pgaccel_geometry`, resident-batch structs have Rust-side-only tests gated behind
  test cfgs (`bridge.rs:2042-2143`, `types.rs:507-525`); five
  `pgaccel_expr_template_*` symbols are declared in no C header at all
  (`expr_templates.cpp:2371-2594` vs `bridge.rs:880-970`); union punning for raster
  band-index assumes little-endian with no assert (`types.rs:445-450`,
  `raster_ops.cpp:114,272`). Fix: C-side `static_assert` (size + offsetof) for every
  shared struct; declare all exports in headers; static-assert endianness.

- [ ] **Key-buffer stride desync on unknown key types** — `append_key_bytes` falls
  through `_ => {}` (`executor/agg/keys.rs:227`), desyncing buffer stride from
  `key_size()` so the kernel reads shifted keys. Fix: `pgrx::error!` on unknown type.

- [ ] **`extract_f64` `_` arm reads any unrecognized type as f64 bits** (NUMERIC pointer
  datum → garbage) (`materialize/tuple_extract.rs:749-756`). Fix: restrict to FLOAT8OID.

- [ ] **`typlen as usize` before validating fixed width** — varlena `typlen = -1` wraps
  to `usize::MAX`, `Vec::with_capacity` aborts the backend
  (`executor/vectorized_scan.rs:596-597`). Fix: validate `typlen > 0` first.

- [ ] **`pg_accel.min_batch_size` GUC has `max_val == default` (65536)** — can only be
  lowered, never raised (`engine/gucs.rs:105-114`). Fix: raise max_val or document.

- [ ] **Contradictory `unsafe impl Send for GpuHashTable`** justified by "only accessed
  from the main backend thread" (`gpu/hash_join.rs:12-14`). Fix: remove or document the
  real cross-thread transfer.

## P2 — Architecture

- [ ] **Two parallel architectures under one roof.** The documented
  adapter→dispatch→executor pipeline admission-gates to nothing, while the live
  planner→olap-executor→gpu resident path bypasses dispatch entirely and imports
  executor concrete types directly into planner modules
  (`ssbm_q1.rs:13-25`, `resident_star_groupagg.rs:7-18`). Fix: define a neutral
  plan-spec type in a shared module; decide which architecture is real; update
  ARCHITECTURE.md (which never mentions olap_cache/residency/resident planners —
  its flagship section documents the quarantined spatial pipeline).

- [ ] **Split the god files.** `planner_hooks/mod.rs` (6,167 lines: hook install,
  1300-line agg injector at `:2337-3658`, 580-line window injector at `:777-1359`,
  490-line preagg injector, hashjoin recognizers, path-search utils, type policy —
  while `scan.rs`/`sort.rs`/`window.rs` are 13-15-line stubs);
  `executor/agg/execute.rs` (6,103); `olap_cache.rs` (4,829); `olap.rs` (3,005);
  `pg_accel_bench/src/report.rs` (7,194: stats + classification + gates + hand-rolled
  markdown via 117 `writeln!` + CSV/JSON + hardware detection);
  `pg_accel_bench/src/runner.rs` (5,354). Split by concern.

- [ ] **Hardcoded GPU dispatch thresholds everywhere, violating rule #10** (three
  reviewers independently): ~20 consts in `executor/olap.rs:14-25` duplicated at
  `olap_cache.rs:2417-2428` (must stay in lockstep or scratch capacity returns 0);
  `FUSED_FILTER_COUNT_*` (`executor/agg/execute.rs:40-41`);
  `HASH_JOIN_MATCHES_PER_OUTER = 4` (`executor/join/mod.rs:60`);
  `GPU_SORT_TOPK_MAX_LIMIT = 128` (`executor/sort/mod.rs:38`);
  `RESIDENT_STAR_GROUPAGG_MAX_DIM_KEYS = 100_000` (`resident_star_groupagg.rs:21`);
  h3 NULL-ratio gate (`dispatch/h3.rs:164`); kernel-side `SORT_AGG_MIN_ROWS = 100000`
  (`hash_agg.cpp:1243`), `GPU_WINDOW_THRESHOLD = 65536` (`window.cpp:40`),
  olap_ssbm floors (`olap_ssbm.cpp:24-48`), `MAX_PAIRS` (`gpu/spatial.rs:32`).
  Fix: consolidate into `DeviceLimits::from_profile`.

- [ ] **Cost model calibrated on one machine** — every per-row constant in
  `engine/cost/constants.rs:14-164` is a hardcoded M2 Max measurement applied to all
  hardware, unlike DeviceLimits' `cu_scale` derivation. Fix: scale by detected profile.

- [ ] **Large dead-code bodies behind `#[allow(dead_code)]`**: typed-descriptor/GpuError
  facade with zero production consumers (`gpu/descriptors.rs:1-802`,
  `gpu/error.rs:1-284`) while the validation it provides is missing at real call sites;
  ~1000 lines of `residency.rs` behind six allows; 270-line `next_gpu_spatial`
  (`executor/join/mod.rs:505-815`); `dispatch/predicate_chain.rs:1-133` (no callers,
  but the only thing `dispatch/tests.rs` covers); dead 196-line C++ arena —
  `pgaccel_alloc` has zero callers, so the `pool_reset` calls and their SAFETY comments
  in `gpu/h3.rs:18-21` are no-ops describing fiction (`mem_pool.cpp:112-158`);
  `adapters/mod.rs:8` allows the whole extractor tree. Fix: wire in or delete.

- [ ] **Copy-paste sprawl**: four near-identical recursive clause walkers (~500 lines,
  `rel_pathlist.rs:851-1385`) → one generic walker; four copies of the recognizer
  matcher toolkit (`ssbm_q1.rs:2037-2130`, `resident_star_groupagg.rs:22-52`,
  `resident_groupagg.rs:700-739`, `mod.rs:3905`); chunked GPU reducers duplicated per
  type lane with repeated Kahan fold (`executor/agg/execute.rs:1547-1750+`); join key
  extraction ×4 (`executor/join/probe.rs:65-252` vs `:624-752`); window pipeline
  duplicated (`executor/window/mod.rs:126-253` vs `:289-407`); three ~150-line SSBM
  emitters (`olap.rs:1051-1500`); INET canonicalization duplicated by hand with a
  "must match" comment (`executor/agg/keys.rs:201-224` vs `tuple_extract.rs:513-543`).

- [ ] **Nine versioned copies of `resident_dense_grouped_f64_usm` with up to ~45
  positional params** (`pgaccel_expr.h:481-622`) instead of one descriptor struct.
  Fix: collapse to a descriptor-driven entry point.

- [ ] **Eight sequential recognizers re-walk rtable/targetlist per grouped query**
  (`mod.rs:231-248`) — O(8×) planning overhead; plus uncached `pg_depend` scans per
  candidate node during clause walks (`rel_pathlist.rs:688-694`,
  `engine/ffi/syscache.rs:100-119`) and palloc'd name strings never freed
  (`syscache.rs:75-95`). Fix: shared shape-extraction pass; backend-local memo with
  syscache invalidation.

- [ ] **Bench workload metadata is stringly and scattered** — matrix profiles keyed on
  workload name strings re-listed in ≥6 unconnected tables
  (`workloads/mod.rs:1058-2566, 944, 997, 761`; `report.rs:898`;
  `runner.rs:1752-2163`) — adding a workload silently misses tables. Fix: single
  declarative registry per workload.

- [ ] **Wrong decline-reason key pollutes evidence** — star-dim cardinality decline
  increments `"hashjoin_build_side_too_large"`
  (`resident_star_groupagg.rs:84`). Fix: dedicated key.

- [ ] **`pg_accel_reset_stats()` doesn't reset the process-wide atomics** (7 SRF columns
  read cumulative values after reset) (`engine/stats.rs:49-78, 454-464`). Fix: reset or
  document.

- [ ] **Silent CPU accumulation for i64 ops with no kernel** — `dispatch_gpu_reduce_i64`
  `_` arm falls back to `drain_small_batch` while the plan reads GPU-accelerated
  (`executor/agg/execute.rs:1583-1585`). Fix: planner declines these ops.

- [ ] **Test gaps in the riskiest code**: `tuple_extract.rs` (1,099 lines of raw offset
  math, zero tests); `executor/window/{frame,functions}.rs`; spatial/h3/raster dispatch
  arms (`dispatch/tests.rs` covers only predicate_chain); kernel side: the 1,140-line
  expr bytecode VM has no test references in `pgaccel-kernels/test/`, nor do
  `pgaccel_fused_filter_multi_reduce_f32`, `pgaccel_topk_kv_*`, window lag/lead.
  Fix: layout tests (preceding-NULL, varlena-header), VM edge semantics
  (overflow/NULL/NaN), Deferred-vs-error cases.

- [ ] **`unwrap_or`/`expect`/silent-swallow stragglers**: SsbmQ4 filter build error
  swallowed by `if let Ok` then misreported as "cache missing"
  (`olap_cache.rs:3996-4010`); `.expect()` in non-test code (`olap_cache.rs:1604`);
  stats field `gpu_kernel_executions` never written (`stats.rs:33-34, 395-418`).

## P3 — Tooling, CI, docs, hygiene

- [ ] **CI and local dev pin different AdaptiveCpp SHAs** — `ci.yml:26` requires
  `7e79a6ca…`, `scripts/setup_acpp.sh:8` defaults `456ae691…`; docs disagree too
  (ARCHITECTURE.md:11-14 + CHANGELOG.md:5-9 vs README.md:92 + TODO.md:23). Fix:
  single-source the pin in one file (e.g. `.acpp-version`) read by CI, script, and docs.

- [ ] **The 1,942-line plan-shape suite + parallel-stress tests never run in CI** —
  gated behind an `integration_tests` feature no CI step, recipe, or script enables
  (`plan_shape_test.rs:18`, `parallel_stress_test.rs:18`, `ci.yml:153,327`). Fix: add a
  live-PG CI lane running `cargo test -p pg_accel_bench --features integration_tests`.

- [ ] **Default `just bench` passes `--skip-guc-verify`** — the flag main.rs:139-146
  says is dev-only bypasses the postmaster-GUC mismatch hard-fail on evidence runs
  (`Justfile:374-396`). Fix: drop from default recipe; add `bench-dev`.

- [ ] **Justfile portability/duplication**: BSD-only `stat -f%z` breaks log-rails on
  Linux (`Justfile:449`); the 8-line PG-major preamble copy-pasted into ~25 recipes;
  `gpu-test` hardcodes a 20-binary list so new CMake test targets silently never run
  (`Justfile:552-573`); `log-rails` issues persistent `ALTER SYSTEM SET
  log_min_messages='error'` as a side effect (`Justfile:460-465`); metal-cpp zip
  downloaded with no checksum (`Justfile:194-224`).

- [ ] **PG17 remnants**: `coverage_gate.sh:4` and `cuda_stress_gate.sh:5` default
  `pg=17` (unsupported, hard-fails), and `pg_version_audit.sh:8-27` doesn't scan them;
  CLAUDE.md documents `pg_accel.fp64_enabled` as a live kill switch while
  `lib.rs:97-102` implements it as a deprecated no-op; `cross-verification.md`
  references `~/.pgrx/data-17`.

- [ ] **Two drifting advisory allowlists** — Justfile cargo-audit ignores
  RUSTSEC-2021-0127 + RUSTSEC-2026-0097; deny.toml carries only the former
  (`Justfile:304-306` vs `deny.toml:33-35`). Fix: consolidate with reasons.

- [ ] **Panic on multibyte crash text** — `&c.error[..77]` byte-slices a String
  (`report.rs:3297-3298`). Fix: char-boundary truncation.

- [ ] **Repo hygiene**: 0-byte root `build.rs` never compiled; generated
  `test_fp64_transcendental_probe.cpp.ll` checked in (gitignore `*.ll`); 14k lines of
  generated benchmark reports tracked (`benchmarks/README.md`, `benchmarks/last_run.md`)
  while their evidence artifacts are gitignored (.gitignore:36-38); duplicate dep
  versions instead of `[workspace.dependencies]`; empty `[build-dependencies]`
  (`pg_accel/Cargo.toml:48`); `scripts/create_agent_dbs.sh` unreferenced with unquoted
  SQL interpolation; `SpatialSelectivityRepro` workload referenced nowhere
  (`workloads/spatial_selectivity_sweep.rs:171`); dead `run_all`/stats helpers behind
  lint allows (`runner.rs:2342`, `stats.rs:104-158`); `format_rows` implemented 3×
  (`report.rs:1046`, `runner.rs:4352`, `workloads/mod.rs:2478-2497`).

- [ ] **Docs drift**: ARCHITECTURE.md source map stale on most lines (gpu/ listed as 4
  files vs 21, `platform_caps.cpp` doesn't exist, olap_ssbm/archive_stats/ooo_overlap/
  nested_loop_ineq/bbox_ops_f64 omitted, "five sets of vtables" enumerates six —
  ARCHITECTURE.md:123-126, 453-506); TODO.md is 3,518 lines of prose violating its own
  "open work only" charter with ~40% already done; NOTES.md:48-80 teaches the banned
  fixed-constant pattern without noting DeviceLimits migration status; README CI badge
  points at dead `pg-accel/pg_accel` org (README.md:6).

- [ ] **Small consistency fixes**: magic OID literals (`16` for BOOLOID at
  `join_pathlist.rs:909`; `TEXTOID_RAW`/`SORT_INT4OID` re-declared at `ssbm_q1.rs:65-67`,
  `rel_pathlist.rs:2341-2344`; NUMERICOID at `execute.rs:34`; raw 700/701 + `0x2`
  TTS flag at `preagg/mod.rs:307, 352-355`); `static mut PREV_*_HOOK` globals →
  `SyncUnsafeCell`/pgrx hook facility (`mod.rs:68-70`); `uses_fp64` classifies by bare
  lowercase fn name with no schema qualification (`adapters/mod.rs:39-53`); h3 comments
  claim LE packing + "high nibble" check that's actually `cell != 0`
  (`dispatch/h3.rs:837-844, 155-159`); stale doc claims warning-on-chunk-failure while
  code raises ERROR (`execute.rs:1591-1593`); SSBM cache loaders use search_path-relative
  hardcoded names (`olap_cache.rs:3830-3856`); bridge.rs "full surface" comment omits six
  declared symbols (`bridge.rs:19-24` vs `pgaccel_ffi.h:80-192`); undocumented
  intentional queue leak on fork re-init (`device_manager.cpp:153-154`); outlier-robust
  stats could add Wilcoxon alongside paired-t (`stats.rs:271-286`).

---

## What reviewers verified as solid (no action)

- Custom Scan three-vtable handling is correct (`custom_scan/mod.rs:224-352`).
- Bench core methodology is rigorous where not undermined above: randomized
  accel/baseline order per iteration, persistent per-mode connections with
  `DISCARD ALL`, deterministic `setseed` data, VACUUM ANALYZE before timing,
  correctness diff via temp-table EXCEPT, dispatch-counter deltas, `MIN_ITERATIONS=10`
  (`runner.rs:1305-1308, 2250-2260, 2376`).
- The `max_parallel_workers_per_gather = 0` ban is honored throughout — both modes use
  DEFAULT.
- All shell scripts use `set -euo pipefail`.
- Median+mean, paired t, Bonferroni, Cohen's d, CI, and outlier flagging all present in
  `pg_accel_bench/src/stats.rs`.
