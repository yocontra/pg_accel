#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/hosted-budget-20261018"
major=$1
stage=$2
case "$major" in 18) version=18.4;; 19) version=19beta1;; *) exit 2;; esac
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version"
bindir="$prefix/bin"
run_dir="$task_root/pg$major-scalar-proof-$stage"
test ! -e "$run_dir"
socket_dir="$task_root/scalar-s$major"
mkdir -p "$run_dir/cache" "$socket_dir"
export ACPP_APPDB_DIR="$run_dir/cache"
shasum -a 256 "$prefix/lib/pg_accel.dylib" > "$run_dir/installed-library.sha256"
shasum -a 256 -c "$task_root/task1-candidate11.sha256" > "$run_dir/source-before.log"
shasum -a 256 "$task_root/run-scalar-device-proof.sh" "$task_root/scalar-device-proof.sql" > "$run_dir/harness.sha256"
pgdata="$run_dir/pgdata"
port=$((30400+major))
"$bindir/initdb" -D "$pgdata" --no-locale --encoding=UTF8 --auth=trust > "$run_dir/initdb.log" 2>&1
"$bindir/pg_ctl" -D "$pgdata" -l "$run_dir/server.log" -o "-p $port -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel" -w start > "$run_dir/start.log" 2>&1
trap '"$bindir/pg_ctl" -D "$pgdata" -m fast -w stop > "$run_dir/stop.log" 2>&1' EXIT
connection="host=$socket_dir port=$port dbname=postgres"
"$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION pg_accel' -f "$task_root/scalar-device-proof.sql" > "$run_dir/result.log" 2>&1
shasum -a 256 -c "$run_dir/installed-library.sha256" > "$run_dir/module-after.log"
shasum -a 256 -c "$task_root/task1-candidate11.sha256" > "$run_dir/source-after.log"
printf 'PG%s scalar default-admission actual-device proof PASS\n' "$major"
