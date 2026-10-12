# Independent exact NUMERIC FILTER: 2026-09-29

Production planner admission now supports independent aggregate predicates
normalized to up to four inclusive INT4 ranges, alongside exact decimal
SUM/AVG and COUNT(value) sharing the same value/predicate state. For example:

```sql
SELECT g,
       count(*) AS n,
       sum(v) FILTER (WHERE h IN (1,3,5)) AS s,
       avg(w) FILTER (WHERE k=1) AS a,
       count(w) FILTER (WHERE k=1) AS c
FROM bench_numeric_rollup
GROUP BY g;
```

This is fused predicate evaluation, not a cached predicate mask. Each flagged
decimal state borrows a typed INT4 predicate column through the descriptor's
otherwise inactive RHS view. Predicates run before decimal accumulation; NULL
predicates reject the value, and excluded NaNs, infinities and overflowing
sums do not affect the result. All rows still establish groups and contribute
to unfiltered COUNT(*). Zero accepted values produce NULL SUM/AVG and zero
shared COUNT(value).

No descriptor layout, extra aggregate state, intermediate mask or extra
launch is required. Predicate columns participate in resident input borrowing,
span/lineage checks, fingerprints and chunk advancement. Planning charges
comparisons using the filter coefficient without assuming digit selectivity.
Singleton ranges are specialized on the host to one equality comparison;
non-singleton intervals retain both bound comparisons. The cost model counts
the matching worst-case comparison work without changing its coefficients.

## Scope and evidence

The initial three workload directories retain production-binary provenance, exact
native-result comparisons, paired raw timings, plans, numeric-mode counters,
resident loading and lifecycle evidence. The new conditional case is
`numeric_grouped_filtered`; `numeric_global_sum_avg` and
`numeric_grouped_three_keys` are unfiltered controls. Each runs at 250,000 and
1,000,000 rows, with five warmups and ten randomized balanced measured pairs.

The initial `numeric_grouped_filtered` run selected native parallel PostgreSQL
at both scales and receives **no GPU speedup credit**. Its GPU candidate cost
charged two comparisons even for singleton ranges. A separate diagnostic
removed native parallel competition to inspect that candidate; it is not a
matched benchmark and supplies no speedup claim. These initial reports are
retained unchanged. The final [equality-specialized report](equality-specialized/report.md)
uses ordinary production planning with eight native parallel workers enabled,
unchanged cost coefficients and no forced admission.

| Filtered rollup | pg_accel median | PostgreSQL median | Warm speedup | Separate load |
|---|---:|---:|---:|---:|
| 250,000 rows | 3.09 ms | 11.53 ms | 3.73x | 116.85 ms |
| 1,000,000 rows | 5.38 ms | 30.23 ms | 5.62x | 367.80 ms |

Both scales match exactly, verify `parallel_dense_numeric`, execute all
measured rows on the GPU, and record zero executor fallback. Ten balanced
paired measurements per scale pass the existing significance, effect-size
and warm-performance gates. Loading/refresh is not included in these warm
medians; the 250K refresh lifecycle alone took 88.52 ms end-to-end versus
11.98 ms for native PostgreSQL. First use is not a win.

Final-build unfiltered controls remain accelerated and exact:
[global SUM/AVG](equality-controls/numeric_global_sum_avg/report.md) records
2.41x/2.69x at 250K/1M, and
[three-key grouping](equality-controls/numeric_grouped_three_keys/report.md)
records 4.72x/6.01x. All four control cells verify the NUMERIC mode and zero
fallback. These are separate measurements, not a paired before/after estimate
of the equality optimization itself.

This is local warm-resident characterization on an Apple M2 Max with Metal,
64 GB memory, and PostgreSQL 18.4 on a shared development host. It is not a
cold-query, first-use, concurrency or release headline. CUDA qualification
remains owner-deferred. Loading is timed separately and is not free.

[Verification logs](verification/) retain 1,136 core tests, 571 benchmark tests,
eight live NUMERIC tests on each of PG18.4/PG19beta1, both Clippy builds, and
coverage/safety/documentation checks. Production SQL 117 checks 28 normally
selected global/one-/two-/three-key aggregate shapes on 1,000,017 rows,
including cross-chunk predicate inputs, rejected groups, shared filtered
states, hidden HAVING counts, ordering and LIMIT. The integer FILTER contracts
111 and 115 also pass on both majors. These targeted production SQL runs have
no warnings. The prior full SQL-suite characterization remains documented in
[the unfiltered milestone](../numeric-20260929/README.md); it is not silently
relabelled as a full-suite run for this change.

After the final equality specialization, all 1,136 core tests, all 32 native
CTests, both production Clippy builds, doc parity and coverage-scope audit pass
again. Production SQL 111/115/117 also pass warning-free on both majors. The
eight live tests per major and full coverage-tool audit preceded that final
specialization; the later native/production SQL tests exercise it. Final
installed module SHA-256 hashes are
`02f6c3aeb28712dc0be5419725da6d7109f156b6f1aefd7c93d3b8fb2341594e`
(PG18) and `66e15494919a695b4ef63b17eb70a9cb54bddf1eb612afaf7cdccc156f75cfb1`
(PG19). This remains a dirty-tree local implementation, not a published build.

Fresh decimal plans still need explicit pinning and current digit metadata;
DML invalidates that evidence, so the fresh-plan lifecycle test repins before
replanning. Count-only decimal states, mixed integer/decimal states, decimal
predicate inputs, NULL-accepting predicates, multi-column predicates and
query-level WHERE remain outside this expansion. Filtered COUNT(*) is not
newly admitted alongside decimal states; ordinary unfiltered COUNT(*) is.

Reproduce with the existing harness's `run --workload numeric_grouped_filtered
--cache-mode warm --timing raw --iterations 10 --warmup 5 --capture-plans`, a
matching production PG18 installation, and a fresh artifact directory. The
workload owns and recreates `bench_numeric_rollup`; use a development database
without unrelated data under that name.
