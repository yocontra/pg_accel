# Raw integer COUNT: warm-resident qualification

This package qualifies the narrow raw-key COUNT admission and subsequent planner
overhead repairs on the recorded Apple M2 Max/Metal host. It is **not** a cold-query,
cross-device, full-suite, source-proof or release qualification.

## Qualified scope

One direct nullable INT2, INT4 or INT8 grouping key and one COUNT(*) or fixed-width
COUNT(column), optionally with a direct same-fact Boolean aggregate FILTER. The
nine counted SQL types are BOOLEAN, INT2/4/8, FLOAT4/8, DATE, TIMESTAMP and
TIMESTAMPTZ. Dense and raw candidates retain independent admission and currentness
checks. WHERE, general FILTER expressions, multiple measures and raw SUM/AVG are
outside this completed scope.

The changes include typed selected-path identity, adaptive raw COUNT admission,
exact dense-work costing, cancellation handling, per-hook neutral shape reuse,
and lazy catalog byte estimation when complete resident evidence already supplies
the exact answer. The native prerequisite is documented separately in
[native COUNT foundation](../native-count-foundation-20261030/README.md).

Lazy estimation preserves catalog/type validation, complete retained-owner byte
charging, invalidation and later revalidation. Cold/partial paths still estimate
once and capture metadata after SPI, with no store borrow across the callback.
The exact warm path no longer evaluates errors belonging solely to its discarded
estimate; this is not a claim of blanket error equivalence.

## Final performance gates

[Matrix results](raw/dense-planning-diagnostic-20261030/full-gates1/matrix.json)
contain all 22 cells at 1,000,017 and 2,000,017 rows: 12 raw COUNT cells, four H3
controls and six dense controls. Every cell retains five warmups and ten balanced
raw wall-clock pairs against parallel PostgreSQL at the recorded realistic
settings. No outliers were removed, no thresholds changed, and failed historical
runs were not overwritten. The final checker reports `pass` with no errors.

- Raw warm median speedups: **4.83–11.15x**, above the unchanged 1.15x floor;
  none of the 12 raw cells has a warm
  variance flag. Both COUNT(*) and Boolean COUNT with direct Boolean FILTER are
  measured for each key width.
- H3 warm control speedups: 10.59–13.40x. All four H3 cells and three dense cells
  retain variance flags. These controls do not add new H3 admission scope.
- **All 22 refresh-inclusive comparisons lose.** Raw lifecycle speedups are
  0.509–0.859x, H3 0.716–0.980x and dense 0.325–0.618x. The lifecycle `accel_ms`
  already includes refresh; `refresh_us` is a nested component, not another term
  to add. These are refresh-inclusive probes, not OS-cold measurements.

The separate fixed old/new/new/old comparison retained ten measured pairs per
block, producing 20 observations per version/cell. All six original strict
no-slowdown ratios pass. Ratios below are old median divided by new median;
they are not speedups against PostgreSQL.

| Dense key | 1,000,017 rows | 2,000,017 rows |
|---|---:|---:|
| INT2 | 1.02149 | 1.13412 |
| INT4 | 1.11908 | 1.08936 |
| INT8 | 1.03127 | 1.16123 |

[ABBA summary](raw/dense-planning-diagnostic-20261030/full-gates1/abba.json)
is descriptive, not an independent provenance verifier. Independent review also
checked all 12 run receipts, source/module/benchmark bindings, settings, balanced
sample order, complete parity, actual dense GPU dispatch, zero stock dispatch
and stopped owned clusters. Pooled old/new CVs are 8.64–14.54%.

The additional G4096 control uses the original fixture and five warmups/ten
measured executions at both scales. It passes full 4,097-group parity and actual
parallel-hash dispatch, with current EXPLAIN-inclusive diagnostic wall medians
2.9405/3.0925 ms.
Its older roughly 53 ms measurements are retained historical diagnostics, not a
contemporaneous speedup claim. See
[current G4096 receipts](raw/raw-integer-count-20261026/finalization-regression-lazy1/).

## Correctness and planner attribution

- PG18 and PG19 each pass the final 18 focused tests: source-inventory regression,
  six residency checks, mixed-column ownership and ten raw COUNT checks.
- Direct follow-up on each major proves the mixed-column test reached all five
  retained columns, pinned state and 2,614 raw bytes. An earlier apparently
  passing test had silently skipped: arbitrary selection from DISTINCT calibration
  sources hid a hardware entry. The same accepted-source EXISTS repair was applied
  to four test helpers; reference-only, unknown and empty inventories remain false.
- The final unfiltered ordinary library suite passes **1,359/1,359** tests; strict
  lint and formatting pass. This is not a final full live PostgreSQL suite rerun.
- Earlier both-major qualification covers all nine COUNT input types, filtered
  and unfiltered shapes, default selection, mutation/rollback, budget/currentness,
  capacity and cancellation contracts. The retained native prerequisite and build
  receipts identify their exact versions rather than implying every historical
  test used the final module.

The same-protocol planner diagnostic compares final3 against shared-input plus
lazy-estimation changes. Upper-hook medians fall from 288.5 to 149.5 us for INT2
and 288 to 170 us for INT8. This is combined planner-time attribution, not an
isolated lazy-estimation effect. Diagnostic end-to-end CVs were roughly 19–30%
with mixed wall results; the distinct final gates above establish the narrow
warm-resident qualification. Prepared-query guards require no planner activity.

## Source and binary identity

Authoritative Git-derived patches are in `source/`:

1. `raw-count-admission.patch`: code and benchmark delta from native-foundation
   commit `f6f4f8e502be51ec080e31e2c4fad00c012d59db` to
   `07f7c9bdab6ceaddff11d35bc45921cb140b1af5`.
2. `shared-input.patch`: commit `01665f28808f7e453f4756ec252bd0b204047c43`.
3. `gpu-source-test-helper.patch`: commit `a15630639f644edf325e1189973b96b3ae1d5f1b`.
4. `lazy-resident-estimate.patch`: final store change against that last commit.

Final store SHA256:
`1406ae1ebebeaeea9b5d5a39048b86979aa968a73642ed3d713bfa9fc3647c23`.
Final measured PG18 production module SHA256:
`dde4b9950928f3f9fd74588e3460d584a9625ca400ced179cb1f8c46b34c2b7e`.
PG19 final focused checks use the separately sealed test module; do not describe
that as the measured PG18 production binary. The unchanged benchmark binary was
retained, not rebuilt: its original binary seal and exact benchmark/workspace
source comparison are included. It has no dependency on the extension crate.

The reference-device dense cost coefficient remains the frozen fit's
`b/a = 0.06788047049882354`; no acceptance-driven refit was made. Training,
holdout errors, variability and original failures are retained. This calibration
is not uniform-accuracy or cross-device proof.

## Package layout, preservation and replay

`raw/` mirrors selected scratch-relative paths from the raw COUNT and planner
diagnostic tasks. It includes current and historical sample reports, parity and
plan artifacts, checker/harness sources, calibration data, build/source/module
seals, tests and failed attempts. The earlier two failed dense controls, failed
prepared-plan diagnostic and silent-skip proof remain evidence, not successes.
`copy-scope.md`, inventories and copy verification describe the exact copied set.
`SHA256SUMS` covers the finalized package except itself.

Original absolute paths and historical hashes are deliberately unchanged. The
package excludes PostgreSQL database directories, caches, installed prefixes,
compiled binaries, unrelated process listings and environment dumps. Hashes
identify excluded binaries; they do not supply those binaries. Replaying requires
the corresponding exact retained/rebuilt PostgreSQL, extension, native library,
benchmark and toolchain artifacts and the recorded host settings. Historical
checksum receipts and machine-specific scripts are not immediately relocatable
commands or grants of execution authority.

## Still open

Cold/first-use and refresh-inclusive performance, other devices, hosted CI,
upstream PostgreSQL regression/isolation coverage, broader aggregation and geo
expansion, and release qualification remain open. The source audit still reports
10 unproved exports across 134 ABI entries. No exemption, compiler closure,
source-effects proof or release approval is established by this runtime package.
