#!/usr/bin/env bash
set -uo pipefail
ulimit -n 4096
repo_root=/Users/contra/Projects/pg_accel
cd "$repo_root" || exit 1
run_root="$repo_root/.codex/scratch/uniform-budget-live-20261012"
export PGRX_HOME="$run_root/pgrx"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
export RUST_TEST_THREADS=1
overall_status=0
for major in 18 19; do
    version=18.4
    [ "$major" = 19 ] && version=19beta1
    prefix="$run_root/postgres/$version"
    export PG_CONFIG="$prefix/bin/pg_config"
    export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
    "$PG_CONFIG" --version --bindir --sharedir --pkglibdir > "$run_root/pg$major-config.log"
    cargo pgrx test --package pg_accel --pgdata "$run_root/pgdata$major" \
        "pg$major" pg_dense_integer_execution_budget_and_bounded_pool_ownership \
        > "$run_root/pg$major-test.log" 2>&1
    status=$?
    printf 'PG%s exact uniform grouped budget fixture exit=%s\n' "$major" "$status"
    [ "$status" -eq 0 ] || overall_status=1
done
exit "$overall_status"
