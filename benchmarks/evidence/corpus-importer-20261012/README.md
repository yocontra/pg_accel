# Pinned NYC corpus importer — 2026-10-12

This bundle records the standalone importer implementation published in commit
`61d89164157ec349c599f0e71257072fe4b55214`. Its scope is exactly four files:
`scripts/nyc_taxi_corpus.py`, `scripts/tests/test_nyc_taxi_corpus.py`, and the
`manifest.json` and `jfk-airport.ewkt` assets in
`benchmarks/corpora/nyc-yellow-2015-01/`. It does not publish or qualify the
accumulated Rust/native engine changes or the complete NYC benchmark milestone.

## Integrity fixes

- COPY now hashes the exact binary chunks consumed by PostgreSQL and compares
  them with the reviewed source pin inside the transaction, before commit.
  Preverification remains in place. Replacement with equal-row-count data,
  including replacement followed by restoring the original file after COPY,
  raises an error and rolls back. Reopening the path afterward is not used as
  evidence of what COPY consumed. Binary reading preserves CRLF bytes.
- Download and projection publication use an atomic no-replace hard link,
  followed by removal of the producer's partial name. A concurrently created
  destination is preserved and the failed producer's partial remains available.
  Partial files are opened exclusively with `x`/`xb`; interleaved producers
  cannot open the same staging inode for writing. Tests verify the exact
  diagnostic partial pathname and unchanged first-producer bytes. This is a
  cooperative-producer guarantee, not protection against arbitrary filesystem
  writes by another process with the same user's permissions.
- Both CSV readers enable strict parsing. Unterminated final quoted fields
  and illegal characters after closing quotes are rejected instead of being
  silently normalized into a projected corpus.

## Verification

The five initial regression methods demonstrated eight failing subcases on the
original production implementation. `unit-before-fix.log` retains those failures.
After the fixes, 16 tests completed with one opt-in live-test skip. Two additional
interleaving tests brought the hermetic run to 18 tests: 17 passes and one skip.
Those intermediate results are retained separately, not relabeled as live passes.

The final run against a newly initialized disposable PostgreSQL 18.4 cluster
passes **all 18 tests with no skips**. Its four-row real import exercises valid,
zero, missing and invalid coordinates, generated PostGIS/H3 values, row counts,
the mutation-rejecting trigger and rollback. The COPY-replacement and concurrent
publication regressions remain hermetic tests; the live fixture does not itself
inject those races. Post-test SQL confirms that both the corpus table and its
manifest table are absent after rollback.

The server used the isolated copied PostgreSQL prefix from the hash qualification
work, with PostGIS 3.6.4, H3 `unreleased`, and pg_accel 1.0.0-rc1. Import explicitly
disables pg_accel execution. The owned cluster lived under
`/private/tmp/pgaccel-corpus-live-j96WVc/data`, used only its private Unix socket,
and was stopped afterward. Existing installations and servers were not changed.
Raw test, post-test identity/absence and shutdown logs are included.

A fresh read-only verification of the existing complete projected source passes:
**12,748,986 rows**, with the pinned SHA-256, exact CSV header and strict row-shape
checks. The existing compressed archive and JFK geometry also match their pins;
their hash verification is recorded separately. No source download or full-size
database import was performed for this checkpoint. No query-speedup, GPU parity,
full-corpus performance, PostgreSQL 19 importer, or release qualification follows
from these results.

The scoped source archive contains only the four files listed above, excluding
Git metadata, build outputs, database contents, the large source corpus, and
macOS AppleDouble metadata. `source-files.sha256` identifies their exact bytes;
`SHA256SUMS` seals the README, raw evidence and archive.
