#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
run="$PWD/.codex/scratch/hash-count-where-rust-20261030/cost-validate3"
test ! -e "$run"
mkdir "$run"
files=(pg_accel/src/engine/spec/abi.rs pg_accel/src/gpu/bridge.rs pg_accel/src/gpu/grouped_agg.rs pg_accel/src/engine/executor/agg/descriptor.rs pg_accel/src/engine/executor/agg/hash_partition.rs pg_accel/src/engine/ffi/planner_hooks/shape/cost.rs pg_accel/src/engine/ffi/planner_hooks/shape/tests.rs pg_accel/src/engine/ffi/planner_hooks/generic_groupagg.rs pg_accel/src/engine/executor/agg/raw_count_test.rs pg_accel/src/engine/executor/agg/h3_count_test.rs)
files+=(pg_accel/src/engine/ffi/planner_hooks/mod.rs)
shasum -a256 "${files[@]}" > "$run/source.sha256"
shasum -a256 "$0" > "$run/harness.sha256"
stage=ordinary
finish() {
 result=$?
 trap - EXIT
 set +e
 shasum -a256 -c "$run/source.sha256" > "$run/source-after.log" || result=1
 printf 'stage=%s\nterminal=%s\n' "$stage" "$result" > "$run/terminal.log"
 exit "$result"
}
trap finish EXIT
export ACPP_PREFIX="$PWD/.pgaccel/acpp/current" ACPP_TARGETS=generic CMAKE_BUILD_PARALLEL_LEVEL=4 CARGO_NET_OFFLINE=true
base="$PWD/.codex/scratch/dense-planning-diagnostic-20261030"
export PGRX_HOME="$base/pgrx" PG_CONFIG="$base/postgres18/bin/pg_config" PGRX_PG_CONFIG_PATH="$base/postgres18/bin/pg_config"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)" BINDGEN_EXTRA_CLANG_ARGS="-isysroot $(xcrun --show-sdk-path)"
timeout=/opt/homebrew/bin/gtimeout
"$timeout" --signal=TERM --kill-after=10s 15m cargo test --locked -p pg_accel --lib --no-default-features --features pg18 -- --test-threads=1 > "$run/ordinary.log" 2>&1
stage=lint
"$timeout" --signal=TERM --kill-after=10s 15m cargo clippy --locked -p pg_accel --no-default-features --features pg18 --all-targets -- -D warnings > "$run/lint.log" 2>&1
stage=fmt
cargo fmt --all --check > "$run/fmt.log" 2>&1
for major in 18 19; do
 stage="compile-pg$major"
 export PG_CONFIG="$base/postgres$major/bin/pg_config" PGRX_PG_CONFIG_PATH="$base/postgres$major/bin/pg_config"
 "$timeout" --signal=TERM --kill-after=10s 15m cargo test --locked -p pg_accel --lib --no-default-features --features "pg$major,pg_test" --no-run > "$run/pg$major.log" 2>&1
done
stage=complete
