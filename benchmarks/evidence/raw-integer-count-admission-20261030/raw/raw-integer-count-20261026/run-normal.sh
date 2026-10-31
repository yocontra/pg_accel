#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/raw-integer-count-20261026"
major=${1:?major required}
stage=${2:?build stage required}
case "$major" in 18) version=18.4;; 19) version=19beta1;; *) exit 2;; esac
case "$stage" in ''|*[!a-zA-Z0-9_-]*) exit 2;; esac
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version"
build_dir="$task_root/build$major-$stage"
run_dir="$task_root/normal$major-$stage"
socket_dir="$PWD/.codex/scratch/raw$major"
test ! -e "$run_dir"
test ! -e "$socket_dir"
test -z "${PGOPTIONS:-}"
test "$(<"$build_dir/terminal.log")" = terminal=0
shasum -a256 -c "$build_dir/source.sha256"
shasum -a256 -c "$build_dir/module.sha256"
mkdir "$run_dir" "$socket_dir"
export ACPP_APPDB_DIR="$task_root/cache-production$major"
export PG_CONFIG="$prefix/bin/pg_config"
export PG_ACCEL_PG_MAJOR="$major"
ulimit -n 4096
shasum -a256 "$task_root/run-normal.sh" > "$run_dir/harness.sha256"
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
 exit "$task_rc"
}
trap cleanup EXIT
"$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -l "$run_dir/server.log" -w -t60 \
 -o "-p 335$major -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel -c shared_buffers=16GB -c max_worker_processes=16" start > "$run_dir/start.log" 2>&1
connection="host=$socket_dir port=335$major dbname=postgres connect_timeout=10"
"$prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION pg_accel; SELECT extname FROM pg_extension ORDER BY extname' > "$run_dir/extensions.log" 2>&1
export PG_ACCEL_TEST_CONNECTION="$connection"
export PG_ACCEL_TEST_PG_MAJOR="$major"
export RUST_TEST_THREADS=1
test_name=plan_shape_raw_integer_count_all_fixed_width_types
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 15m cargo test -p pg_accel_bench --features integration_tests "$test_name" -- --nocapture --test-threads=1 > "$run_dir/test.log" 2>&1
rg -q 'test result: ok. 1 passed; 0 failed;' "$run_dir/test.log"
