#!/usr/bin/env bash
set -uo pipefail
ulimit -n 4096
repo_root=/Users/contra/Projects/pg_accel
cd "$repo_root" || exit 1
run_root="${1:-$repo_root/.codex/scratch/sql-live-20261012}"
mkdir -p "$run_root"
prefix_root="$repo_root/.codex/scratch/hash-implied-filter-20261012/postgres"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
export PGRX_HOME="$repo_root/.codex/scratch/hash-implied-filter-20261012/pgrx"
active_pg_ctl=
active_pgdata=
stop_owned_cluster() {
    if [ -n "$active_pg_ctl" ]; then
        "$active_pg_ctl" -D "$active_pgdata" -m fast -w stop > "$run_root/pg$active_major/stop.log" 2>&1
        local stop_status=$?
        printf 'PG%s stop exit=%s\n' "$active_major" "$stop_status"
        active_pg_ctl=
        active_pgdata=
        return "$stop_status"
    fi
}
trap stop_owned_cluster EXIT
overall_status=0
for major in 18 19; do
    version=18.4
    [ "$major" = 19 ] && version=19beta1
    prefix="$prefix_root/$version"
    bindir="$prefix/bin"
    pg_config="$bindir/pg_config"
    major_root="$run_root/pg$major"
    pgdata="$major_root/data"
    port=$((29400 + major))
    mkdir -p "$major_root"
    export PG_CONFIG="$pg_config" PGRX_PG_CONFIG_PATH="$pg_config"
    "$pg_config" --version --bindir --sharedir --pkglibdir > "$major_root/pg-config.log"
    if ! cargo pgrx install --package pg_accel --release --no-default-features --features "pg$major" --pg-config "$pg_config" > "$major_root/install.log" 2>&1; then
        printf 'PG%s release install FAILED\n' "$major"
        overall_status=1
        continue
    fi
    printf 'PG%s release install passed\n' "$major"
    shasum -a 256 "$prefix/lib/pg_accel.dylib" "$prefix/share/extension/pg_accel.control" "$prefix/share/extension/pg_accel--1.0.0-rc1.sql" > "$major_root/module-sha256.log"
    if ! "$bindir/initdb" -D "$pgdata" --no-locale --encoding=UTF8 --auth=trust > "$major_root/initdb.log" 2>&1; then
        printf 'PG%s initdb FAILED\n' "$major"
        overall_status=1
        continue
    fi
    if ! "$bindir/pg_ctl" -D "$pgdata" -l "$major_root/server.log" -o "-p $port -k /private/tmp -c listen_addresses=localhost -c shared_preload_libraries=pg_accel" -w start > "$major_root/start.log" 2>&1; then
        printf 'PG%s start FAILED\n' "$major"
        overall_status=1
        continue
    fi
    active_pg_ctl="$bindir/pg_ctl"
    active_pgdata="$pgdata"
    active_major="$major"
    connection="host=localhost port=$port dbname=postgres"
    if ! "$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1 -f sql/init/01-create-extensions.sql > "$major_root/extensions.log" 2>&1; then
        printf 'PG%s extensions FAILED\n' "$major"
        overall_status=1
        stop_owned_cluster || overall_status=1
        continue
    fi
    "$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'SELECT version(); SELECT * FROM pg_accel_device_info(); SELECT * FROM pg_accel_device_limits(); SELECT extname,extversion FROM pg_extension ORDER BY extname;' > "$major_root/runtime-profile.log" 2>&1
    PG_ACCEL_PSQL="$bindir/psql" PATH="$bindir:$PATH" PGOPTIONS='-c pg_accel.kernel_timeout_ms=5000' \
        PG_ACCEL_PG_MAJOR="$major" PG_ACCEL_SQL_TEST_REQUIRE_EXTENSION=1 PG_ACCEL_RELEASE_MODE=1 \
        PG_ACCEL_SQL_TEST_EXPECT_KERNEL_TIMEOUT_MS=5000 PG_ACCEL_SQL_TEST_ARTIFACT_DIR="$major_root/sql" \
        sql/tests/run_all.sh "$connection" > "$major_root/sql-tests.log" 2>&1
    sql_status=$?
    printf 'PG%s full SQL runner exit=%s\n' "$major" "$sql_status"
    [ "$sql_status" -eq 0 ] || overall_status=1
    python3 scripts/coverage_tools.py sql-inventory --tests-dir sql/tests --manifest coverage/sql-semantic-assertions.json \
        --results "$major_root/sql/results.tsv" --output-dir "$major_root/inventory" --threshold 90 --execution-status "$sql_status" \
        > "$major_root/inventory.log" 2>&1
    inventory_status=$?
    printf 'PG%s fixed semantic inventory exit=%s\n' "$major" "$inventory_status"
    [ "$inventory_status" -eq 0 ] || overall_status=1
    stop_owned_cluster || overall_status=1
done
exit "$overall_status"
