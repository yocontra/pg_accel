#!/usr/bin/env bash
set -uo pipefail
ulimit -n 4096
cd /Users/contra/Projects/pg_accel || exit 1
run_root="$PWD/.codex/scratch/join-status-20261018"
major=$1
stage=$2
filter=${3:-pg_resident_join}
version=18.4
if [ "$major" = 19 ]; then version=19beta1; fi
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PG_CONFIG="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version/bin/pg_config"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
export RUST_TEST_THREADS=1
export ACPP_APPDB_DIR="$run_root/cache-pg$major-$stage"
mkdir -p "$ACPP_APPDB_DIR"
shasum -a 256 pg_accel/src/gpu/bridge.rs pg_accel/src/engine/executor/join.rs pg_accel/src/tests/join_rows.rs > "$run_root/pg$major-$stage-source.sha256"
cargo pgrx test --package pg_accel --pgdata "$run_root/pgdata$major-$stage" "pg$major" "$filter" > "$run_root/pg$major-$stage.log" 2>&1
join_test_exit=$?
printf 'PG%s %s exit=%s\n' "$major" "$stage" "$join_test_exit"
shasum -a 256 -c "$run_root/pg$major-$stage-source.sha256" > "$run_root/pg$major-$stage-source-after.log" || exit 1
exit "$join_test_exit"
