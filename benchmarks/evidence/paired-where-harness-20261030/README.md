# Paired WHERE harness correctness checkpoint

This archive qualifies the test-only paired diagnostic harness, not performance or broader WHERE admission. No paired timing run is included. Parent checkpoint `da57e2a9` contains the reviewed correction source; `raw/paired-validate7/source.sha256` binds all 19 source files, and the PG18/PG19 cleanup3 source manifests additionally bind the native implementation/header. The earlier harness checkpoint was `b501cb38`.

## Verified results

- Validation7: 1,365 ordinary tests, strict pg18/pg_test Clippy, formatting, PG18/PG19 test compilation, three primary-error receipt tests, four durable-sink tests, and four shell status-precedence cases passed.
- Offline checker: four tests passed in 19.444 seconds, including malformed/incomplete/duplicate receipt and inventory rejection cases. The exact synthetic generator is retained; its large generated corpus is excluded.
- PG18 cleanup3: exact one-test inventory passed in 17.94 seconds.
- PG19 cleanup3: exact one-test inventory passed in 27.64 seconds.
- Cleanup tests exercise both candidate executors, installed-owner failure/cancellation paths, full result parity on recovery, ledger restoration, and targeted eviction rejection. Actual kernel mode, nonzero dispatch and exact lifecycle counts are required; adaptive and nonadaptive dispatch completion are distinguished. The runner separately consumes actual EOF.

Each final wrapper terminal is zero and source/harness/protected-file postflight checks passed. Compiled libraries are identified by hashes, not distributed here. Auxiliary Rust integration suites with zero matching tests are explicitly checked separately from the mandatory nonzero main test inventory.

## Preserved failures

Compile1 and compile3 retain compiler failures. Validation1, validation3 and validation5 retain strict lint failures. PG18 cleanup1 failed before its test body because the SQL registration schema differed from pgrx's runner schema. Cleanup2 reached recovery and passed full parity, then failed because a test-only accessor read adaptive-only completion state for the Legacy executor. Cleanup3 fixes the accessor without mutating production completion state or dropping parity/completion assertions. Both failed live runs remain unchanged; PG19 was not started after either failure.

Primary Result errors now survive secondary receipt/logging failures. A secondary failure rejects otherwise successful work. Shell cleanup retains original nonzero process status separately from secondary cleanup/checker failures. These changes do not alter production costs, admission, cache policy, fixtures or sampling.

## Contents and replay limits

`copy-inventory.json` maps 180 original scratch artifacts to `raw/`, with their exact byte sizes and SHA-256 hashes (2,864,955 bytes). It includes all compile1–4 and validation1–7 receipts, both live failures and both final live passes, earlier source snapshots/patches, and checker/wrapper/test sources. Source paths in historical seals are absolute or repository-relative to the original machine; they are provenance records, not relocatable replay commands. Earlier seals intentionally reference earlier source versions.

`raw/run-paired-measurement.sh` is the historical, disabled draft and still references the failed cleanup1 qualification. It must not be used as a qualified measurement command. A separately reviewed new wrapper is required to bind cleanup3 before any measurement authorization. No performance speedup, ranking, full-query timing, or successful measurement is claimed here.

PGDATA, JIT caches, compiled binaries and the large synthetic checker corpus are excluded. Replaying the commands requires the pinned local Rust/PostgreSQL/native toolchain and installed libraries; hashes alone do not provide those dependencies. Preserve the bounded run directories and every failed sample when performing separately authorized replay.
