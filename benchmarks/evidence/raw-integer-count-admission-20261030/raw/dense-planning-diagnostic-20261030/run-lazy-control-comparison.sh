#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/dense-planning-diagnostic-20261030"
prior="$PWD/.codex/scratch/raw-integer-count-20261026"
block=${1:?ABBA block 1 through 4 required}
lane=${2:?exact control workload required}
case "$block" in 1|4) arm=old;; 2|3) arm=new;; *) exit 2;; esac
case "$lane" in dense_int2_count_control|dense_int4_count_control|dense_int8_count_control) ;; *) exit 2;; esac
current_build="$task_root/lazy-production18"
if test "$arm" = old; then
 build_dir="$prior/control-baseline-build2"
 source_root="$prior/baseline-ce24"
 prefix="$build_dir/postgres18"
 test "$(<"$build_dir/terminal.log")" = terminal=0
else
 build_dir="$current_build"
 source_root="$PWD"
 prefix="$current_build/postgres"
 test "$(<"$build_dir/terminal.log")" = terminal=0
fi
run_dir="$task_root/control-comparison-$block-$lane"
socket_dir="$PWD/.codex/scratch/rawcontrol18"
test ! -e "$run_dir"
test ! -e "$socket_dir"
test -z "${PGOPTIONS:-}"
(cd "$source_root" && shasum -a256 -c "$build_dir/source.sha256")
shasum -a256 -c "$build_dir/module.sha256"
test "$(<"$current_build/terminal.log")" = terminal=0
bash "$task_root/verify-retained-bench.sh"
shasum -a256 -c "$current_build/source.sha256"
mkdir "$run_dir" "$socket_dir"
export ACPP_APPDB_DIR="$task_root/cache-performance18"
export PG_CONFIG="$prefix/bin/pg_config"
export PG_ACCEL_PG_MAJOR=18
export PG_ACCEL_EXPECTED_DYLIB="$prefix/lib/pg_accel.dylib"
ulimit -n 4096
shasum -a256 "$task_root/run-lazy-control-comparison.sh" "$task_root/verify-retained-bench.sh" target/release/pg_accel_bench > "$run_dir/harness.sha256"
printf '%s\n' "$lane" > "$run_dir/lane.txt"
printf '%s\n' "$build_dir" > "$run_dir/build-dir.txt"
bash "$task_root/verify-retained-bench.sh" > "$run_dir/retained-bench-before.log"
printf 'block=%s arm=%s\n' "$block" "$arm" > "$run_dir/comparison.txt"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 2m "$prefix/bin/initdb" -D "$run_dir/pgdata" --no-locale --encoding=UTF8 --auth=trust > "$run_dir/initdb.log" 2>&1
cleanup() {
 task_rc=$?
 trap - EXIT
 set +e
 /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=5s 40s "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -m fast -w -t30 stop > "$run_dir/stop.log" 2>&1 ||
  /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=5s 40s "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -m immediate -w -t30 stop >> "$run_dir/stop.log" 2>&1
 /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=5s 10s "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" status > "$run_dir/status.log" 2>&1
 test "$?" = 3 || task_rc=1
 test ! -e "$run_dir/pgdata/postmaster.pid" || task_rc=1
 (cd "$source_root" && shasum -a256 -c "$build_dir/source.sha256") > "$run_dir/source-after.log" || task_rc=1
 shasum -a256 -c "$build_dir/module.sha256" > "$run_dir/module-after.log" || task_rc=1
 shasum -a256 -c "$run_dir/harness.sha256" > "$run_dir/harness-after.log" || task_rc=1
 bash "$task_root/verify-retained-bench.sh" > "$run_dir/retained-bench-after.log" || task_rc=1
 rmdir "$socket_dir" || task_rc=1
 printf 'terminal=%s\n' "$task_rc" > "$run_dir/terminal.log"
 # Preserve failed runs too. The checker requires every mandatory receipt and
 # terminal zero; this seal only records the exact files that were produced.
 if ! (
  cd "$run_dir" || exit 1
  {
   for receipt in report.json lane.txt build-dir.txt retained-bench-before.log retained-bench-after.log comparison.txt terminal.log source-after.log module-after.log harness-after.log harness.sha256 bench.log device.log extensions.log status.log stop.log; do
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
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 90s "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -l "$run_dir/server.log" -w -t60 \
 -o "-p 33618 -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel -c shared_buffers=16GB -c max_worker_processes=16" start > "$run_dir/start.log" 2>&1
connection="host=$socket_dir port=33618 dbname=postgres connect_timeout=10"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 2m "$prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION pg_accel' > "$run_dir/extensions.log" 2>&1
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 2m "$prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'SELECT version(); SELECT extname FROM pg_extension ORDER BY extname; SELECT * FROM pg_accel_device_info(); SELECT * FROM pg_accel_device_limits()' > "$run_dir/device.log" 2>&1
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 20m target/release/pg_accel_bench run --connection "$connection" --workload "$lane" \
 --iterations 10 --warmup 5 --timing raw --cache-mode warm --realistic-gucs --format json \
 --artifacts-dir "$run_dir/artifacts" > "$run_dir/report.json" 2> "$run_dir/bench.log"
