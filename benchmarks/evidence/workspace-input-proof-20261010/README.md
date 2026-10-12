# Workspace offset initializer and update effects

2026-10-10. Extends the staged call facts in `workspace-base-proof-20261010`.
**Not a complete caller/device proof, release approval or new SQL capability.**

## Implementation

An integer expression is not automatically safe just because its value is
stored in a local variable. The call verifier now follows offset-dependent
initializers transitively, rejects missing/self-dependent initialization, and
checks every use/update of each dependency across the caller. Scalar input
parameters also receive the update/escape check. Writes after a call and on
other branches are included, because a backedge can reach the call again.

Scalar initializer/update expressions admit the existing literal, scalar and
explicit metadata grammar. Row-pointer reads, callbacks, conversions with
unmodeled effects, address/reference escapes and unknown mutation forms fail
the complete result. Ordinary initialized induction variables and scalar
dependency cycles through subsequent safe updates remain supported.

Local metadata records must be same-type const-lvalue copies from a proved
metadata place. Source-bound compiler assertions require a trivial selected
const-reference constructor and trivial destructor; a CXXConstructExpr alone
does not prove absence of lifecycle effects. Record mutation/reference escape
remains unsupported, while chained copies and value captures are checked.
The existing declared-member reference checks still prevent a scalar-looking
MemberExpr from concealing an indirect row read.

Successful reports include the checked offset dependency declaration IDs.
Those IDs are scoped to the report's compiler view, not durable identities.
Failure discards all partial calls and dependency IDs.

## Current-source outcome

| Caller | Calls accepted | Outcome |
| --- | ---: | --- |
| grouped `bind_params` | 32 | Pass |
| grouped `bind_publish_params` | 1 | Pass |
| grouped `launch_grouped_agg_device` | 1 | Pass |
| join `execute` | 11 | Pass |
| join `execute_controlled` | 0 of 14 | Declined |

The controlled join captures its local `level` record by reference in a
callback. The stronger checker reports `level has an unproved LambdaExpr use`
and returns no facts. No callback-name exemption or runtime rewrite was added
to make this conservative check pass. Its closure/lifetime proof remains work
to do. `*-final.json` reports were regenerated with the final checker; the
controlled invocation exits 1, explicitly checked as the expected decline.

## Verification and limits

- **40 tests pass**: 36 workspace body/call tests plus four indexed-output
  escape tests. Additional cases cover initializer row reads/callbacks,
  uninitialized/self-referencing values, transitive assignments, scalar
  parameter mutation, reference/address escapes, record updates, custom copy
  constructors/destructors, induction variables and metadata-array copies.
- A positive value-capture case is paired with a failing reference-capture
  case. A control-predicate row-read witness still reports `caller not
  approved`: checking initializer/update expressions does not prove predicates
  or the full caller's host effects.
- The initial test log retains the old callback-initializer expectation
  failing under the stricter implementation. That test now correctly requires
  rejection; final focused results are in `tests-final.log`.
- Ruff formatting/lint, whitespace, safety contracts, documentation parity
  and scope checks are recorded. No scope baseline or threshold is reduced.

The core release analyzer does not import/consume these staged modules. Its
180-test suite passed in the preceding bundle and was not rerun for this
isolated checker extension. Native and Rust runtime sources are unchanged;
no native build, linked release-audit pass, GPU execution, live PostgreSQL or
performance qualification is claimed. Seven release execution findings remain
open. Control predicates, closures, transitive/conditional output aliases,
bounds, alignment, lifetime and exact build binding must still be proved before
release integration.

## Reproduction

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest \
  scripts/tests/test_workspace_calls.py \
  scripts/tests/test_workspace_effects.py \
  scripts/tests/test_output_container_escape.py -v
```

Actual-source command contracts and parameter positions are unchanged from
the previous call-fact bundles. The accumulated dirty-source snapshot and raw
results are sealed by SHA256SUMS. Git remains read-only; no commit or push was
possible. TODO and planning documents remain because the goal is unfinished.
