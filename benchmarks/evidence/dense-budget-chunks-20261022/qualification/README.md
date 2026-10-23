# Dense chunk qualification preparation

Not executed. This runner does not build/install, change source or claim complete
chunk qualification. It runs the unchanged SQL121 file at 64/128/256/DEFAULT and
a separate original pinned, fresh-derived three-key 64 MiB positive probe, and a
measured pinned-raw-only 32 MiB impossible control. Every
matrix status is retained even when a prior case fails; any failure fails the
overall known-gates run. GPU, production-module and default cost/min-batch guards
must pass in every backend. Kernel timeout is explicitly 60 seconds, not default;
statement timeout is 20 minutes and the owned psql child is bounded to 30 minutes.

Invocation after review and exclusive runtime handoff:

    bash run-known-production.sh 18 <fresh-stage> <absolute-installed-provenance>
    bash run-known-production.sh 19 <fresh-stage> <absolute-installed-provenance>

The provenance directory must contain immutable copies of all nine final changed
source files under repository-relative paths, source.sha256 verifying those
copies, and installed-library.sha256 containing the exact owned prefix module's
absolute path/hash. Runtime owner must bind these receipts to the actual
production install. Current mutable worktree source is never used as installed
identity. SQL121 is additionally pinned to its unchanged committed hash.

The harness manifest is created only after independent source review. The runner
fails if it is absent. Each major owns a fresh run directory, cache, pgdata and
short Unix socket directory. Existing paths are rejected, not deleted. PostgreSQL
stop/status/PID, source/module/harness after-checks and terminal status are kept.
No warning filtering or latency claim is made.

## Explicit remaining dependencies

1. Selected chunk-row telemetry and maximal-fit native-quote comparison are not
   currently SQL-visible. Existing EXPLAIN provides planned lifecycle calls,
   executed Batches and a verification flag; the positive probe retains those,
   but cannot claim directly observed selected-chunk/maximality qualification.
   The implementation owner plans exact internal decision/native-oracle unit and
   pg_test assertions within the approved nine-file scope. Those separate gates
   must supply chunk/maximality evidence; no new production SQL API is assumed.
2. The plan owner explicitly approved the measured pinned-raw lower-bound
   negative: assert actual raw bytes > 32 MiB, then lower the local budget to
   32 MiB. Pin, columns, generation and raw bytes must remain identical after the
   budget change and query. Require exact budget decline, zero dispatch and
   native parity. This is not a native-layout quote: raw ownership alone proves
   impossibility, independent of any nonnegative workspace. Exact native layout
   boundaries still require the separate selector tests in item 1.
3. Final installed nine-file source/module identities are not available until
   the implementation owner freezes and installs production builds.

First-execution EXPLAIN ANALYZE proves fresh-derived dispatch/call accounting;
the following actual-producing query independently requires positive GPU kernel
delta and checks exact native parity with warm derived state. This is
not a claim that EXPLAIN returns its actual result rows for parity comparison.
