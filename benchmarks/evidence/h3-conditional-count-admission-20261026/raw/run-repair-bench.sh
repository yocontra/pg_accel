#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/h3-conditional-count-20261026"
build_stage=${1:?qualified build stage required}
stage=${2:?fresh stage required}
lane=${3:?typed workload required}
case "$build_stage" in ''|*[!a-zA-Z0-9_-]*) exit 2;; esac
case "$stage" in ''|*[!a-zA-Z0-9_-]*) exit 2;; esac
case "$lane" in h3_parent_count_int4_res0|h3_parent_count_int4_res1|h3_parent_count_bool_res0|h3_parent_count_bool_res1|h3_parent_count_star_filter_res0|h3_parent_count_star_filter_res1|h3_parent_count_int4_filter_res0|h3_parent_count_int4_filter_res1|h3_parent_count_bool_filter_res0|h3_parent_count_bool_filter_res1) ;; *) exit 2;; esac
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4"
run_dir="$task_root/repair-performance18-$stage"
socket_dir="$PWD/.codex/scratch/h3rbench18"
test ! -e "$run_dir"
test ! -e "$socket_dir"
test -z "${PGOPTIONS:-}"
test "$(<"$task_root/repair-production-build18-$build_stage/terminal.log")" = terminal=0
shasum -a256 -c "$task_root/repair-production-build18-$build_stage/source.sha256"
shasum -a256 -c "$task_root/repair-production-build18-$build_stage/module.sha256"
shasum -a256 -c "$task_root/repair-production-build18-$build_stage/bench.sha256"
mkdir -p "$run_dir" "$socket_dir" "$task_root/cache-production-repair18"
export ACPP_APPDB_DIR="$task_root/cache-production-repair18"
export PG_CONFIG="$prefix/bin/pg_config"
export PG_ACCEL_PG_MAJOR=18
ulimit -n 4096
shasum -a256 target/release/pg_accel_bench "$task_root/run-repair-bench.sh" > "$run_dir/harness.sha256"
"$prefix/bin/initdb" -D "$run_dir/pgdata" --no-locale --encoding=UTF8 --auth=trust > "$run_dir/initdb.log" 2>&1
cleanup() {
  task_rc=$?
  trap - EXIT
  set +e
  "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -m fast -w -t30 stop > "$run_dir/stop.log" 2>&1
  "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" status > "$run_dir/status.log" 2>&1
  test "$?" = 3 || task_rc=1
  test ! -e "$run_dir/pgdata/postmaster.pid" || task_rc=1
  shasum -a256 -c "$task_root/repair-production-build18-$build_stage/source.sha256" > "$run_dir/source-after.log" || task_rc=1
  shasum -a256 -c "$task_root/repair-production-build18-$build_stage/module.sha256" > "$run_dir/module-after.log" || task_rc=1
  shasum -a256 -c "$run_dir/harness.sha256" > "$run_dir/harness-after.log" || task_rc=1
  rmdir "$socket_dir" || task_rc=1
  printf 'terminal=%s\n' "$task_rc" > "$run_dir/terminal.log"
  exit "$task_rc"
}
trap cleanup EXIT
"$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -l "$run_dir/server.log" -w -t60 \
  -o "-p 33018 -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel -c shared_buffers=16GB -c max_worker_processes=16" start > "$run_dir/start.log" 2>&1
connection="host=$socket_dir port=33018 dbname=postgres connect_timeout=10"
"$prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 \
  -c 'CREATE EXTENSION pg_accel; CREATE EXTENSION h3; SELECT version(); SELECT * FROM pg_accel_device_info(); SELECT * FROM pg_accel_device_limits()' > "$run_dir/device.log" 2>&1
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 20m target/release/pg_accel_bench run --connection "$connection" --workload "$lane" \
  --iterations 10 --warmup 5 --timing raw --cache-mode warm --realistic-gucs --format json \
  --artifacts-dir "$run_dir/artifacts" > "$run_dir/report.json" 2> "$run_dir/bench.log"
