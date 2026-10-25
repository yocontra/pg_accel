#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/h3-count-column-20261022"
build_dir="$task_root/production-build18"
test ! -e "$build_dir"
mkdir -p "$build_dir"
shasum -a256 \
  Cargo.toml Cargo.lock pg_accel/Cargo.toml pg_accel_bench/Cargo.toml \
  pg_accel/src/engine/executor/agg/descriptor.rs \
  pg_accel/src/engine/executor/agg/h3_count_test.rs \
  pg_accel/src/engine/executor/agg/mod.rs \
  pg_accel/src/engine/ffi/planner_hooks/generic_groupagg.rs \
  pg_accel/src/engine/residency/mod.rs pg_accel/src/engine/residency/store.rs \
  pg_accel/src/gpu/grouped_agg.rs pg_accel/src/tests/mod.rs \
  pg_accel_bench/src/main.rs pg_accel_bench/src/runner.rs pg_accel_bench/src/report.rs \
  pg_accel_bench/src/h3_protection_test.rs \
  pg_accel_bench/src/workloads/h3_cell_to_parent.rs \
  pg_accel_bench/src/workloads/mod.rs pg_accel_bench/src/workloads/registry.rs \
  > "$build_dir/source.sha256"
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PG_CONFIG="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4/bin/pg_config"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
ulimit -n 4096
cargo pgrx install --package pg_accel --pg-config "$PG_CONFIG" --no-default-features --features pg18 --release > "$build_dir/install.log" 2>&1
cargo build -p pg_accel_bench --release > "$build_dir/bench-build.log" 2>&1
shasum -a256 -c "$build_dir/source.sha256" > "$build_dir/source-after.log"
shasum -a256 "$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4/lib/pg_accel.dylib" > "$build_dir/module.sha256"
shasum -a256 target/release/pg_accel_bench > "$build_dir/bench.sha256"
