#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task="$PWD/.codex/scratch/hash-count-where-rust-20261030"
run="$task/where-validate2"
test ! -e "$run"
mkdir "$run"
files=(pg_accel/src/engine/executor/agg/{hash_partition,descriptor,execute,raw_count_test,h3_count_test}.rs pg_accel/src/engine/ffi/custom_scan/explain.rs pg_accel_bench/src/workloads/{hash_count_where,mod,registry}.rs pg_accel_bench/src/{runner,report}.rs)
shasum -a256 "${files[@]}" Cargo.toml Cargo.lock pg_accel/Cargo.toml pg_accel_bench/Cargo.toml > "$run/source.sha256"
shasum -a256 "$0" > "$run/harness.sha256"
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
stage=benchmark-tests
"$timeout" --signal=TERM --kill-after=10s 15m cargo test --locked -p pg_accel_bench -- --test-threads=1 > "$run/benchmark.log" 2>&1
for name in where_population_requires_complete_exact_same_node_evidence where_gate_accepts_complete_warm_and_both_and_rejects_mutations where_evidence_fields_are_additive_and_fail_closed where_mask_and_aggregate_calls_have_independent_exact_accounting where_parser_requires_exact_key_mask_and_count_in_one_node; do
 rg -F "test report::tests::$name ... ok" "$run/benchmark.log"
done > "$run/benchmark-inventory.log"
rg -F 'test workloads::hash_count_where::tests::exact_32_cell_inventory_and_populations ... ok' "$run/benchmark.log" >> "$run/benchmark-inventory.log"
rg '^test result: ok\. [1-9][0-9]* passed; 0 failed; 0 ignored; 0 measured;' "$run/benchmark.log" >> "$run/benchmark-inventory.log"
stage=ordinary
"$timeout" --signal=TERM --kill-after=10s 15m cargo test --locked -p pg_accel --lib --no-default-features --features pg18 -- --test-threads=1 > "$run/ordinary.log" 2>&1
rg -F 'test engine::executor::agg::hash_partition::tests::population_proof_rejects_partial_unvalidated_and_reset_state ... ok' "$run/ordinary.log" > "$run/ordinary-inventory.log"
stage=benchmark-lint
"$timeout" --signal=TERM --kill-after=10s 15m cargo clippy --locked -p pg_accel_bench --all-targets -- -D warnings > "$run/benchmark-lint.log" 2>&1
stage=extension-lint
"$timeout" --signal=TERM --kill-after=10s 15m cargo clippy --locked -p pg_accel --no-default-features --features pg18,pg_test --all-targets -- -D warnings > "$run/extension-lint.log" 2>&1
for major in 18 19; do
 stage="compile-pg$major"
 export PG_CONFIG="$base/postgres$major/bin/pg_config" PGRX_PG_CONFIG_PATH="$base/postgres$major/bin/pg_config"
 "$timeout" --signal=TERM --kill-after=10s 15m cargo test --locked -p pg_accel --lib --no-default-features --features "pg$major,pg_test" --no-run > "$run/pg$major.log" 2>&1
done
stage=complete
