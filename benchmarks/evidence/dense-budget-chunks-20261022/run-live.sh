#!/usr/bin/env bash
set -euo pipefail
ulimit -n 4096
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/dense-budget-chunks-20261022"
major=$1
stage=$2
filter=$3
manifest=$4
case "$major" in 18) version=18.4;; 19) version=19beta1;; *) exit 2;; esac
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PG_CONFIG="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version/bin/pg_config"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
export RUST_TEST_THREADS=1
export ACPP_APPDB_DIR="$task_root/cache-pg$major-$stage"
test ! -e "$task_root/pgdata$major-$stage"
test ! -e "$task_root/pg$major-$stage.log"
mkdir -p "$ACPP_APPDB_DIR"
shasum -a 256 -c "$manifest" > "$task_root/pg$major-$stage-source-before.log"
set +e
cargo pgrx test --package pg_accel --pgdata "$task_root/pgdata$major-$stage" "pg$major" "$filter" > "$task_root/pg$major-$stage.log" 2>&1
result=$?
set -e
shasum -a 256 -c "$manifest" > "$task_root/pg$major-$stage-source-after.log"
printf 'PG%s live stage=%s filter=%s terminal=%s\n' "$major" "$stage" "$filter" "$result"
exit "$result"
