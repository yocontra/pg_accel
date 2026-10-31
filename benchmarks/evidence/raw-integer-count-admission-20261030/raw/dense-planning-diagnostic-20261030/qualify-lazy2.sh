#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task="$PWD/.codex/scratch/dense-planning-diagnostic-20261030"
run="$task/lazy-qualification2"
test ! -e "$run"
mkdir "$run"
{ printf '%s\n' Cargo.toml Cargo.lock pg_accel/Cargo.toml pg_accel/build.rs pg_accel_bench/Cargo.toml pgaccel-kernels/CMakeLists.txt .acpp-version; rg --files pg_accel/src pg_accel_bench/src pgaccel-kernels/src pgaccel-kernels/include pgaccel-kernels/test; } | LC_ALL=C sort -u | while IFS= read -r file; do shasum -a256 "$file"; done > "$run/source.sha256"
shasum -a256 "$task/qualify-lazy2.sh" "$task/run-pg-tests.sh" "$task/lint.sh" > "$run/harness.sha256"
finish() {
 local result=$?
 trap - EXIT
 set +e
 shasum -a256 -c "$run/source.sha256" > "$run/source-after.log" || result=1
 shasum -a256 -c "$run/harness.sha256" > "$run/harness-after.log" || result=1
 printf 'terminal=%s\n' "$result" > "$run/terminal.log"
 exit "$result"
}
trap finish EXIT
bash "$task/lint.sh" > "$run/lint.log" 2>&1
for major in 18 19; do
 bash "$task/run-pg-tests.sh" "$major" lazy-inventory2 test_gpu_device_source_inventory_is_order_independent
 bash "$task/run-pg-tests.sh" "$major" lazy-live2 engine::residency::store::pg_tests
 bash "$task/run-pg-tests.sh" "$major" lazy-union2 test_loaded_raw_quote_keeps_full_union_and_selected_type_metadata
 bash "$task/run-pg-tests.sh" "$major" lazy-raw2 pg_raw_count
done
cargo fmt --all --check > "$run/fmt.log" 2>&1
