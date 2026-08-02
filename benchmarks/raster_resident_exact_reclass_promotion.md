# Exact Resident Raster Reclass Promotion

## Candidate boundary

The only promotable raster surface is the existing RQS2 contract for
`ST_Reclass(raster,text,text)`. It requires one unqualified base relation, one
non-junk output target, no quals/order/group/limit/window/CTE/row marks, a
catalog-proved PostGIS Raster overload, constant arguments, canonical singular
`integer:integer` mappings, at most 64 unique sources, and an integer output
pixel type. NULL raster rows are preserved, a missing band is passed through,
source nodata is treated as an ordinary value, unmatched pixels become ordinary
zero, and the reconstructed output band has no nodata flag.

Missing-band passthrough is an executor capability, not part of the promotable
resident surface: the typed cost gate requires every non-NULL row to contain
the selected band and declines mixed relations with
`raster_selected_band_missing`. The benchmark candidate therefore contains
NULL, nodata, matched, and unmatched rows but no missing-band rows. The external
SQL contract verifies missing-band PostGIS parity on its separate native-decline
fixture.

The existing `raster_reclass` workload is not evidence for this surface. It
uses the five-argument range-rule overload and a floating output type. The
separate `raster_resident_exact_reclass` workload is registered as a GPU winner
with resident `rast` pinning and exact WKB correctness projection.

## Static audit

- Shape and catalog validation already fail closed before an RQS2 spec is
  encoded. Ranges, floats, whitespace, duplicate sources, oversized rule sets,
  noncanonical integers, wrappers, extra targets, and cardinality modifiers are
  rejected.
- Resident sizing proves every row action and exact WKB span before allocation,
  checks overflow for persistent and transient byte charges, rejects zero-grid
  present bands that PostGIS cannot round-trip, and validates copied device row
  actions before reconstruction.
- Native validation checks ABI versions, exact byte spans, alignment,
  disjointness, current-device allocation ownership, rule ordering/ranges,
  row/band metadata, offsets, capacity, launch bounds, and low-bit pixel values.
- Reconstruction preserves the original raster header and later-band suffix,
  replaces band one with the selected integer encoding, writes a zero nodata
  field without a nodata flag, handles source endianness, preserves NULL rows,
  and imports the result only through the catalog-revalidated PostGIS WKB
  function. Runtime failure is a hard error; there is no stock executor
  fallback.

No unguarded host pointer dereference or unchecked output span was found in the
audited RQS2 path. Existing unit, native, forced pg_test, cancellation/error,
rescan, cursor, memory-context, malformed-WKB, NULL, missing-band, nodata,
endianness, narrow-pixel, and chunk-boundary tests cover the principal crash
boundaries.

## Promotion result

The release run `raster-exact-reclass-qualified-dispatch-warm-20260802`
measured 10 iterations after 5 warmups with randomized paired ordering. The
10K, 100K, and 1M cells achieved median speedups of 8.56x, 3.37x, and 1.67x.
Every cell selected `GpuAccelRaster`, rebuilt through a real measured kernel,
consumed all output rows, produced an exact zero-diff WKB artifact, and kept
stock fallback at zero.

Production admission is restricted to the measured selected-pixel envelope,
10,134,528 through 63,340,224 pixels. Smaller, larger, structurally unsupported,
missing-band, or unproved shapes remain native.

## Retention gate

Temporarily enable only the exact candidate, install a release build, and run
paired warm measurements against extension-disabled PostgreSQL with exact WKB
parity, selected-plan proof, real kernel build evidence, consumed output, and
zero stock fallback. Retain normal planning only for independently measured
cells at or above 1.15x. Otherwise restore observation-only planning, register
the workload as an exact native decline, and preserve the losing artifact
without relabeling it.
