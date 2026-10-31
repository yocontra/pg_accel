#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/raw-integer-count-20261026"
build_stage=${1:?qualified final build stage required}
stage=${2:?fresh stage required}
lane=${3:?exact workload required}
case "$build_stage" in ''|*[!a-zA-Z0-9_-]*) exit 2;; esac
case "$stage" in ''|*[!a-zA-Z0-9_-]*) exit 2;; esac
case "$lane" in
 raw_int2_count_star|raw_int2_count_bool_filter|raw_int4_count_star|raw_int4_count_bool_filter|raw_int8_count_star|raw_int8_count_bool_filter|dense_int2_count_control|dense_int4_count_control|dense_int8_count_control) needs_h3=false;;
 h3_parent_count_int4_res0|h3_parent_count_bool_res0) needs_h3=true;;
 *) exit 2;;
esac
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4"
build_dir="$task_root/build18-$build_stage"
run_dir="$task_root/performance18-$stage"
socket_dir="$PWD/.codex/scratch/rawbench18"
test ! -e "$run_dir"
test ! -e "$socket_dir"
test -z "${PGOPTIONS:-}"
test "$(<"$build_dir/final-terminal.log")" = terminal=0
shasum -a256 -c "$build_dir/source.sha256"
shasum -a256 -c "$build_dir/module.sha256"
shasum -a256 -c "$build_dir/bench.sha256"
shasum -a256 -c "$build_dir/final-harness.sha256"
mkdir "$run_dir" "$socket_dir"
export ACPP_APPDB_DIR="$task_root/cache-performance18"
export PG_CONFIG="$prefix/bin/pg_config"
export PG_ACCEL_PG_MAJOR=18
ulimit -n 4096
shasum -a256 "$task_root/run-benchmark.sh" target/release/pg_accel_bench > "$run_dir/harness.sha256"
printf '%s\n' "$lane" > "$run_dir/lane.txt"
printf '%s\n' "$build_dir" > "$run_dir/build-dir.txt"
"$prefix/bin/initdb" -D "$run_dir/pgdata" --no-locale --encoding=UTF8 --auth=trust > "$run_dir/initdb.log" 2>&1
cleanup() {
 task_rc=$?
 trap - EXIT
 set +e
 "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -m fast -w -t30 stop > "$run_dir/stop.log" 2>&1
 "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" status > "$run_dir/status.log" 2>&1
 test "$?" = 3 || task_rc=1
 test ! -e "$run_dir/pgdata/postmaster.pid" || task_rc=1
 shasum -a256 -c "$build_dir/source.sha256" > "$run_dir/source-after.log" || task_rc=1
 shasum -a256 -c "$build_dir/module.sha256" > "$run_dir/module-after.log" || task_rc=1
 shasum -a256 -c "$run_dir/harness.sha256" > "$run_dir/harness-after.log" || task_rc=1
 rmdir "$socket_dir" || task_rc=1
 printf 'terminal=%s\n' "$task_rc" > "$run_dir/terminal.log"
 # Preserve failed runs too. The checker requires every mandatory receipt and
 # terminal zero; this seal only records the exact files that were produced.
 if ! (
  cd "$run_dir" || exit 1
  {
   for receipt in report.json lane.txt build-dir.txt terminal.log source-after.log module-after.log harness-after.log harness.sha256 bench.log device.log extensions.log status.log stop.log; do
    test ! -f "$receipt" || printf '%s\n' "$receipt"
   done
   if test -d artifacts; then rg --files artifacts; fi
  } | LC_ALL=C sort -u | while IFS= read -r receipt; do shasum -a256 "$receipt" || exit 1; done
 ) > "$run_dir/evidence.sha256"; then
  task_rc=1
  printf 'terminal=1\n' > "$run_dir/terminal.log"
 fi
 exit "$task_rc"
}
trap cleanup EXIT
"$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -l "$run_dir/server.log" -w -t60 \
 -o "-p 33618 -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel -c shared_buffers=16GB -c max_worker_processes=16" start > "$run_dir/start.log" 2>&1
connection="host=$socket_dir port=33618 dbname=postgres connect_timeout=10"
"$prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION pg_accel' > "$run_dir/extensions.log" 2>&1
if test "$needs_h3" = true; then
 "$prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION h3' >> "$run_dir/extensions.log" 2>&1
fi
"$prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'SELECT version(); SELECT extname FROM pg_extension ORDER BY extname; SELECT * FROM pg_accel_device_info(); SELECT * FROM pg_accel_device_limits()' > "$run_dir/device.log" 2>&1
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 20m target/release/pg_accel_bench run --connection "$connection" --workload "$lane" \
 --iterations 10 --warmup 5 --timing raw --cache-mode warm --realistic-gucs --format json \
 --artifacts-dir "$run_dir/artifacts" > "$run_dir/report.json" 2> "$run_dir/bench.log"
