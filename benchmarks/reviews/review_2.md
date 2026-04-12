From: A Grumpy Committer <grumpy@postgresql.invalid>
To: pgsql-hackers@postgresql.org
Subject: [REVIEW] pg_accel benchmark — unfit for homepage claims

-hackers,

I was forwarded `benchmarks/README.md` from the pg_accel extension, with the
suggestion that the numbers are ready to be put on a project homepage. They
are not. I want to walk through, specifically, why this report would not
survive ten minutes on this list, and what the authors would have to change
before I would even bother to read the kernel code.

I will try to be precise rather than merely unpleasant. Every complaint below
has a line number from the report as it stands.

-----------------------------------------------------------------------------
1. The "PostgreSQL Settings" table is a Potemkin village.
-----------------------------------------------------------------------------

The report, at lines 13-28, prints this table:

    ## PostgreSQL Settings
    ...
    | `max_parallel_workers_per_gather` | `8` |
    | `max_parallel_workers` | `8` |
    ...
    | `work_mem` | `256MB` |
    | `shared_buffers` | `8GB` |
    | `effective_cache_size` | `48GB` |
    | `server_version` | `17.9 (Homebrew)` |

On the face of it, that looks like a reasonable workstation-sized config for
a 64 GB M2 Max (line 11: "| Memory | 64 GB |"). It is not what the benchmark
actually ran against.

Reading `pg_accel_bench/src/runner.rs` — the only thing in this project that
touches GUCs at setup — tells a different story. The `realistic()` profile
correctly enumerates these values, but the surrounding code splits them into
two buckets:

  - reloadable (`work_mem`, `effective_cache_size`,
    `max_parallel_workers_per_gather`, `parallel_*_cost`) — applied via
    `ALTER SYSTEM` / `SET` at session start.
  - NON-reloadable (`shared_buffers`, `max_worker_processes`) — these are
    PGC_POSTMASTER. The runner's own comment admits they will not take effect
    until "a full PG restart", and the runner only emits a stderr warning
    before proceeding anyway.

The report does not document, anywhere, whether that restart happened. There
is no "server start time" line, no `pg_postmaster_start_time()` capture, no
`SHOW shared_buffers` dump taken from inside the benchmarked session, no
banner saying "postmaster restarted at T=...". The only thing rendered into
`README.md` is the *intended* config, not the *observed* config. Based on
how the pgrx dev harness this project uses normally starts PG (128MB
shared_buffers, stock everything), the overwhelmingly likely reality is:

  - `shared_buffers` was 128 MB during the run, not 8 GB.
  - `max_worker_processes` was whatever pgrx started it at, which caps
    `max_parallel_workers = 8` to something smaller in practice.
  - Every other reloadable GUC did apply.

Publishing a "PostgreSQL Settings" table that does not match the running
postmaster is worse than publishing no table at all. It launders pgrx
defaults behind numbers that look production-realistic. That is exactly the
kind of thing that ends up screenshot in a slide deck next to a 3x bar
chart, and I would like it to stop.

Required fix, non-negotiable:

  a. Before the benchmark run, stop the postmaster, write the desired
     `shared_buffers = 8GB` / `max_worker_processes = 16` into
     `postgresql.conf` (or `postgresql.auto.conf`), start it back up, and
     only *then* connect.
  b. Inside the benchmark's own session, run `SHOW shared_buffers`,
     `SHOW max_worker_processes`, `SHOW max_parallel_workers`,
     `SHOW max_parallel_workers_per_gather`, `SHOW work_mem`,
     `SHOW effective_cache_size`, `SHOW jit`, `SHOW
     effective_io_concurrency`, `SHOW random_page_cost`, and
     `pg_postmaster_start_time()`, and render *those* values into the
     table. Not the values you *asked* for — the values PG tells you it
     has.
  c. If `SHOW shared_buffers` comes back as `128MB`, the script must
     refuse to continue and exit non-zero. No warning-and-proceed.

-----------------------------------------------------------------------------
2. The parallel-query config is still unrealistic even if 2(a) is fixed.
-----------------------------------------------------------------------------

Even taking the "Settings" table at face value, it is missing half the knobs
that matter for a parallel sort/agg comparison. None of the following are
set or shown:

    max_worker_processes          (postmaster-only — the real ceiling)
    parallel_leader_participation
    min_parallel_table_scan_size
    min_parallel_index_scan_size
    jit                           (on by default in 17 — huge effect on agg)
    jit_above_cost
    jit_inline_above_cost
    jit_optimize_above_cost
    random_page_cost
    effective_io_concurrency
    wal_level / synchronous_commit / fsync / full_page_writes
    checkpoint_timeout / max_wal_size
    track_io_timing

For a benchmark whose headline category is `gpu_sort` and `gpu_hashagg`, not
disclosing whether JIT was on or off is a show-stopper. PG17's JIT will
compile aggregate transition functions and save 20-40% on
`hashagg_10g`-style workloads; if it was off, the parallel baseline is
artificially slow, and if it was on, pg_accel is being compared against
something very different from pgrx defaults. Pick one, state it, and
document which.

Recommended replacement values, for the record:

    shared_buffers              = 16GB    (not 8GB — you have 64GB of RAM
                                            and the working set at 10M rows
                                            is deliberately larger than 8GB)
    effective_cache_size        = 48GB
    work_mem                    = 256MB
    maintenance_work_mem        = 2GB
    max_worker_processes        = 16
    max_parallel_workers        = 12
    max_parallel_workers_per_gather = 8
    parallel_leader_participation   = on
    jit                         = on     (and say so!)
    jit_above_cost              = 100000
    random_page_cost            = 1.1    (NVMe / unified memory)
    effective_io_concurrency    = 200
    checkpoint_timeout          = 30min
    max_wal_size                = 8GB
    wal_compression             = on
    track_io_timing             = on     (so EXPLAIN BUFFERS means something)
    synchronous_commit          = off    (for benchmark only, disclose it)

Anything less than this is "pg_accel vs pgrx dev harness", not "pg_accel vs
PostgreSQL". The difference matters.

-----------------------------------------------------------------------------
3. "Methodology" does not describe the methodology.
-----------------------------------------------------------------------------

Lines 30-43:

    ## Methodology
    | Parameter | Value |
    |-----------|-------|
    | Iterations | 10 |
    | Warmup iterations | 1 |
    | Row scales | 1K, 10K, 100K, 1M, 10M |
    | Measurement ordering | randomized per iteration (...) |
    | Statistical test | Paired t-test (two-tailed, p < 0.05) |
    ...
    **Ordering note:** Measurement order (accel-first vs baseline-first) is
    randomized per iteration to eliminate cache-warming bias. Each mode uses
    a fresh connection with `DISCARD ALL` on close.

This is a list of statistical knobs, not a methodology. I need to know, at
minimum:

  (i)   Is the measured time raw wall clock from the client (psql / pgx /
        whatever the harness uses), or is it parsed out of EXPLAIN ANALYZE?
        The report never says. These are not the same number. `EXPLAIN
        ANALYZE` imposes per-tuple instrumentation overhead that is charged
        unequally: a Custom Scan Provider's Next() path can report
        row-counts essentially for free, while a parallel Seq Scan + Gather
        + HashAggregate pays the cost on every tuple in every worker. On a
        10M row aggregate that is easily 15-25% of the measured parallel
        time. If pg_accel is being credited for that delta, `gpu_hashagg`
        and `gpu_reduce` numbers need to be thrown out.

        Required: report both raw wall-clock (client-side timer, no
        EXPLAIN) and EXPLAIN ANALYZE time side by side, for at least the
        agg/sort/reduce workloads. Document which column the geomean uses.

  (ii)  `DISCARD ALL` does not clear the OS page cache. It resets session
        state — temp tables, prepared statements, sequence state, GUCs — and
        that is it. On Linux you need `echo 3 > /proc/sys/vm/drop_caches`;
        on macOS you need `sync && purge`. Neither is called. The
        "Ordering note" at line 43 claims randomization "eliminates
        cache-warming bias". It does not eliminate it, it *averages* over
        it, which is not the same thing and will silently punish whichever
        path the harness happens to call first on the first iteration of a
        fresh scale.

        Required: one of the following.
          - a "cold" column (`purge` / `drop_caches` before every
            iteration) and a "warm" column (no purge), reported
            separately, or
          - explicit documentation that every reported number is
            steady-state warm-cache, obtained by discarding the first N
            iterations, with N chosen by looking at the variance
            convergence. Not 1 warmup. 1 warmup on a 10M-row workload on
            unified memory is not enough; the first iteration is still
            paying GPU shader compile cost and OS page fault cost.

  (iii) There is no `ANALYZE` line. Anywhere. Not in the per-workload
        preamble, not in the setup section, not in the methodology. If the
        PG planner was making costing decisions on stale
        `pg_class.reltuples` values from the bulk loader, every single
        `parallel_mean` in this report is suspect — the planner could have
        been choosing Seq Scan where an Index Scan or Parallel Bitmap Heap
        Scan would have won, or vice versa.

        Required: after load, before bench, run `VACUUM (ANALYZE, VERBOSE)`
        on every benchmark table and capture `relpages` / `reltuples` /
        `n_distinct` into the report. If you cannot be bothered to do
        that, you cannot claim your parallel baseline is optimal.

  (iv)  10 iterations with a paired t-test and Bonferroni correction is
        statistically defensible in principle, but the report does not
        show the raw distribution. Where are the per-iteration wall clocks?
        Where is the coefficient of variation per cell? A 0.98x result at
        1K rows with CV=40% is noise; a 0.98x result at 10M with CV=2% is
        a finding. I cannot tell which is which from this table.

-----------------------------------------------------------------------------
4. `min_batch_size = 65536` is a magic number.
-----------------------------------------------------------------------------

Line 19:

    | `pg_accel.min_batch_size` | `65536` |

This is the single knob that gates whether a query goes to the GPU at all.
Nowhere in the report is there a sweep over this value. Not at 8K, 16K,
32K, 65K, 128K, 256K, 1M. Not per-kernel (a reduce kernel and a
point-in-polygon kernel do not have remotely the same break-even point).
Not per-scale. It is simply asserted, as if 65536 fell out of a derivation
on a whiteboard.

Look at the `gpu_reduce` column in the results table:

    Line 74:  | gpu_reduce_sum           | 0.98x | 0.10x | 0.65x | 0.43x | 0.22x |
    Line 81:  | reduce_multi             | 0.98x | 0.13x | 0.70x | 0.46x | 0.24x |

`0.10x` at 10K rows is not "the GPU is too slow for small batches". It is
"the harness dispatched to the GPU below break-even, ate the kernel launch
and the host<->device copy, then lost 10x". The entire point of
`min_batch_size` is to prevent exactly that, and it did not prevent it.
Either the gate is not actually enforced on these workloads, or 65536 is
the wrong value on this hardware. Either answer kills the headline number.

Required: a `min_batch_size` sweep, per workload category, reported as a
separate appendix section, with the selected value justified from the
sweep. Until that appendix exists, any geomean that includes the sub-1x
reduce cells is not measuring "pg_accel on M2 Max" — it is measuring
"pg_accel on M2 Max with an untuned dispatch threshold", and that is not
what the homepage is going to claim.

And while we're here — line 45:

    **Crashes:** 4 scale(s) crashed and were excluded from results.

No. Excluding crashes from a performance comparison is not acceptable
practice on this list. If `reduce_sum_i64` crashes at 10K, 100K, 1M, and
10M (see lines 78, you can read that row yourself), then the correct
reporting is "reduce_sum_i64: CRASH", and the geomean row for `gpu_reduce`
must either omit the category or carry an asterisk saying "5/6 kernels
stable". A silent drop biases the geometric mean upward. This is the
oldest trick in the benchmarketing book and I will not pretend not to
notice it.

-----------------------------------------------------------------------------
5. What the numbers actually say, once you read past the geomean.
-----------------------------------------------------------------------------

Since the authors buried the lede, let me surface it. From lines 51-66:

    | Category     | Workloads | Geomean vs Parallel | Significant ... |
    | gpu_expr     | 60        | 0.77x               | 22 / 60 |
    | gpu_hashagg  | 35        | 0.66x               | 12 / 35 |
    | gpu_raster   | 20        | 0.76x               | 15 / 20 |
    | gpu_reduce   | 36        | 0.50x               | 23 / 36 |
    | gpu_spatial  | 100       | 0.76x               | 60 / 100 |
    | gpu_window   | 35        | 0.83x               | 20 / 35 |
    | vertex_sweep | 85        | 0.69x               | 48 / 85 |
    | **overall**  | **631**   | **0.89x**           | **252 / 631** |

The overall geomean is `0.89x`. That is a *regression*, not a speedup. Six
of thirteen categories are significantly slower than parallel PG on the
same hardware. Of the 631 measured cells, only 252 are statistically
distinguishable from the baseline *at all*, and many of those 252 are the
significant-slower ones in `gpu_reduce`, `gpu_spatial`, and `vertex_sweep`.

The two categories that do win — `gpu_h3` at 2.85x and `gpu_hashjoin` at
1.08x — are the ones where I would be most suspicious of methodology, not
least. An h3 benchmark where the parallel baseline is `h3-pg` going
through V8/SQL function call overhead per row is not a meaningful
comparison; it is a comparison of pg_accel against a known-slow CPU
implementation of h3 cell arithmetic. That speedup almost certainly
shrinks by 5-10x if the baseline uses batched C-level calls or even a
materialized expression index.

I want to be fair here: pg_accel may well have real wins on h3 and on
large hashjoins. I just cannot tell from this report, because the config
is wrong, the methodology is undocumented, the batch threshold is
untuned, and the baseline may have been handicapped by JIT-off /
stale-stats / pgrx-default postmaster.

-----------------------------------------------------------------------------
What would change my mind
-----------------------------------------------------------------------------

I would review a v2 of this benchmark. Specifically, v2 must:

  1. Actually restart the postmaster with the claimed `shared_buffers` /
     `max_worker_processes`, and render the *observed* values (`SHOW`)
     into the Settings table. Refuse to run if they do not match the
     requested values.
  2. Add the missing GUCs (JIT, random_page_cost, effective_io_concurrency,
     track_io_timing) to the table and to the run.
  3. Report raw wall-clock and EXPLAIN ANALYZE time side by side for the
     agg/sort/reduce categories, and clearly mark which number feeds the
     geomean.
  4. Add a cold-vs-warm breakdown, where cold means the OS page cache was
     actually purged (`purge` on macOS), not just `DISCARD ALL`.
  5. Document `VACUUM (ANALYZE)` ran after load, and capture `reltuples` /
     `relpages` in the report.
  6. Publish a `min_batch_size` sweep per category, and justify the
     chosen value from the data.
  7. Stop silently dropping crashed scales from the geomean.
  8. Label any geomean that includes sub-1x cells as "net regression",
     because that is what `0.89x` means.

Until then, this benchmark cannot go on the homepage, cannot be cited in a
blog post, and in my personal opinion should not be merged into the project
README in its current form. The code may well be good. The numbers, as
presented, are not evidence of it.

I am happy to re-review once the above is addressed. I am not happy to
argue about whether it needs to be addressed.

Regards,

  — A Grumpy Committer
    (still on -hackers, still tired)
    "shared_buffers = 128MB is not a benchmark, it is a page-cache test"
