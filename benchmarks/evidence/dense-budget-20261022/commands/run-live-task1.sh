#!/usr/bin/env bash
set -euo pipefail
ulimit -n 4096
cd /Users/contra/Projects/pg_accel
run_root="$PWD/.codex/scratch/hosted-budget-20261018"
major=$1
stage=$2
filter=${3-test_dense_execution_reclaims_stale_artifact_at_exact_pressure}
case "$major" in 18) version=18.4;; 19) version=19beta1;; *) exit 2;; esac
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PG_CONFIG="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version/bin/pg_config"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
export RUST_TEST_THREADS=1
export ACPP_APPDB_DIR="$run_root/cache-pg$major-$stage"
test ! -e "$run_root/pgdata$major-$stage"
mkdir -p "$ACPP_APPDB_DIR"
shasum -a 256 pg_accel/src/engine/executor/agg/descriptor.rs pg_accel/src/engine/executor/agg/mod.rs pg_accel/src/engine/ffi/planner_hooks/generic_groupagg.rs pg_accel/src/engine/ffi/planner_hooks/shape/mod.rs pg_accel/src/engine/residency/store.rs pg_accel/src/engine/residency/mod.rs pg_accel/src/gpu/grouped_agg.rs pg_accel/src/tests/phase2_cache.rs > "$run_root/pg$major-$stage-source.sha256"
set +e
cargo pgrx test --package pg_accel --pgdata "$run_root/pgdata$major-$stage" "pg$major" "$filter" > "$run_root/pg$major-$stage.log" 2>&1
result=$?
set -e
shasum -a 256 -c "$run_root/pg$major-$stage-source.sha256" > "$run_root/pg$major-$stage-source-after.log"
printf 'PG%s %s terminal=%s\n' "$major" "$stage" "$result"
exit "$result"
