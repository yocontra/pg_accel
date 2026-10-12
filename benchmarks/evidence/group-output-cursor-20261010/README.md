# Allocation-free group traversal and key decoding — 2026-10-10

Aggregate output no longer builds a host `Vec<usize>` of emitted group indices.
Compact output uses a contiguous cursor; dense output walks the completed,
owned activity bitmap after validating its length, Boolean values and emitted
count. Dense mixed-radix key decoding now writes ABI-bounded stack storage
instead of allocating a vector for every tuple. Output order and group/NULL
semantics are unchanged. Cursor advancement occurs after field conversion.

## Verification

- PG18 and PG19 standalone suites: 1,296 passed each.
- Production and pg_test strict Clippy: passed on both versions. The latter
  compiles live fixtures; it does not execute PostgreSQL or GPU tests.
- Documentation parity: 21 tests passed. Safety-contract and scope audits,
  formatting and whitespace checks passed. Scope remains 229 Rust files,
  18 C++ sources, 36 CTests, 100 SQL files, 462 assertions and 43 semantic
  families, with the unchanged 90% coverage threshold.
- Allocation-counter tests compare the old index-vector pattern and new cursor
  for compact sizes 0, 1, 65,537 and 1,000,017, plus a 65,537-slot sparse dense
  bitmap. Successful construction/traversal allocates zero times. The old
  nonempty compact vector allocates once and carries 8,000,136 payload bytes at
  the largest case on this 64-bit host; the new cursor has constant-size state.
- Dense decoding of all 105 groups in a 3×5×7 domain uses zero allocations,
  compared with 105 for the old vector-backed wrapper. Exact digits and checksum
  match. PostgreSQL Datum construction is outside this allocation claim.
- Every bitmap of lengths 0 through 12 matches the reference index sequence.
  Repeated peek/EOF, invalid bitmap values/length/count, truncated bitmap,
  backward/overflow/EOF advances and decoder bounds are covered.

No native source, ABI, SQL admission or installed extension module changes are
part of this increment. GPU execution is unavailable on this host, and live
tuple materialization has not been rerun. There is no new query timing or
end-to-end speedup claim. Earlier GPU failures and native audit findings remain
open; the broader TODO objective is incomplete.

Logs and the source snapshot accompany this file; SHA256SUMS seals the bundle.
The source snapshot excludes Git metadata, build output and evidence bundles.
The branch is main, but read-only `.git` prevents committing or pushing.
