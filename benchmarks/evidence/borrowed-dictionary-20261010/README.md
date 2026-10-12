# Borrowed resident text dictionaries during aggregate preparation

2026-10-10. Dense aggregate/star preparation no longer deep-clones every
resident text label into its temporary host-column snapshot. This reduces
temporary host memory and copy work; it is not a measured query-speedup claim.

## Ownership change

`HostColumn` and `HostColumns` now carry the resident input bundle's lifetime.
The text constructor receives already-copied code/NULL lanes but borrows
`&[String]` from that bundle, represented by `Cow::Borrowed`. The empty typed
text case borrows an empty slice. The resident store cannot be mutated through
the immutable borrow while preparation is using its labels. No device pointer,
new unsafe operation, ABI field or wire representation is introduced.

`dictionary_value` still copies the canonical key and final `GroupDatum::Text`
when needed. Final group domains, prepared join codes and aggregate artifacts
therefore own everything they retain. No resident decoder reference escapes
preparation, including when the source decoder is subsequently replaced or
destroyed. BPCHAR trailing-space canonicalization and first-visible spelling,
text/varchar byte identity, NULL-code placement and invalid-input checks are
unchanged. Tests retain an owned-label construction as the old-policy control.

The change does not share dictionaries across backends, reuse a stale decoder
across generations, bypass budget/currentness checks or broaden SQL admission.
It affects the existing preparation path on an artifact miss; a warm artifact
cache hit already avoids this work. Device-to-host value/NULL copies and the
per-label i32 encoding remap are unchanged, as is retained artifact accounting.

## Evidence

The standalone PG18.4 and PG19beta1 suites each pass 1,262 tests, up from 1,258.
Four new tests use the same borrowed-label constructor as production:

- Compare codes and complete final domains with the previous owned-label
  policy for TEXT, VARCHAR and BPCHAR, including empty strings, Unicode,
  trailing spaces, repeated rows and ignored invalid codes under NULLs.
  Clear/drop source strings after preparation and check owned output values.
- Borrow a 100K-label dictionary by pointer identity. Encode a sparse matched
  row from the complete resident-code sequence; unused labels do not become
  output groups. The retained remap has exactly one initialized entry.
- Retain rejection of negative/out-of-range codes and malformed NULL sidecars,
  plus empty-column bounds and source immutability.
- Correlate independently ordered BPCHAR fact/dimension dictionaries and check
  the prepared output after both borrowed source decoders go out of scope.

Each high-cardinality fixture label has 77 bytes. The previous temporary clone
copied 7,700,000 label bytes plus 100,000 String headers (2,400,000 bytes on
this 64-bit host): 10,100,000 bytes of requested payload in total. The borrowed
snapshot makes none of these label allocations. This is a structural payload
calculation, **not total RSS, an allocator-overhead measurement or a timing**.
The remap still allocates 400,000 bytes in that fixture, and owned final labels
still consume their usual memory. Savings vary with actual dictionary size.

Strict Clippy on both majors, the safety contracts (18 tests and three audits),
documentation parity (21 tests), formatting and the unchanged scope inventory
also pass; logs are retained here. Scope remains 229 Rust files, 18 C++ sources,
35 CTests, 100 SQL files, 462 assertions, 43 semantic families and a 90% floor.

These standalone checks do not run full SQL queries against a live database.
The session cannot connect to local PostgreSQL sockets or expose a SYCL GPU.
No installed extension was changed. End-to-end load/refresh/query timings,
live PG18/19 parity and GPU/CUDA release qualification were not refreshed.
The seven previously reported release-audit findings remain open.

## Reproduction and source

```sh
ulimit -n 4096
just test-standalone 18
just test-standalone 19
just lint 18
just lint 19
just safety-contract-audit
just doc-parity
python3 scripts/coverage_tools.py audit-scope --scope coverage/scope.json --repo-root .
```

The only runtime source changed from the preceding TPC-H corpus snapshot is
`pg_accel/src/engine/executor/agg/artifact.rs`. The complete dirty worktree
source is captured in `source-snapshot.tar.gz`; `SHA256SUMS` binds this evidence
bundle. Git metadata is read-only, so this change is not committed or pushed.
