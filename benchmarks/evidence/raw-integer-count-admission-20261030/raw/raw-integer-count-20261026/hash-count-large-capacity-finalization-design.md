# Native COUNT large-capacity finalization repair proposal

No implementation yet. This is a source-backed performance hypothesis; the
observed ~52ms G4097 dispatch has not been isolated with per-kernel GPU timing.
The raw path performs one native call, no retry, zero resident load/artifact
construction. The discontinuity matches the source's serial heapsort branch,
but actual speed improvement must be measured, not inferred.

## Narrow scope

Modify only `pgaccel-kernels/src/grouped_agg.cpp` and its existing
`test/test_grouped_agg_hash.cpp`. No header/export/layout/threshold/cost changes.
Restrict new behavior to validated partition-v1 COUNT calls (`partition != nullptr`)
in the already-selected parallel-hash branch with
`desc.group_capacity > kHashSerialFinalizeGroups`. Validation already restricts
this API to one COUNTstar/directCOUNT, raw or H3 transformed keys.
Legacy nonpartition calls including other aggregate families remain byte-for-byte
behaviorally unchanged; truly small capacity retains the existing serial kernel.

Current spans: grouped_agg.cpp5518–5562 chooses branch on declared capacity;
5526–5531 nevertheless calls `run_hash_compact_kernel` if actual emitted<=16384.
That routine2982–2995 serially compacts and calls heapsort2898–2923. The already
scheduled parallel compaction/merge/finalization kernels then immediately return.

For validated v1 COUNT large-capacity calls:

1. CompactPrepare always calls `run_hash_compact_prepare`, not serial heap sort.
2. Parallel CompactRows does not skip merely because emitted<=serial threshold;
   it still rejects emitted>capacity and respects semantic failure/nonpublication.
3. Merge kernels retain `width >= emitted` and overflow guards, but do not skip
   small emitted populations solely because the serial threshold was crossed.
4. SortFinish likewise handles every valid emitted population, including0/1,
   using the existing actual-emitted ping-pong parity and packed-NULL finish.
5. Preserve the legacy low-emitted early returns/fallback under !partition-v1.

No new host cardinality readback, queue wait, allocation, submission, sorting,
aggregation, or partition/cancellation behavior. Same bounded grid and
capacity-derived merge launch sequence. Dynamic no-op tail rounds remain,
but already scheduled work no longer accompanies a serial O(GlogG) bottleneck.

## Correctness obligations

- Zero emitted: no compaction/merge/finish reads; completion publishes zero.
- One emitted: compact packed count/key; no merge round; finish decodes NULL tag.
- For any emitted<=capacity, active merge rounds are a prefix of capacity rounds.
  Existing `from_staged` toggling matches that prefix, and finish recomputes the
  parity from actual emitted, so no inactive tail-buffer publication.
- NULL remains separately tagged in count high bit, which cannot collide with
  u32-domain row count; unsigned key order and NULL order match existing output.
- Invalid key/filter/null errors and capacity overflow still suppress all
  publication and preserve typed error priority. No partial success on a failed
  attempt. Adaptive retry/counters/spool retain existing ownership and semantics.
- Capacity<=16384 v1 and all legacy paths are unchanged, preserving canonical
  low-resolution H3 fast paths and unrelated SUM/AVG behavior.

## Tests before runtime claims

Native high-capacity fixtures with actual emitted populations
0,1,2,3,4,5,65,257,4097,16383,16384,16385 and nonpoweroftwo32769; includeNULL as an
actual distinct group and explicitly test emitted vs nonnull count accounting.
Declared capacity16384/16385 and100000 exercise both branch edges; below actual
population must return typed overflow without output. Use existing independent
host test oracle (tests only), full sorted key/count/NULL comparison.
Explicitly cover odd/even active merge-round parity, packed NULL word tails,
zero-contribution FILTER groups, and pooled scratch reuse after success/failure.

Cross raw INT32/INT64 signed extrema/high-bit keys, COUNTstar/all nine value types,
nullable values, BOOL SQL FILTER TRUE/FALSE/NULL, key=count and count=predicate
aliases. Existing v1 descriptors include H3 canonical/noncanonical keys; test
large-capacity H3 low actual populations plus semantic resolution errors with
false/null masks, excluded partition, and late errors outranking overflow.
Use bounded cross-products/pairwise edges, not a giant full Cartesian suite.
Legacy comparison tests demonstrate unchanged results/failure semantics.

Run complete native hash/H3 and metadata contract suites. Independent source
review before production PG rebuild. Both-major normal raw/H3 parity, partition,
spill/error/cancel/rescan qualification remains required. Re-run original
G4096 known-regression cells at default settings with exact full parity and
actual native dispatch before claiming cause repaired; retain before/after
identities and every sample. No price changes during this isolated kernel check.

After this repair is measured, revisit the frozen dense cost candidate; do not
assume it alone fixes ordering. Final acceptance still includes original12raw,
six dense no-slowdown comparisons, H3 controls, and explicit G4096 regression
coverage, unchanged settings/thresholds/no forced modes. No residual-driven refit.
