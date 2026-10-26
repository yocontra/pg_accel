# Adaptive hash watchdog diagnostic

The `repair-cap18-repaired` and fresh-cache `repair-cap18-freshjit` runs both
failed at capacity 7,872, seventh ALL attempt, with Metal Impacting Interactivity.
They are failures, not qualification. Fresh JIT excludes reuse of the prior
task cache as the explanation. No installed compiler/cache was deleted.

The fresh failing MSL is
`repair-cap18-freshjit/jit-cache/apps/global/jit-cache/12468944802878307522.3710491702715165948.jit`
(SHA256 `d3ff65a3f9737040388d777d75dcc4094caad6d483b6ceac279fba0d580d56bb`).
Its capacity-flag read is inside the probe loop at line 1330, but the successful
claim's ordinal increment/flag publication is after that loop at lines
1446–1453. This creates a SIMT reconvergence risk: successful lanes may wait
behind probing lanes before publishing the flag those lanes need. The CFG is
observed; attributing every watchdog cycle to that mechanism is an inference.

The scoped experiment uses a noinline helper containing both CAS and commit,
returning the observed owner. It runs on occupied attempts too, so the first
compiler pass cannot sink just the commit onto the successful exit edge.
No convergent attribute, capacity change, row-limit change, or generic retry
was introduced. Legacy API claiming remains inline and unchanged.

Fresh native-test MSL:
`cache-native-helper/apps/global/jit-cache/7346182221842539176.3710491702715165948.jit`
(SHA256 `ff983d50d8aa200e8b69d33ea87f2239c2d33bc7974264bd42e2e0a7bd5e9f4c`).
The final S2 compiler inlined the helper, but preserved the intended placement:
outer probe loop starts at 1168; v1 CAS at 1241 and ordinal/flag publication at
1286/1298 are inside it. Legacy commit remains outside at 1691. Thus the claim
is about actual generated placement, not an assumption that noinline survives
every pass.

`native-repair-build6.log`: terminal 0.
`native-repair-hash5.log`: terminal 0, 10,309,648 passed, 0 failed.
`native-repair-h3-3.log`: terminal 0, 1,176 passed, 0 failed.

The same real-cap fixture subsequently PASSED in `repair-cap18-helper`:
terminal 0, one live test passed in 108.84 seconds, all source seals verified.
The assertions require actual device-cap crossing, normal selected/dispatched
ParallelHash, multiple completed leaves, actual tuplestore spill, exact output
cardinality and full bidirectional native EXCEPT ALL parity. This is PG18
pg_test-module evidence, not production or PG19 qualification. Successful pgrx
logs omit notices and EXPLAIN values; cardinality/device numbers are asserted
from the backend's device limits, not dumped numeric receipts in that log.

Installed pg_test module SHA256:
`a64ab60b9dc609851c118942396c9472d8310ffdf8f36d695784ef8686b2dcb3`.
Actual extension-executed MSL:
`repair-cap18-helper/jit-cache/apps/global/jit-cache/14690400167917884164.11088897622669638989.jit`
(SHA256 `11f5eb8ad109471f858f13be81f25fc9a06ad039f20437dcf71fff1bc26debda`).
Its outer probe loop is at 1167 and v1 ordinal/flag publication at 1286/1297
is inside the loop, independently reviewed. Legacy publication is at 1691.
The owned PGDATA directory is empty after pgrx cleanup; no separate status3
receipt exists because pgrx removed its child cluster before the wrapper trap.
Disk free was 52GiB before/after; reported free memory was 69%/65%.

Final canonical performance still requires qualification. Matching-key helper
overhead has not yet been benchmarked.
