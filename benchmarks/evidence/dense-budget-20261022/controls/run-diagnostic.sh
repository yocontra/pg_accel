#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/hosted-budget-20261018"
major=${1:-19}
stage=${2:-baseline}
case "$major" in 18) version=18.4;; 19) version=19beta1;; *) exit 2;; esac
case "$stage" in baseline) stage_code=b;; instrumented) stage_code=i;; cold) stage_code=c;; *) exit 2;; esac
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version"
bindir="$prefix/bin"
run_dir="$task_root/pg$major-$stage"
if [ -e "$run_dir" ]; then
  printf 'Refusing to overwrite diagnostic stage: %s\n' "$run_dir" >&2
  exit 2
fi
socket_dir="$task_root/s$major$stage_code"
mkdir -p "$socket_dir" "$run_dir/cache"
export ACPP_APPDB_DIR="$run_dir/cache"
pgdata="$run_dir/pgdata"
port=$((29900+major))
shasum -a 256 sql/tests/121_integer_where_aggregates.sql \
  pg_accel/src/gpu/grouped_agg.rs pg_accel/src/engine/ffi/planner_hooks/generic_groupagg.rs \
  pg_accel/src/engine/executor/agg/descriptor.rs pg_accel/src/engine/residency/store.rs \
  "$task_root/sql121-original.sql" "$task_root/sql121-instrumented.sql" \
  "$task_root/cold-target.sql" "$task_root/profile.sql" "$task_root/run-diagnostic.sh" \
  > "$run_dir/source.sha256"
cmp sql/tests/121_integer_where_aggregates.sql "$task_root/sql121-original.sql"
find "$prefix/lib" -maxdepth 2 -name 'pg_accel*' -type f -exec shasum -a 256 {} + > "$run_dir/installed-library.sha256"
"$bindir/initdb" -D "$pgdata" --no-locale --encoding=UTF8 --auth=trust > "$run_dir/initdb.log" 2>&1
"$bindir/pg_ctl" -D "$pgdata" -l "$run_dir/server.log" \
  -o "-p $port -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel" \
  -w start > "$run_dir/start.log" 2>&1
trap '"$bindir/pg_ctl" -D "$pgdata" -m fast -w stop > "$run_dir/stop.log" 2>&1' EXIT
"$bindir/psql" -X -v ON_ERROR_STOP=1 -h "$socket_dir" -p "$port" -d postgres \
  -c 'CREATE EXTENSION pg_accel' > "$run_dir/extension.log" 2>&1
case "$stage" in
  baseline) input="$task_root/sql121-original.sql";;
  instrumented) input="$task_root/sql121-instrumented.sql";;
  cold) input="$task_root/cold-target.sql";;
esac
printf 'major\tstage\tbudget_mib\tpsql_exit\n' > "$run_dir/results.tsv"
for budget in 64 128 256; do
  set +e
  "$bindir/psql" -X -v ON_ERROR_STOP=1 -h "$socket_dir" -p "$port" -d postgres \
    -c "SET pg_accel.resident_memory_budget_mb=$budget; SET pg_accel.kernel_timeout_ms=60000;" \
    -f "$task_root/profile.sql" -f "$input" > "$run_dir/budget-$budget.log" 2>&1
  result=$?
  set -e
  printf '%s\t%s\t%s\t%s\n' "$major" "$stage" "$budget" "$result" | tee -a "$run_dir/results.tsv"
done
shasum -a 256 -c "$run_dir/source.sha256" > "$run_dir/source-after.log"
# This process is a diagnostic orchestrator, not a test pass: per-case failures
# are retained verbatim and recorded in results.tsv, never erased by exit 0.
