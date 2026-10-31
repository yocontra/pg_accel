# Independent paired diagnostic harness recommendation

Design recommendation only. No harness implementation or runtime authorization.
Preserve the unchanged failed query fixtures and all milestone gates. Explicit
diagnostic execution does not prove default planner selection or performance.

## Exact execution mechanism

Capture owned neutral recipes from each actual candidate after its ordinary
extraction/lowering: spec, projection, Legacy/RawIntegerCount selection and cost
evidence. Do not clone the raw spec into Legacy: independent aggregate FILTER is
lowered to checked bytecode for Legacy, while Raw retains its direct mask.
Retain no planner-owned pointers or residency borrows in the captured recipes.
The observer must not change path addition, pruning, costs or candidate order.

Use production `AggExecState::new_selected_descriptor_unprepared`, install its
abort-safe query-context owner, then `prepare_descriptor` and `next(slot)` through
EOF. Reuse the ownership/error-boundary pattern in custom_scan/mod.rs rather than
stack-only Rust RAII across PostgreSQL ERROR. Include cancellation/error cleanup
review and no RefCell/residency borrow across PostgreSQL service.

Do NOT use the existing H3 test's execute_projected/DescriptorAggPlan::execute_prepared
as the Raw arm: it is a staged one-shot grouped execution path, not the adaptive
v2 population path. AggExecState preserves actual adaptive retries, spool, final
materialization and lifecycle evidence.

## Correctness and identity

Use both exact diagnostic1 SQL/data fixtures, default coefficients and natural
capacities. Set no test capacity, qualification artifact policy or cost override.
Obtain complete GPU-disabled stock maps. Every execution must consume every row
and match exact keys/counts, including null keys and zero-count groups. Decode
INT2/INT4 signed keys using their declared output types, not generic INT8 casts.

Record/assert each arm's actual physical mode and completed lifecycle; no stock
fallback counter increase. For Raw, prove adaptive v2 routing and successful
whole-WHERE/leaf population conservation, not merely a parallel_hash label.
Keep the default-plan observations separate from the explicit-execution arms.

## Fixed measurements

Follow dense-where-cost-proposal.md: five warmups per arm, then ten balanced
A/B pairs, all samples retained with no best-of/retry or invented threshold.
Record preparation, dispatch and complete tuple consumption separately, plus
end-to-end duration. Keep sorting/parity comparison outside the timed interval.
Freeze harness and exact sample-order schedule before timing.

Warm Legacy can legitimately reuse its derived dictionary/mask; Raw reuses
residency but rebuilds query-local masks. Report that asymmetry explicitly.
Separately run ten paired fresh/invalidated-artifact executions including
construction. Prefer targeted invalidation of owned derived artifacts while
retaining identical resident inputs. If only generation mutation/reload is
available, label the result reload-inclusive; do not present it as isolated
artifact construction. No global cleanup of unrelated artifacts.

Warm JIT for both modes and every naturally exercised branch before measurements,
then invalidate only the intended artifacts for the fresh arm. Do not hide mask
construction outside a Raw timed interval or prebuild a supposedly fresh Legacy
artifact. Record cache hit/miss, construction bytes/time, resident generation,
natural capacities, actual calls/work facts, and whether loading is included.

The unchanged designated WHERE cells, released matrix, G4096 and strict controls
remain required. A diagnostic comparison cannot replace a failing selection gate.
