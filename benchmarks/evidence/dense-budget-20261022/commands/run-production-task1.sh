#!/usr/bin/env bash
set -euo pipefail
ulimit -n 4096
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/hosted-budget-20261018"
major=$1
stage=$2
case "$major" in 18) version=18.4;; 19) version=19beta1;; *) exit 2;; esac
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version"
bindir="$prefix/bin"
run_dir="$task_root/pg$major-production-$stage"
test ! -e "$run_dir"
socket_dir="$task_root/prod-s$major"
mkdir -p "$run_dir/cache" "$socket_dir"
export ACPP_APPDB_DIR="$run_dir/cache"
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PG_CONFIG="$bindir/pg_config"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
shasum -a 256 pg_accel/src/engine/executor/agg/descriptor.rs pg_accel/src/engine/executor/agg/mod.rs pg_accel/src/engine/ffi/planner_hooks/generic_groupagg.rs pg_accel/src/engine/ffi/planner_hooks/shape/mod.rs pg_accel/src/engine/residency/store.rs pg_accel/src/engine/residency/mod.rs pg_accel/src/gpu/grouped_agg.rs pg_accel/src/tests/phase2_cache.rs sql/tests/121_integer_where_aggregates.sql > "$run_dir/source.sha256"
# Explicit production feature set: NO --test, NO pg_test.
cargo pgrx install --package pg_accel --pg-config "$PG_CONFIG" --no-default-features --features "pg$major" > "$run_dir/install.log" 2>&1
find "$prefix/lib" -maxdepth 2 -name 'pg_accel*' -type f -exec shasum -a 256 {} + > "$run_dir/installed-library.sha256"
pgdata="$run_dir/pgdata"
port=$((30100+major))
"$bindir/initdb" -D "$pgdata" --no-locale --encoding=UTF8 --auth=trust > "$run_dir/initdb.log" 2>&1
"$bindir/pg_ctl" -D "$pgdata" -l "$run_dir/server.log" -o "-p $port -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel" -w start > "$run_dir/start.log" 2>&1
trap '"$bindir/pg_ctl" -D "$pgdata" -m fast -w stop > "$run_dir/stop.log" 2>&1' EXIT
connection="host=$socket_dir port=$port dbname=postgres"
"$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION pg_accel' > "$run_dir/extension.log" 2>&1
"$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1 -c "DO \$\$ BEGIN IF EXISTS(SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='tests') THEN RAISE EXCEPTION 'pg_test functions in production module'; END IF; END \$\$;" > "$run_dir/production-identity.log" 2>&1
for budget in 128 256 DEFAULT; do
  "$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1 -c "SET pg_accel.resident_memory_budget_mb=$budget; SET pg_accel.kernel_timeout_ms=60000;" -f "$task_root/production-profile.sql" -f sql/tests/121_integer_where_aggregates.sql > "$run_dir/sql121-$budget.log" 2>&1
  printf 'PG%s unchanged SQL121 budget=%s PASS\n' "$major" "$budget"
done
"$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'SET pg_accel.resident_memory_budget_mb=64; SET pg_accel.kernel_timeout_ms=60000;' -f "$task_root/production-profile.sql" -f "$task_root/cold-target.sql" -f "$task_root/cold-check.sql" > "$run_dir/cold64.log" 2>&1
"$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'SET pg_accel.resident_memory_budget_mb=64; SET pg_accel.kernel_timeout_ms=60000;' -f "$task_root/production-profile.sql" -f "$task_root/warm64-subset.sql" > "$run_dir/warm64-subset.log" 2>&1
shasum -a 256 -c "$run_dir/source.sha256" > "$run_dir/source-after.log"
printf 'PG%s production SQL matrix PASS (shared-ledger pressure qualification separate)\n' "$major"
