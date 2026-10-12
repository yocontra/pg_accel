# Stable workspace-parameter provenance

2026-10-10. Extends the staged compiler-resolved call facts from
`workspace-call-proof-20261010`. **This is not a caller/device-work approval.**
The seven linked release-audit execution findings remain unresolved.

## Closed prerequisite

The selected helper's first argument previously had to reference the caller's
designated workspace parameter by compiler declaration ID. That is necessary
but insufficient: assigning a new pointer to that parameter preserves its ID.
Taking its address or binding a reference can hide the assignment elsewhere.
`mutation-replay.json` records the same compiler view checked with the sealed
previous verifier and the current one: `workspace = nullptr` before the helper
call previously produced a fact, while the new verifier rejects it with zero
facts. Neither version's staged facts alone constitute a release approval.

`CallVerifier` now checks every use of that parameter throughout the full
caller, including nested lambdas. An evaluated use must be a compiler-resolved
LValueToRValue copy of the plain void pointer. Direct sizeof/alignof queries
are permitted as unevaluated uses. Reassignment, pointer-reference/address
escape and unmodeled uses reject the complete fact set. The check includes
writes after the selected call and in other branches, because a backedge can
make source-order-only reasoning unsound. Independent local pointer copies
and genuinely shadowing declarations are not mistaken for root mutations.

This is deliberately conservative, not branch-sensitive reaching-definition
analysis. The body may still read/write pointed-to memory, call unrelated
helpers, or end a workspace's lifetime; these remain caller obligations. Tests
explicitly retain a host-write witness whose call facts say `caller not approved`.
No release waiver, export exemption or new proof receipt consumer was added.

## Actual-source results

All 59 selected calls pass the stronger prerequisite, recompiled from current
native sources with the existing explicit metadata parameter contracts:

| Caller | Helper | Calls |
| --- | --- | ---: |
| grouped `bind_params` | `arena_ptr` | 32 |
| grouped `bind_publish_params` | `arena_ptr` | 1 |
| grouped `launch_grouped_agg_device` | `arena_ptr` | 1 |
| join `execute` | `at` | 11 |
| join `execute_controlled` | `at` | 14 |

The five JSON reports contain exact expanded-source hashes, compiler IDs,
bindings and unchanged outstanding local-read/conditional-alias facts. They
are not build-bound GPU-output provenance.

## Verification boundary

The workspace suites pass 28 tests, including four additional test methods
with compiler-valid reassignment, reference/pointer escape, callback, lambda,
branch, backedge, harmless copy and shadowing cases. Partial facts are discarded
on failure. Source-bound declaration/type checks and helper-body verification
remain active. The existing local recipe and no-GPU CI configuration already
discover these tests; no hosted execution is claimed.

The complete analyzer recipe passes all 258 tests: four output-container,
28 workspace, 180 core analyzer/production-witness, 20 metadata-effect,
15 metadata-build and 11 metadata-link tests. Safety contracts, documentation
parity (21 tests), whitespace and scope checks pass; logs are included. Native
source and ABI are unchanged; there is no fresh
native build, live PostgreSQL test, GPU result or linked release-audit approval.
Local offset initializer effects, transitive/conditional aliases, bounds,
alignment, lifetime and exact build binding still block release integration.

## Reproduction

```sh
just audit-cpu-cheats-test
PYTHONDONTWRITEBYTECODE=1 python3 scripts/workspace_calls.py \
  --source pgaccel-kernels/src/grouped_agg.cpp \
  --caller launch_grouped_agg_device --helper arena_ptr \
  --filter '(anonymous namespace)::' \
  --base-parameter 3 --metadata-parameter 4 \
  --include pgaccel-kernels/include \
  --include .pgaccel/acpp/current/include \
  --include .pgaccel/acpp/current/include/AdaptiveCpp
```

The other parameter positions are unchanged from the preceding call-fact
report. SHA256SUMS binds this report, raw results and accumulated dirty-source
snapshot. Git metadata is read-only, so nothing was committed or pushed.
The full TODO goal remains incomplete; planning documents are retained.
