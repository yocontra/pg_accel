# Workspace control-qualified address facts

2026-10-12. Standalone proof tooling, not a production/device/release approval.

`CallVerifier.prove_controls()` produces a separate atomic result. It checks
explicit host controls, transitive scalar inputs, syntactic metadata-use
discipline and consumed metadata declaration IDs. Row-dependent predicates,
unsupported flow, metadata mutation/escape and closure-crossing dependencies
decline without partial facts. Existing address-only `prove()` behavior remains
unchanged. Compiler-valid counterexamples retain the distinction between these
facts and ordinary host effects or later host output writes.

## Verification and independent publication scope

- **52/52 tests pass** in a separate export of committed baseline `5f4b165`,
  containing only the five files below and the proposed test-discovery hunks.
  Later committed release-profile changes do not affect these Python fixtures.
  No pending engine/analyzer implementation is needed by this package.
- **56/56 focused tests pass** in the accumulated working tree: the same 52
  plus four adjacent indexed-output-escape regressions. Those four depend on
  separate analyzer work and are not part of this standalone publication.
- The extracted Justfile parses. CI YAML passes actionlint with its existing
  `.github/actionlint.yaml` explicitly selected. The initial invocation without
  that config reported the existing custom `cuda` runner label; its log is
  retained, not counted as a pass.
- Both independent implementation reviewers approved the final metadata-ID
  correction. Ruff formatting/lint passed before integration; reviewed and
  integrated source bytes match.

Dependency-complete source scope:

```
scripts/workspace_effects.py
scripts/workspace_calls.py
scripts/tests/test_workspace_effects.py
scripts/tests/test_workspace_calls.py
scripts/tests/test_workspace_controls.py
```

These files use Python's standard library and real Clang C++17 fixture parsing.
Local validation used Apple Clang 21.0.0 (`clang-2100.3.34.2`).
`standalone-wiring.patch` contains only the Justfile/CI discovery additions and
the narrow TODO section, against the supplied baseline. Its CI context is
compatible with the subsequent release-profile test addition. It intentionally
excludes cumulative engine, cache-path and other TODO work.

From a checkout containing this five-file package:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s scripts/tests -p 'test_workspace*.py' -v
just --list
actionlint -config-file .github/actionlint.yaml .github/workflows/ci.yml
shasum -a 256 -c benchmarks/evidence/workspace-control-proof-20261012/SOURCE_SHA256SUMS
shasum -a 256 -c benchmarks/evidence/workspace-control-proof-20261012/SHA256SUMS
```

## Accumulated-source inspection, not a release gate

`production-inventory.json` records a contextual inspection of the uncommitted
native expansion, including expanded-source identities and explicit metadata
parameter contracts. The old address facts cover **65** calls: grouped binding
38, publish binding 1, grouped launch 1, ordinary join 11 and controlled join 14.
All five stronger qualifications decline, including when descriptor/request
metadata roots are explicitly added:

| Caller | First remaining obligation |
| --- | --- |
| `bind_params` | `dense_integer_value_count(desc)` predicate |
| `bind_publish_params` | Metadata reference passed to `materialized_key_type` |
| `launch_grouped_agg_device` | `validate_program` predicate |
| `execute` | Lambda capture initialization |
| `execute_controlled` | `run.poll()` predicate |

This inspection preceded the final dependency-ID reporting correction; it is
retained as diagnostic context, not final-checker production qualification.
All its stronger results fail and therefore contain no qualified dependencies.
Its native inputs are outside the independently publishable tooling package.
Unsupported proofs are not evidence of CPU row processing.

Ordinary host effects, external alias contracts, bounds, lifetime, conditional
alias propagation, dispatch/output composition and exact compiler/object binding
remain separate obligations. No native source, SQL admission or release-analyzer
consumer changes are part of this milestone. The seven grouped/join execution
findings remain open. Hosted CI, GPU runtime and performance were not qualified
by these tests. No large source archive is duplicated in this bundle.
