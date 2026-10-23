# Dense budget chunk qualification

## Source boundaries

- `integration-candidate6`: reviewed production implementation plus ownership-aware live fixture. Both non-pg_test production builds and shared-owner proofs use this exact snapshot.
- `integration-candidate7`: production implementation unchanged; cancellation fixture now materializes a normally costed generic GPU plan before the runtime one-row interrupt hook. Both full PostgreSQL suites and both visible hardware proofs use this snapshot.
- `integration-candidate9`: final source. `lint-only.diff` records the complete candidate7-to-final change: two documented local argument-count lint allowances, equivalent unsigned saturating subtraction, removal of a redundant Option dereference, and a last-use move instead of cloning pointer-free test metadata. No capability or execution-policy change. Candidate8 retained the fifth lint and its failure is preserved.

## Completed gates

- `ordinary-integrated2.log`: 1,335 ordinary unit tests passed on candidate5 (same production implementation as candidate6; later phase2 changes are pg_test-only).
- `qualification/pg18-candidate6` and `qualification/pg19-candidate6`: non-pg_test production, all six cases exit0: fresh 64MiB positive; unchanged SQL121 at 64/128/256/default; measured pinned-raw 32MiB impossible-owner negative. Exact native parity, default admission costs, real dispatch and source/module identities retained. These runs set kernel timeout60s and statement timeout20min; they do not establish latency or speedup.
- `qualification/shared-owner/pg18-candidate6` and `pg19-candidate6`: real foreign-active and foreign-idle protection, local idle credit, exact exit release and final ledger0. Fixed-owner lower bound18,000,306 exceeds the17,825,792 budget independently of any selectable B workspace.
- `pg18-final7.log` and `pg19-final7.log`: each 1,867 library tests plus55 integration tests passed; zero failures, ignored or filtered tests. Cancellation recovery and the new budget test explicitly passed. All nine source-before/after checks passed.
- `visible-pg18-final7` and `visible-pg19-final7`: affirmative hardware-derived GPU, selected chunk605184/calls3/peak19104216 at64MiB; prepared128→64 reselects1000000→605184. Actual construction policy `ephemeral_fused`, raw36000693/new artifact13000221/retained derived0, idle50648304→10530264 with required idle release true. Independent kernels0→12 and integer-mode0→8. Both owned clusters stopped/status3/no PID, source/module/harness after-checks passed. Expected installed pg_test identities are in `pgtest-install-pg{18,19}-final7`.
- `candidate9-rustfmt.log` and `candidate9-clippy.log`: Rust2024 formatting and pg18+pg_test all-target Clippy with `-D warnings` passed; exact nine-file before/after manifests retained.
- `pg18-postlint9.log` and `pg19-postlint9.log`: each final-source phase2 module13 passed,1854 filtered; integration binary0 run/55 filtered. These are focused final-byte checks, not second full suites. Both wrappers exited0 and all nine source-after checks passed.

## Warning and failure boundaries

Both final visible server logs have18 SDK-link warnings and one AdaptiveCpp JIT-cache warning; no PostgreSQL WARNING/ERROR/FATAL/PANIC. Production logs retain92 SDK-link warnings each; shared-owner logs24 SDK-link plus two JIT-cache warnings each. No blanket warning-free claim.

Preserve the first production SQL12164 artifact-construction failure, mistaken one-lane/JSON-alias/retained-owner fixture failures, candidate6 full-PG19 cancellation failure, and lint failures. The cancellation diagnostic observed `generic_cost_not_competitive`; its native-counter7 is cumulative, not an attempt delta. See `cancel-diagnostic-pg19-candidate6/INTERPRETATION.md` for the SQL0/wrapper1 and verified shutdown recovery distinction.

The first final lint wrapper used Rust2021 erroneously; `final7-rustfmt-wrong-edition.log` preserves that harness error. The repository declares2024, and the corrected formatting check passed without changing source.

All listed gates are terminal. Runtime/GPU ownership is released after the final PG19 focused wrapper exit0. The final candidate9 source manifest is frozen for publication review; no Git commands were run by this worker.
