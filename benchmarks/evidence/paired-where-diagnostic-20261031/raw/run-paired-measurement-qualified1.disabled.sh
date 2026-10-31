#!/usr/bin/env bash
set -euo pipefail
# Source draft. Only root's reviewed literal gate change may authorize execution.
RUN_AUTHORIZED=false
test "$RUN_AUTHORIZED" = true
repo=/Users/contra/Projects/pg_accel
cd "$repo"
task="$repo/.codex/scratch/hash-count-where-rust-20261030"
source "$task/paired-exit-status.sh"
run="$task/paired-run1"
qualified="$task/pgtests18-paired-correctness3"
qualified19="$task/pgtests19-paired-correctness3"
validation="$task/paired-validate7"
prefix="$task/postgres18"
timeout=/opt/homebrew/bin/gtimeout
test ! -e "$run"
test -z "${PGOPTIONS:-}"
test "$(tail -n1 "$qualified/terminal.log")" = terminal=0
test "$(tail -n1 "$qualified19/terminal.log")" = terminal=0
test "$(tail -n1 "$validation/terminal.log")" = terminal=0
mkdir -m700 "$run"
mkdir -m700 "$run/receipts"
export PGACCEL_PAIRED_RECEIPTS="$run/receipts"
export ACPP_APPDB_DIR="$run/jit-cache"
data="$run/pgdata"
socket="$repo/.codex/scratch/paired-20261030"
test ! -e "$socket"
mkdir -m700 "$socket"
stage=preflight
shasum -a256 "$0" "$task/paired-run.sql" "$task/check-paired.py" "$task/paired-exit-status.sh" "$qualified/terminal.log" "$qualified/source.sha256" "$qualified/pg-test-module.sha256" "$qualified/protected.sha256" "$qualified/harness.sha256" "$qualified19/terminal.log" "$qualified19/source.sha256" "$qualified19/pg-test-module.sha256" "$qualified19/protected.sha256" "$qualified19/harness.sha256" "$validation/terminal.log" "$validation/source.sha256" "$validation/harness.sha256" > "$run/harness.sha256"
seals() {
 shasum -a256 -c "$qualified/source.sha256" || return 1
 shasum -a256 -c "$qualified/pg-test-module.sha256" || return 1
 shasum -a256 -c "$qualified/protected.sha256" || return 1
 shasum -a256 -c "$qualified/harness.sha256" || return 1
 shasum -a256 -c "$qualified19/source.sha256" || return 1
 shasum -a256 -c "$qualified19/pg-test-module.sha256" || return 1
 shasum -a256 -c "$qualified19/protected.sha256" || return 1
 shasum -a256 -c "$qualified19/harness.sha256" || return 1
 shasum -a256 -c "$validation/source.sha256" || return 1
 shasum -a256 -c "$validation/harness.sha256" || return 1
 shasum -a256 -c "$run/harness.sha256" || return 1
}
cleanup() {
 primary_status=$?; primary_stage=$stage; secondary_status=0
 trap - EXIT; set +e
 if test -e "$data/PG_VERSION"; then
  test "$(cat "$data/PG_VERSION")" = 18 || secondary_status=1
  "$timeout" --signal=TERM --kill-after=5s 40s "$prefix/bin/pg_ctl" -D "$data" -m fast -w -t30 stop > "$run/stop.log" 2>&1 ||
   "$timeout" --signal=TERM --kill-after=5s 40s "$prefix/bin/pg_ctl" -D "$data" -m immediate -w -t30 stop > "$run/stop-immediate.log" 2>&1 || true
  "$timeout" --signal=TERM --kill-after=5s 10s "$prefix/bin/pg_ctl" -D "$data" status > "$run/status.log" 2>&1
  test "$?" = 3 || secondary_status=1
  test ! -e "$data/postmaster.pid" || secondary_status=1
 fi
 rmdir "$socket" || secondary_status=1
 seals > "$run/seals-after.log" || secondary_status=1
 # Strict verification is attempted even on failure, but never repairs or
 # deletes missing/truncated/partial records and never erases primary status.
 "$timeout" --signal=TERM --kill-after=5s 60s /usr/bin/python3 -I "$task/check-paired.py" "$run/receipts" > "$run/inventory-check.log" 2>&1 || secondary_status=1
 find "$run/receipts" -type f -exec shasum -a256 {} + > "$run/receipts.sha256" || secondary_status=1
 result=$(paired_exit_status "$primary_status" "$secondary_status")
 printf 'stage=%s\nprimary_status=%s\nsecondary_status=%s\nterminal=%s\n' "$primary_stage" "$primary_status" "$secondary_status" "$result" > "$run/terminal.log"
 exit "$result"
}
trap cleanup EXIT
seals > "$run/seals-before.log"
ulimit -n 4096
stage=init
"$timeout" --signal=TERM --kill-after=5s 90s "$prefix/bin/initdb" -D "$data" --no-locale --encoding=UTF8 --auth=trust > "$run/initdb.log" 2>&1
"$timeout" --signal=TERM --kill-after=5s 90s "$prefix/bin/pg_ctl" -D "$data" -l "$run/server.log" -w -t60 -o "-p 35418 -k $socket -c listen_addresses='' -c shared_preload_libraries=pg_accel" start > "$run/start.log" 2>&1
stage=paired
"$timeout" --signal=TERM --kill-after=30s 20m "$prefix/bin/psql" "host=$socket port=35418 dbname=postgres connect_timeout=10" -X -v ON_ERROR_STOP=1 -f "$task/paired-run.sql" > "$run/paired.log" 2>&1
stage=complete
