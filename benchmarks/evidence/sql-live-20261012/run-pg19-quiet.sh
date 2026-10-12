#!/usr/bin/env bash
set -uo pipefail
ulimit -n 4096
repo_root=/Users/contra/Projects/pg_accel
cd "$repo_root" || exit 1
run_root="$repo_root/.codex/scratch/sql-live-20261012/pg19-quiet-rerun"
prefix="$repo_root/.codex/scratch/hash-implied-filter-20261012/postgres/19beta1"
bindir="$prefix/bin"
mkdir -p "$run_root/appdb"
# Fresh private cache: this is not a prewarmed subset repeat.
export ACPP_APPDB_DIR="$run_root/appdb"
shasum -a 256 -c .codex/scratch/sql-live-20261012/fixed/pg19/module-sha256.log > "$run_root/module-identity.log" 2>&1 || exit 1
"$bindir/initdb" -D "$run_root/data" --no-locale --encoding=UTF8 --auth=trust > "$run_root/initdb.log" 2>&1 || exit 1
"$bindir/pg_ctl" -D "$run_root/data" -l "$run_root/server.log" -o '-p 29719 -k /private/tmp -c listen_addresses=localhost -c shared_preload_libraries=pg_accel' -w start > "$run_root/start.log" 2>&1 || exit 1
stop_owned() {
    "$bindir/pg_ctl" -D "$run_root/data" -m fast -w stop > "$run_root/stop.log" 2>&1
    printf 'PG19 quiet cluster stop exit=%s\n' "$?"
}
trap stop_owned EXIT
connection='host=localhost port=29719 dbname=postgres'
"$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1 -f sql/init/01-create-extensions.sql > "$run_root/extensions.log" 2>&1 || exit 1
"$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'SELECT version(); SELECT * FROM pg_accel_device_info(); SELECT * FROM pg_accel_device_limits(); SELECT extname,extversion FROM pg_extension ORDER BY extname;' > "$run_root/runtime-profile.log" 2>&1 || exit 1
PG_ACCEL_PSQL="$bindir/psql" PATH="$bindir:$PATH" PGOPTIONS='-c pg_accel.kernel_timeout_ms=5000' \
    PG_ACCEL_PG_MAJOR=19 PG_ACCEL_SQL_TEST_REQUIRE_EXTENSION=1 PG_ACCEL_RELEASE_MODE=1 \
    PG_ACCEL_SQL_TEST_EXPECT_KERNEL_TIMEOUT_MS=5000 PG_ACCEL_SQL_TEST_ARTIFACT_DIR="$run_root/sql" \
    sql/tests/run_all.sh "$connection" > "$run_root/sql-tests.log" 2>&1
sql_status=$?
printf 'PG19 quiet full SQL runner exit=%s\n' "$sql_status"
python3 scripts/coverage_tools.py sql-inventory --tests-dir sql/tests --manifest coverage/sql-semantic-assertions.json \
    --results "$run_root/sql/results.tsv" --output-dir "$run_root/inventory" --threshold 90 --execution-status "$sql_status" \
    > "$run_root/inventory.log" 2>&1
inventory_status=$?
printf 'PG19 quiet fixed semantic inventory exit=%s\n' "$inventory_status"
if [ "$sql_status" -ne 0 ] || [ "$inventory_status" -ne 0 ]; then
    exit 1
fi
