# Staged native INT4 hash SUM/COUNT — 2026-10-10

This is implementation evidence, not GPU runtime or performance qualification.
The TODO remains open; SQL admission and Rust SUM binding/materialization have
not been expanded.

## Implementation

The native hash route accepts one INT64 FACT key (including fused H3 parents)
and one INT32 column with exact SUM|COUNT, optionally with the canonical SQL
measure FILTER mask. Signed 64-bit sums are accumulated using two 32-bit atomic
words. The UINT32_MAX row limit bounds every partial sum of INT32 inputs within
signed 64-bit range. NULL or rejected values do not contribute, but their keys
still create groups. COUNT supplies the internal nonnull validity source.
Compaction and both serial and parallel sorting carry sums with their keys.
COUNT-only dispatch remains specialized. Public ABI exports are unchanged;
callers must query the current private workspace layout.

Tests cover metadata without readable payloads, NULL/filter semantics, signed
extremes, H3 parents, the launch and parallel-sort thresholds, output aliases,
capacity failure, malformed input, workspace reuse, and empty input. Runtime
fixtures compiled but could not execute on this host.

## Results

| Check | Result |
| --- | --- |
| Native shared library and both grouped test targets | Built successfully |
| Protected-input hash metadata contracts | 2,069 passed, 0 failed |
| Standalone Rust tests, PG18 and PG19 configurations | 1,282 passed each |
| Strict lint, PG18 and PG19 configurations | Passed |
| Documentation parity, safety contracts, coverage scope | Passed |
| Native grouped/hash GPU tests | Failed: 0/2; GPU initialization unavailable |
| Full native device-work audit | Failed: 124 passes, 7 unresolved exports |

The seven failing audit exports remain grouped execute/execute_ex and join rows
execute/execute_controlled/count_controlled/project/project_controlled. No
finding was waived. No GPU correctness, live PostgreSQL behavior, or speedup
claim is made. The native build also retains its macOS deployment-target warning.

The initial metadata run had 24 failed expectations: floating SUM with an I64
accumulator is malformed (ERROR), not merely unsupported. Expectations were
corrected and the final run passed; the initial failure log is retained.

## Provenance and remaining work

Raw build, metadata, GPU, audit, Rust, lint and contract logs accompany this
file. The audit JSON and native metadata binding retain audit provenance.
`source-snapshot.tar.gz` contains tracked and untracked implementation and
documentation inputs, excluding Git metadata, generated builds and evidence.
`SHA256SUMS` seals this bundle. No benchmark ran for this change.

The audited dylib SHA-256 was
`daf367f279bb7b531b68d0fe2d510944c0a07c76e8a01eb1a2026c214653c097`.
The 131-export manifest SHA-256 was
`d4dc7e0e01e18f9e642a5ac69ae4e4c0f361df6ef88d570dce361ea240b5402c`.

Remaining: Rust integration, GPU oracle execution, live PostgreSQL differential
tests, resolution of the audit gate, and measured end-to-end qualification.
This session cannot commit or push because `.git` is explicitly read-only.
