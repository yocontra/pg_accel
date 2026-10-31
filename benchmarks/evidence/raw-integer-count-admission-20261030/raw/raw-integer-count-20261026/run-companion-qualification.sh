#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/raw-integer-count-20261026"
build_dir="$task_root/calibration-companion-build1"
source_root="$task_root/calibration-diagnostic-ce24"
prefix="$build_dir/postgres18"
run_dir="$task_root/companion-qualification1"
socket_dir="$PWD/.codex/scratch/countdiag18"
test ! -e "$run_dir"
test ! -e "$socket_dir"
test -z "${PGOPTIONS:-}"
test "$(<"$build_dir/terminal.log")" = terminal=0
(cd "$source_root" && shasum -a256 -c "$build_dir/source.sha256")
shasum -a256 -c "$build_dir/module.sha256"
mkdir "$run_dir" "$socket_dir"
printf '%s\n' "$build_dir" > "$run_dir/build-dir.txt"
shasum -a256 "$task_root/run-companion-qualification.sh" "$task_root/qualify-companion.sql" "$task_root/qualify-companion.py" "$task_root/dense-count-calibration.py" > "$run_dir/harness.sha256"
export ACPP_APPDB_DIR="$run_dir/jit-cache"
ulimit -n 4096
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
 (cd "$source_root" && shasum -a256 -c "$build_dir/source.sha256") > "$run_dir/source-after.log" || task_rc=1
 shasum -a256 -c "$build_dir/module.sha256" > "$run_dir/module-after.log" || task_rc=1
 shasum -a256 -c "$run_dir/harness.sha256" > "$run_dir/harness-after.log" || task_rc=1
 rmdir "$socket_dir" || task_rc=1
 printf 'terminal=%s\n' "$task_rc" > "$run_dir/terminal.log"
 exit "$task_rc"
}
trap cleanup EXIT
"$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -l "$run_dir/server.log" -w -t60 \
 -o "-p 33818 -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel -c shared_buffers=16GB -c max_worker_processes=16" start > "$run_dir/start.log" 2>&1
connection="host=$socket_dir port=33818 dbname=postgres connect_timeout=10"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 2m "$prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION pg_accel' > "$run_dir/extensions.log" 2>&1
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 10m "$prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -v "artifact_dir=$run_dir" -f "$task_root/qualify-companion.sql" > "$run_dir/result.log" 2>&1
rg -q 'UNTIMED_COMPANION_QUALIFICATION_COMPLETE' "$run_dir/result.log"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 1m python3 "$task_root/qualify-companion.py" "$run_dir" > "$run_dir/adapter.log" 2>&1
rg -q 'UNTIMED_ADAPTER_PASS' "$run_dir/adapter.log"
