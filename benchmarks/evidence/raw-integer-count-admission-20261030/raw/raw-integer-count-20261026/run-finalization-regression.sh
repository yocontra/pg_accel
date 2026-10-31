#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/raw-integer-count-20261026"
build_dir="$task_root/build18-hash-finalization1"
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4"
run_dir="$task_root/finalization-regression1"
socket_dir="$PWD/.codex/scratch/countsort18"
test ! -e "$run_dir"
test ! -e "$socket_dir"
test -z "${PGOPTIONS:-}"
test "$(<"$build_dir/final-terminal.log")" = terminal=0
shasum -a256 -c "$build_dir/source.sha256"
shasum -a256 -c "$build_dir/module.sha256"
mkdir "$run_dir" "$socket_dir"
shasum -a256 "$task_root/run-finalization-regression.sh" "$task_root/measure-finalization-regression.py" "$task_root/acquire-dense-count-validation.py" "$task_root/qualify-companion.py" "$task_root/dense-count-calibration.py" "$task_root/dense-count-training3/manifest.json" "$task_root/dense-count-validation1/validation-results.json" "$task_root/dense-count-validation1/n1000017-g4096-current.json" "$task_root/dense-count-validation1/n2000017-g4096-current.json" > "$run_dir/harness.sha256"
export ACPP_APPDB_DIR="$run_dir/jit-cache"
ulimit -n 4096
cleanup() {
 task_rc=$?
 trap - EXIT
 set +e
 if test -f "$run_dir/pgdata/PG_VERSION"; then
  if test -e "$run_dir/pgdata/postmaster.pid"; then
   /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=5s 40s "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -m fast -w -t30 stop > "$run_dir/stop.log" 2>&1
   if test "$?" != 0; then
    /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=5s 40s "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -m immediate -w -t30 stop >> "$run_dir/stop.log" 2>&1 || task_rc=1
   fi
  fi
  "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" status > "$run_dir/status.log" 2>&1
  test "$?" = 3 || task_rc=1
  test ! -e "$run_dir/pgdata/postmaster.pid" || task_rc=1
 fi
 shasum -a256 -c "$build_dir/source.sha256" > "$run_dir/source-after.log" || task_rc=1
 shasum -a256 -c "$build_dir/module.sha256" > "$run_dir/module-after.log" || task_rc=1
 shasum -a256 -c "$run_dir/harness.sha256" > "$run_dir/harness-after.log" || task_rc=1
 rmdir "$socket_dir" || task_rc=1
 vm_stat > "$run_dir/vm-after.log"
 df -h "$run_dir" > "$run_dir/disk-after.log"
 printf 'terminal=%s\n' "$task_rc" > "$run_dir/terminal.log"
 exit "$task_rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
vm_stat > "$run_dir/vm-before.log"
df -h "$run_dir" > "$run_dir/disk-before.log"
ps -axo pid,ppid,%cpu,%mem,command > "$run_dir/processes-before.log"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 2m "$prefix/bin/initdb" -D "$run_dir/pgdata" --no-locale --encoding=UTF8 --auth=trust > "$run_dir/initdb.log" 2>&1
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 90s "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -l "$run_dir/server.log" -w -t60 -o "-p 33921 -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel -c shared_buffers=16GB -c max_worker_processes=16" start > "$run_dir/start.log" 2>&1
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 2m "$prefix/bin/psql" "host=$socket_dir port=33921 dbname=postgres connect_timeout=10" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION pg_accel' > "$run_dir/extensions.log" 2>&1
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 10m python3 "$task_root/measure-finalization-regression.py" > "$run_dir/diagnostic.log" 2>&1
rg -q '^TWO_CELL_RAW_FINALIZATION_DIAGNOSTIC_COMPLETE$' "$run_dir/diagnostic.log"
