#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/raw-integer-count-20261026"
major=${1:?major required}
stage=${2:?qualified final build stage required}
run_stage=${3:-$stage}
case "$major" in 18) version=18.4;; 19) version=19beta1;; *) exit 2;; esac
case "$stage" in ''|*[!a-zA-Z0-9_-]*) exit 2;; esac
case "$run_stage" in ''|*[!a-zA-Z0-9_-]*) exit 2;; esac
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version"
build_dir="$task_root/build$major-$stage"
run_dir="$task_root/selection$major-$run_stage"
socket_dir="$PWD/.codex/scratch/rawselection$major"
test ! -e "$run_dir"
test ! -e "$socket_dir"
test -z "${PGOPTIONS:-}"
test "$(<"$build_dir/final-terminal.log")" = terminal=0
shasum -a256 -c "$build_dir/source.sha256"
shasum -a256 -c "$build_dir/module.sha256"
mkdir "$run_dir" "$socket_dir"
printf '%s\n' "$build_dir" > "$run_dir/build-dir.txt"
export ACPP_APPDB_DIR="$task_root/cache-production$major"
export PG_CONFIG="$prefix/bin/pg_config"
export PG_ACCEL_PG_MAJOR="$major"
ulimit -n 4096
shasum -a256 "$task_root/run-selection.sh" "$task_root/production-selection.sql" > "$run_dir/harness.sha256"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 2m "$prefix/bin/initdb" -D "$run_dir/pgdata" --no-locale --encoding=UTF8 --auth=trust > "$run_dir/initdb.log" 2>&1
cleanup() {
 task_rc=$?
 trap - EXIT
 set +e
 /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=5s 40s "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -m fast -w -t30 stop > "$run_dir/stop.log" 2>&1
 if test "$?" != 0; then
  /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=5s 40s "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -m immediate -w -t30 stop >> "$run_dir/stop.log" 2>&1 || task_rc=1
 fi
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
 -o "-p 338$major -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel -c shared_buffers=16GB -c max_worker_processes=16" start > "$run_dir/start.log" 2>&1
connection="host=$socket_dir port=338$major dbname=postgres connect_timeout=10"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 2m "$prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION pg_accel' > "$run_dir/extensions.log" 2>&1
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 20m "$prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -f "$task_root/production-selection.sql" > "$run_dir/result.log" 2>&1
rg -q 'COUNT_SELECTION_COMPLETE' "$run_dir/result.log"

