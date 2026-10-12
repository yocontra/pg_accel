# Typed workspace call facts

2026-10-10. `workspace_calls.py` now checks direct workspace-helper argument
evaluation and records returned-alias bindings from compiler-resolved nodes.
This is not a proof of the caller's complete effects and does not influence
the release audit yet.

## Actual-source results

All 59 direct helper calls in these five inspected functions pass the argument
check and have a recorded destination. Parameter positions below are zero-based.

| Source | Caller | Helper | Base parameter | Metadata parameter | Calls |
| --- | --- | --- | --- | --- | --- |
| grouped_agg.cpp | bind_params | arena_ptr | 2 | 1 | 32 |
| grouped_agg.cpp | bind_publish_params | arena_ptr | 2 | 1 | 1 |
| grouped_agg.cpp | launch_grouped_agg_device | arena_ptr | 3 | 4 | 1 |
| join_rows.cpp | execute | at | 3 | 1 | 11 |
| join_rows.cpp | execute_controlled | at | 4 | 2 | 14 |

Caller and callee definitions come from one Clang pass over the frozen expanded
translation unit. Calls resolve by declaration ID, including the selected
template specialization/overload. The helper's body is rechecked, not approved
by its name. A namespace filter with immediate retention of the requested roots
keeps the IDs consistent; this path has a bounded 128 MB dump limit rather than
the single-helper path's 32 MB limit.

The base argument must reference the designated pointer parameter directly.
Offsets admit builtin scalar values and reads through explicit metadata-record
parameters or ordinary local storage. Pointer-field dereferences, implicit
class conversions, callbacks, increments and assignments in arguments fail.
Local reads are recorded as dependencies: their initialization and value
provenance are **not** proved by the call check.

Clang's expression type erases a field's reference qualification. To avoid
mistaking an implicit row reference for a scalar layout field, an additional
syntax-only pass asserts the declared member type is not a reference, using
`decltype` on the unparenthesized member access. It consumes the same frozen
expanded source and compiler profile, with no header reread. Private, ambiguous
or unnameable owner types conservatively fail this check.

Alias facts include compiler declaration IDs, exact expanded-source ranges,
local/member destinations and conservative array-member paths (`[*]`). Each
join function has one conditional alias, explicitly marked as conditional:
these facts are not unconditional GPU-output provenance. Partial facts are
discarded if any checked call fails. Compiler IDs are meaningful only within
their source view, not stable cross-process identities or receipt identifiers.

## Reproduction

For example, from the repository root with the existing AdaptiveCpp headers:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/workspace_calls.py \
  --source pgaccel-kernels/src/grouped_agg.cpp \
  --caller launch_grouped_agg_device --helper arena_ptr \
  --filter '(anonymous namespace)::' \
  --base-parameter 3 --metadata-parameter 4 \
  --include pgaccel-kernels/include \
  --include .pgaccel/acpp/current/include \
  --include .pgaccel/acpp/current/include/AdaptiveCpp
```

The table specifies the other four invocations with the same include paths
and namespace filter. The five `*-calls.json` files are the final real-source
results, including the declared-field-type protection.

## Tests and boundary

All 24 workspace tests pass (14 call-site tests plus ten body-effect tests),
along with four indexed-escape tests, five release-wiring tests and 21
documentation-parity tests. Cases cover hostile overloads/specializations,
missing callee definitions, indirect helper escapes, class conversions,
reference fields/arrays, pointer-row reads, partial results, member bindings
and conditional aliases. Counterexamples deliberately demonstrate that valid
call facts do not approve preceding initializers or subsequent host writes.
Lint, formatting, safety contracts, coverage scope and diff checks pass.

The local recipe and no-GPU CI configuration run both workspace suites; no
hosted CI execution is claimed. The core analyzer implementation did not change,
so its complete 180-test production suite was not rerun at this checkpoint.
There was no native runtime code change or native/PostgreSQL/GPU test run.

Still required before integration: exact compiler/preprocessor/object binding,
complete selected-call inventory, caller root-reassignment and initializer
effects, branch-sensitive transitive alias propagation, escaping/unbound
results, offset/index bounds, alignment and lifetime. The five inspected
functions are not a claim about every caller or build configuration. The
canonical Clang profile here is not the AdaptiveCpp object-generation profile.
The seven release findings remain open; neither a device-work waiver nor new
SQL planner admission was introduced.

The source snapshot and SHA256SUMS seal working-tree evidence, not a release.
Git remains read-only; these changes are not committed or pushed. The full
TODO objective remains incomplete, so planning documents have not been deleted.
