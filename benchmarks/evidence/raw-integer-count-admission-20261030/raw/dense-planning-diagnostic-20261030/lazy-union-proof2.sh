#!/usr/bin/env bash
set -euo pipefail
repo=/Users/contra/Projects/pg_accel
task_root="$repo/.codex/scratch/dense-planning-diagnostic-20261030"
qualified="$task_root/lazy-qualification2"
run_root="$task_root/lazy-union-proof2"
source_root="$repo"
timeout=/opt/homebrew/bin/gtimeout
test "$(cat "$qualified/terminal.log")" = terminal=0
test -z "${PGOPTIONS:-}"
test ! -e "$run_root"
mkdir "$run_root"
export ACPP_APPDB_DIR="$run_root/jit-cache"
ulimit -n 4096
active_prefix= active_data= active_socket= stage=preflight
shasum -a256 "$task_root/lazy-union-proof2.sh" "$task_root/lazy-union-proof.sql" > "$run_root/harness.sha256"
stop_owned() {
 local result=0
 if [[ -n "$active_data" && -d "$active_data" ]]; then
  "$timeout" --signal=TERM --kill-after=5s 40s "$active_prefix/bin/pg_ctl" -D "$active_data" -m fast -w -t30 stop > "${active_data%/pgdata}/stop.log" 2>&1 ||
   "$timeout" --signal=TERM --kill-after=5s 40s "$active_prefix/bin/pg_ctl" -D "$active_data" -m immediate -w -t30 stop > "${active_data%/pgdata}/stop-immediate.log" 2>&1 || true
  "$timeout" --signal=TERM --kill-after=5s 10s "$active_prefix/bin/pg_ctl" -D "$active_data" status > "${active_data%/pgdata}/status.log" 2>&1
  [[ $? == 3 ]] || result=1
  [[ ! -e "$active_data/postmaster.pid" ]] || result=1
 fi
 if [[ -n "$active_socket" && -d "$active_socket" ]]; then rmdir "$active_socket" || result=1; fi
 if [[ $result == 0 ]]; then active_data=; active_socket=; active_prefix=; fi
 return "$result"
}
check_seals() {
 (cd "$source_root" && shasum -a256 -c "$qualified/source.sha256") || return 1
 shasum -a256 -c "$qualified/harness.sha256" || return 1
 shasum -a256 -c "$run_root/harness.sha256" || return 1
 for major in 18 19; do
  shasum -a256 -c "$task_root/pgtests$major-lazy-raw2/pg-test-module.sha256" || return 1
  shasum -a256 -c "$task_root/pgtests$major-lazy-raw2/protected.sha256" || return 1
 done
}
cleanup() {
 local result=$?
 trap - EXIT
 set +e
 stop_owned || result=1
 check_seals > "$run_root/seals-after.log" || result=1
 printf 'stage=%s\nterminal=%s\n' "$stage" "$result" > "$run_root/terminal.log"
 exit "$result"
}
trap cleanup EXIT
check_seals > "$run_root/seals-before.log"
for major in 18 19; do
 build="$run_root/pg$major"
 mkdir "$build"
 active_prefix="$task_root/postgres$major"
 active_data="$build/pgdata"
 active_socket="$repo/.codex/scratch/leup$major-20261030"
 test ! -e "$active_socket"
 mkdir "$active_socket"
 stage="init_pg$major"
 "$timeout" --signal=TERM --kill-after=10s 2m "$active_prefix/bin/initdb" -D "$active_data" --no-locale --encoding=UTF8 --auth=trust > "$build/initdb.log" 2>&1
 "$timeout" --signal=TERM --kill-after=10s 90s "$active_prefix/bin/pg_ctl" -D "$active_data" -l "$build/server.log" -w -t60 -o "-p 344$major -k $active_socket -c listen_addresses='' -c shared_preload_libraries=pg_accel -c shared_buffers=16GB -c max_worker_processes=16" start > "$build/start.log" 2>&1
 connection="host=$active_socket port=344$major dbname=postgres connect_timeout=10"
 "$timeout" --signal=TERM --kill-after=10s 2m "$active_prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION pg_accel; SELECT extname,extversion FROM pg_extension ORDER BY extname' > "$build/extensions.log" 2>&1
 stage="mixed_union_pg$major"
 "$timeout" --signal=TERM --kill-after=10s 10m "$active_prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -f "$task_root/lazy-union-proof.sql" > "$build/mixed-union.log" 2>&1
 rg -q 'mixed_union_positive_coverage=PASS columns=5' "$build/mixed-union.log"
 set +e
 stop_owned
 stop_rc=$?
 set -e
 test "$stop_rc" = 0
 printf 'terminal=0\n' > "$build/terminal.log"
done
stage=complete
