#!/usr/bin/env bash
set -euo pipefail
ulimit -n 4096
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/residency-ledger-20261018"
major="$1"
source_stage=candidate11
case "$major" in
    18) version=18.4 ;;
    19) version=19beta1 ;;
    *) exit 2 ;;
esac
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version"
bindir="$prefix/bin"
run_dir="$task_root/runtime-pg$major-final-20261022"
socket_dir="$PWD/.codex/scratch/ledger-f$major-20261022"
test ! -e "$run_dir"
test ! -e "$socket_dir"
test -z "${PGOPTIONS:-}"
mkdir -p "$run_dir/cache" "$socket_dir"
export ACPP_APPDB_DIR="$run_dir/cache"
pgdata="$run_dir/pgdata"
port=$((32000+major))
shasum -a 256 -c "$task_root/candidate2.sha256" >"$run_dir/harness-before.log"
shasum -a 256 -c "$task_root/installed-sources/pg$major-final-library.sha256" >"$run_dir/module-before.log"
cp "$task_root/installed-sources/pg$major-final-library.sha256" "$run_dir/installed-library.sha256"
cp "$task_root/installed-sources/$source_stage/source.sha256" "$run_dir/installed-source.sha256"
(cd "$task_root/installed-sources/$source_stage" && shasum -a 256 -c source.sha256) >"$run_dir/immutable-source-check.log"
printf 'PG%s installed production %s; current engine worktree is not the installed build\n' \
    "$major" "$source_stage" >"$run_dir/provenance.log"
"$bindir/initdb" -D "$pgdata" --no-locale --encoding=UTF8 --auth=trust >"$run_dir/initdb.log" 2>&1
cleanup() {
    local original_status=$?
    set +e
    "$bindir/pg_ctl" -D "$pgdata" -m fast -w -t 30 stop >"$run_dir/stop.log" 2>&1
    local stop_status=$?
    "$bindir/pg_ctl" -D "$pgdata" status >"$run_dir/final-status.log" 2>&1
    local status_status=$?
    if [ "$stop_status" -ne 0 ] || [ "$status_status" -ne 3 ] || [ -e "$pgdata/postmaster.pid" ]; then
        echo "error: owned cluster cleanup not confirmed" >&2
        if [ "$original_status" -eq 0 ]; then original_status=1; fi
    fi
    exit "$original_status"
}
trap cleanup EXIT
"$bindir/pg_ctl" -D "$pgdata" -l "$run_dir/server.log" \
    -o "-p $port -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel" \
    -w -t 30 start >"$run_dir/start.log" 2>&1
connection="host=$socket_dir port=$port dbname=postgres"
"$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1 \
    -c 'CREATE EXTENSION pg_accel' >"$run_dir/extension.log" 2>&1
"$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1 >"$run_dir/production-identity.log" 2>&1 <<'SQL'
SELECT version();
SELECT extname, extversion FROM pg_extension WHERE extname='pg_accel';
SELECT count(*) AS installed_pg_test_functions FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='tests';
SELECT * FROM pg_accel_device_info();
SELECT * FROM pg_accel_device_limits();
DO $$ BEGIN
    IF EXISTS(SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='tests') THEN
        RAISE EXCEPTION 'requires non-pg_test production module';
    END IF;
    IF (SELECT gpu_available FROM pg_accel_device_info()) IS DISTINCT FROM true THEN
        RAISE EXCEPTION 'requires real GPU';
    END IF;
END $$;
SQL
PSQL="$bindir/psql" PG_ACCEL_LEDGER_ARTIFACT_DIR="$run_dir/artifacts" \
    bash scripts/residency_ledger_integration.sh "$connection" >"$run_dir/maintained.log" 2>&1
shasum -a 256 -c "$task_root/candidate2.sha256" >"$run_dir/harness-after.log"
shasum -a 256 -c "$run_dir/installed-library.sha256" >"$run_dir/module-after.log"
printf 'PG%s maintained candidate2 production script PASS on installed %s\n' "$major" "$source_stage"
