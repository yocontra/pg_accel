#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task="$PWD/.codex/scratch/hash-count-where-rust-20261030"
run="$task/paired-validate5"
test ! -e "$run"
mkdir "$run"
files=(pg_accel/src/engine/ffi/custom_scan/{mod,paired_driver,paired_backend,paired_runner,paired_population,paired_fixtures,paired_sink,paired_cleanup_tests}.rs pg_accel/src/engine/ffi/planner_hooks/{mod,generic_groupagg}.rs pg_accel/src/engine/ffi/planner_hooks/generic_groupagg/paired_recipes.rs pg_accel/src/engine/residency/{mod,store}.rs pg_accel/src/engine/residency/store/paired.rs pg_accel/src/engine/executor/agg/{mod,descriptor,execute}.rs pg_accel/src/engine/stats.rs pg_accel/src/gpu/grouped_agg.rs)
shasum -a256 "${files[@]}" > "$run/source.sha256"
shasum -a256 "$0" "$task/test-paired-exit-status.sh" "$task/paired-exit-status.sh" "$task/paired-sink.expected" "$task/run-paired-correctness.sh" "$task/paired-cleanup.expected" > "$run/harness.sha256"
stage=fmt
finish() {
 primary=$?; trap - EXIT; set +e
 secondary=0
 shasum -a256 -c "$run/source.sha256" > "$run/source-after.log" || secondary=1
 shasum -a256 -c "$run/harness.sha256" > "$run/harness-after.log" || secondary=1
 result=$primary; if test "$result" = 0; then result=$secondary; fi
 printf 'stage=%s\nprimary=%s\nsecondary=%s\nterminal=%s\n' "$stage" "$primary" "$secondary" "$result" > "$run/terminal.log"
 exit "$result"
}
trap finish EXIT
export ACPP_PREFIX="$PWD/.pgaccel/acpp/current" ACPP_TARGETS=generic CMAKE_BUILD_PARALLEL_LEVEL=4 CARGO_NET_OFFLINE=true
base="$PWD/.codex/scratch/dense-planning-diagnostic-20261030"
export PGRX_HOME="$base/pgrx" PG_CONFIG="$base/postgres18/bin/pg_config" PGRX_PG_CONFIG_PATH="$base/postgres18/bin/pg_config"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)" BINDGEN_EXTRA_CLANG_ARGS="-isysroot $(xcrun --show-sdk-path)"
timeout=/opt/homebrew/bin/gtimeout
"$timeout" 60s cargo fmt --all --check > "$run/fmt.log" 2>&1
stage=lint
"$timeout" --signal=TERM --kill-after=10s 15m cargo clippy --locked -p pg_accel --no-default-features --features pg18,pg_test --all-targets -- -D warnings > "$run/lint.log" 2>&1
stage=ordinary
"$timeout" --signal=TERM --kill-after=10s 15m cargo test --locked -p pg_accel --lib --no-default-features --features pg18 -- --test-threads=1 > "$run/ordinary.log" 2>&1
stage=shell-cases
"$timeout" 30s bash "$task/test-paired-exit-status.sh" > "$run/shell.log" 2>&1
stage=receipt-units
"$timeout" --signal=TERM --kill-after=10s 15m cargo test --locked -p pg_accel --lib --no-default-features --features pg18,pg_test 'engine::ffi::custom_scan::paired_driver::receipt_tests::' -- --test-threads=1 > "$run/receipt.log" 2>&1
for name in failed_secondary_writer_does_not_replace_primary receipt_failure_preserves_each_primary_and_rejects_success secondary_receipt_is_bounded_and_never_retried; do
 rg -F "test engine::ffi::custom_scan::paired_driver::receipt_tests::$name ... ok" "$run/receipt.log"
done > "$run/receipt-inventory.log"
rg '^test result: ok\. 3 passed; 0 failed; 0 ignored; 0 measured;' "$run/receipt.log" >> "$run/receipt-inventory.log"
stage=sink-units
"$timeout" --signal=TERM --kill-after=10s 15m cargo test --locked -p pg_accel --lib --no-default-features --features pg18,pg_test 'engine::ffi::custom_scan::paired_sink::tests::' -- --test-threads=1 > "$run/sink.log" 2>&1
sed -n 's/^test \(engine::ffi::custom_scan::paired_sink::tests::[^ ]*\) \.\.\. ok$/\1/p' "$run/sink.log" | sort > "$run/sink-actual.expected"
sort "$task/paired-sink.expected" > "$run/sink-wanted.expected"
cmp "$run/sink-actual.expected" "$run/sink-wanted.expected"
rg '^test result: ok\. 4 passed; 0 failed; 0 ignored; 0 measured;' "$run/sink.log" > "$run/sink-inventory.log"
for major in 18 19; do
 stage="compile-pg$major"
 export PG_CONFIG="$base/postgres$major/bin/pg_config" PGRX_PG_CONFIG_PATH="$base/postgres$major/bin/pg_config"
 "$timeout" --signal=TERM --kill-after=10s 15m cargo test --locked -p pg_accel --lib --no-default-features --features "pg$major,pg_test" --no-run > "$run/pg$major.log" 2>&1
done
stage=complete
