#!/usr/bin/env bash
set -euo pipefail
repo=/Users/contra/Projects/pg_accel
task="$repo/.codex/scratch/dense-planning-diagnostic-20261030"
prior="$repo/.codex/scratch/raw-integer-count-20261026"
run="$task/run3"
timeout=/opt/homebrew/bin/gtimeout
test ! -e "$run"
test -z "${PGOPTIONS:-}"
mkdir "$run"
export ACPP_APPDB_DIR="$run/jit-cache"
export PYTHONDONTWRITEBYTECODE=1
ulimit -n 4096
active_prefix= active_data= active_socket= stage=preflight
shasum -a256 "$task/run-stable-fixture.sh" "$task/probe-stable-fixture.py" "$task/stable-fixture-proposal.md" "$prior/control-cost-comparison-1-dense_int2_count_control/device.log" > "$run/harness.sha256"
shasum -a256 "$repo/.pgaccel/acpp/current/lib/libacpp-rt.dylib" \
 "$prior/control-baseline-build2/postgres18/lib/libpq.5.dylib" \
 "$repo/.codex/scratch/uniform-budget-live-20261012/postgres/18.4/lib/libpq.5.dylib" \
 "$task/production18/postgres/lib/libpq.5.dylib" \
 "$repo/.codex/scratch/uniform-budget-live-20261012/postgres/18.4/lib/pg_accel.dylib" \
 "$repo/.codex/scratch/uniform-budget-live-20261012/postgres/19beta1/lib/pg_accel.dylib" \
 "$repo/target/release/libpg_accel.dylib" "$repo/target/release/pg_accel_bench" > "$run/protected.sha256"
check_seals() {
 (cd "$repo" && shasum -a256 -c "$task/production18/source.sha256") || return 1
 (cd "$prior/baseline-ce24" && shasum -a256 -c "$prior/control-baseline-build2/source.sha256") || return 1
 shasum -a256 -c "$task/production18/module.sha256" || return 1
 shasum -a256 -c "$prior/control-baseline-build2/module.sha256" || return 1
 shasum -a256 -c "$run/harness.sha256" || return 1
 shasum -a256 -c "$run/protected.sha256" || return 1
}
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
cleanup() {
 local result=$?
 trap - EXIT
 set +e
 stop_owned || result=1
 check_seals > "$run/seals-after.log" 2>&1 || result=1
 printf 'stage=%s\nterminal=%s\n' "$stage" "$result" > "$run/terminal.log"
 exit "$result"
}
trap cleanup EXIT
test "$(cat "$prior/control-baseline-build2/terminal.log")" = terminal=0
test "$(cat "$task/production18/terminal.log")" = terminal=0
check_seals > "$run/seals-before.log" 2>&1
df -h "$repo" > "$run/disk-before.log"
for block in 1 2 3 4; do
 case "$block" in
  1|4) arm=old; active_prefix="$prior/control-baseline-build2/postgres18";;
  2|3) arm=new; active_prefix="$task/production18/postgres";;
 esac
 block_dir="$run/block$block-$arm"
 mkdir "$block_dir"
 active_data="$block_dir/pgdata"
 active_socket="$repo/.codex/scratch/dp18-20261030"
 test ! -e "$active_socket"
 mkdir "$active_socket"
 stage="init_block$block"
 "$timeout" --signal=TERM --kill-after=10s 2m "$active_prefix/bin/initdb" -D "$active_data" --no-locale --encoding=UTF8 --auth=trust > "$block_dir/initdb.log" 2>&1
 "$timeout" --signal=TERM --kill-after=10s 90s "$active_prefix/bin/pg_ctl" -D "$active_data" -l "$block_dir/server.log" -w -t60 -o "-p 34318 -k $active_socket -c listen_addresses='' -c shared_preload_libraries=pg_accel -c shared_buffers=16GB -c max_worker_processes=16" start > "$block_dir/start.log" 2>&1
 connection="host=$active_socket port=34318 dbname=postgres connect_timeout=10"
 "$timeout" --signal=TERM --kill-after=10s 2m "$active_prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION pg_accel; SELECT version(); SELECT * FROM pg_accel_device_info(); SELECT * FROM pg_accel_device_limits()' > "$block_dir/device.log" 2>&1
 stage="probe_block$block"
 "$timeout" --signal=TERM --kill-after=10s 10m python3 "$task/probe-stable-fixture.py" --connection "$connection" --libpq "$active_prefix/lib/libpq.5.dylib" --arm "$arm" --reference-device-log "$prior/control-cost-comparison-1-dense_int2_count_control/device.log" > "$block_dir/samples.jsonl" 2> "$block_dir/probe.stderr"
 set +e
 stop_owned
 stop_rc=$?
 set -e
 test "$stop_rc" = 0
 check_seals > "$block_dir/seals-after.log" 2>&1
 printf 'terminal=0\n' > "$block_dir/terminal.log"
done
stage=complete
