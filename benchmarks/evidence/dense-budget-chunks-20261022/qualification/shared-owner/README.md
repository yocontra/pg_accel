# Shared-owner qualification (not yet executed)

Reuses the prior hosted-budget shared cursor/ledger workload. The foreign
decline budget now comes from independently observed A resident bytes plus B
pinned raw bytes, not the old full-workspace requirement. Require
`B_live <= budget < A_resident + B_raw`, with integer MiB rounding downward.
No suitable interval is a failed fixture, never a skipped/passed test. This
lower bound excludes all B derived/idle allocations and all A execution bytes;
smaller chunks cannot make these fixed owners fit. Foreign A resident owners
remain inaccessible to B while A's cursor is active and after it becomes idle.

The original controls remain: local pool reuse and exact parity; A positive
dispatch and held active output; B budget-specific decline with zero dispatch
and native parity; A identity/pin protection; exact active-to-idle transfer;
continued foreign-idle decline; A exit restoring B dispatch/parity; final zero
ledger after B exit. Active bytes are the original ledger residual, not a new
direct active-allocation API. Raw SQL and observed equations remain in logs.

Only the sole runtime owner may execute after independent harness approval:

```
bash .codex/scratch/dense-budget-chunks-20261022/qualification/shared-owner/run-shared-proof.sh 18 candidate6 /absolute/path/to/production-pg18-candidate6
```

Then run PG19 serially with its own installed provenance. Each provenance
directory must contain exact immutable nine-file source copies/source.sha256
and the single installed-library.sha256 entry for that major's owned prefix.
The runner verifies manifests before/after and never builds or installs.
Fresh run directories and reserved short qh18/qh19 socket directories are
required. Existing paths are rejected; never remove live sockets. Only after
stopped/status3/noPID confirmation may an empty owned socket directory be
removed with nonrecursive rmdir for a later fresh stage.

Every actual workload backend checks non-pg_test production, real M2 Max,
hardware-derived limits and default cost/minimum batch admission. Explicit
2-minute statement and 60-second kernel safety ceilings are not defaults.
Marker waits, client shutdown and owned-cluster stop are bounded. Preserve all
warnings and errors; no warning-free, latency, CI or chunk-maximality claim.
Ordinary/native selector tests and separate production SQL121 runs provide
those other scoped receipts. This preparation has only shell syntax checks.
