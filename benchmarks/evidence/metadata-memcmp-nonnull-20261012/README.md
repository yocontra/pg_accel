# Metadata builtin memcmp annotation compatibility

This fixes the `NonNullAttr` rejection in main 644680e's Linux PG18/PG19 CI
run **38320170652** and release-plz run **38320170633**. Linux libc spells that
attribute on `memcmp`; accepting only compiler-implicit annotations would not
fix it. Actual Linux CI validation of this patch is pending.

The verifier accepts the annotation only on the exact external compiler builtin
C `memcmp` contract: matching return/operand/compiler-size_t types, no variadics,
no implementation, and full earlier/later redeclaration validation. Declaration
children are restricted to parameters and approved attributes. Function-try
bodies, aliases/renaming, unknown attributes, defaults and user implementations
are rejected. Existing exact-object, pointer-origin and `sizeof` checks remain.
General callable annotation policy is unchanged.

## Regressions and scope

- `memcmp-before.log`: all three explicit/macro/inherited positive annotation
  cases fail on the old verifier. These are real local Clang compilations with
  glibc-shaped declarations, not live Linux header/runtime qualification.
- `memcmp-negative-before.log`: ten assertions fail on the old verifier; only
  five represent prior acceptances (three later attributes and two bounded user
  implementations). The other five merely demand the new stricter diagnostic.
- `memcmp-try-body-before.log`: independent review's compiler-valid function-try
  implementations read row storage and were wrongly accepted before and after
  the caller. Both placements now reject via the strict declaration-child rule.
- `memcmp-*-final.log`: isolated candidate effects **26/26**, build **15/15**,
  link **11/11** pass after independent review fixes.
- `integrated-*.log`: the same three suites and Ruff are rerun on the actual
  integrated source. `compiler.log` records the local compiler.
- `ci-*-failure.log`: compact verbatim excerpts with original line numbers
  from the three failing hosted jobs, retained without unrelated build output.

The two files in `source-snapshot.tar.gz` are byte-identical to the independently
reviewed candidate. `SOURCE_SHA256SUMS` binds those sources; `SHA256SUMS` seals the
bundle. Both production metadata exports are included in effect/build testing.
Build/link tests exercise receipt replay, tamper rejection and object ownership.

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts.tests.test_metadata_effects -v
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts.tests.test_metadata_build -v
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts.tests.test_metadata_link -v
ruff check scripts/metadata_effects.py scripts/tests/test_metadata_effects.py
```

No SQL admission, GPU execution, performance limit or release gate is changed.
The seven production grouped/join device/address proof gaps remain open; no
postfix full production audit was run or claimed here. This is host/compiler
compatibility and proof-boundary evidence, not release or live Linux approval.
