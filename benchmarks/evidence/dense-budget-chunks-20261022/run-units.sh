#!/usr/bin/env bash
set -euo pipefail
ulimit -n 4096
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/dense-budget-chunks-20261022"
stage=$1
filter=${2:-}
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PG_CONFIG="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4/bin/pg_config"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
export RUST_TEST_THREADS=1
export ACPP_APPDB_DIR="$task_root/unit-cache"
test ! -e "$task_root/$stage.log"
mkdir -p "$ACPP_APPDB_DIR"
set +e
cargo test --package pg_accel --lib --no-default-features --features pg18 "$filter" -- --test-threads=1 > "$task_root/$stage.log" 2>&1
result=$?
set -e
printf 'PG18 ordinary unit stage=%s filter=%s terminal=%s\n' "$stage" "$filter" "$result"
exit "$result"
