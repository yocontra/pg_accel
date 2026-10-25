#!/bin/bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/h3-count-column-20261022"
stage=${1:?stage required}
filter=${2-}
[[ "$stage" =~ ^[a-zA-Z0-9_-]+$ ]]
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PG_CONFIG="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4/bin/pg_config"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
export RUST_TEST_THREADS=1
ulimit -n 4096
set +e
cargo test --package pg_accel --lib --no-default-features --features pg18 "$filter" -- --test-threads=1 > "$task_root/$stage.log" 2>&1
task_rc=$?
set -e
tail -70 "$task_root/$stage.log"
exit "$task_rc"
