#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
run_root="$PWD/.codex/scratch/join-status-20261018"
major=$1
version=18.4
if [ "$major" = 19 ]; then version=19beta1; fi
bindir="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version/bin"
pgdata="$run_root/device-proof-pgdata$major"
socket_dir="$run_root/socket-proof$major"
export ACPP_APPDB_DIR="$run_root/cache-proof$major"
test ! -e "$pgdata"
mkdir -p "$socket_dir" "$ACPP_APPDB_DIR"
"$bindir/initdb" -D "$pgdata" --no-locale --encoding=UTF8 --auth=trust > "$run_root/pg$major-device-proof-initdb.log" 2>&1
"$bindir/pg_ctl" -D "$pgdata" -l "$run_root/pg$major-device-proof-server.log" \
    -o "-p $((30000 + major)) -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel" \
    -w start > "$run_root/pg$major-device-proof-start.log" 2>&1
trap '"$bindir/pg_ctl" -D "$pgdata" -m fast -w stop > "$run_root/pg$major-device-proof-stop.log" 2>&1' EXIT
"$bindir/createdb" -h "$socket_dir" -p "$((30000 + major))" pgrx_tests
"$bindir/psql" -X -v ON_ERROR_STOP=1 -h "$socket_dir" -p "$((30000 + major))" -d pgrx_tests \
    -c 'CREATE EXTENSION pg_accel' > "$run_root/pg$major-device-proof-extension.log" 2>&1
"$bindir/psql" -X -h "$socket_dir" -p "$((30000 + major))" -d pgrx_tests \
    -f "$run_root/device-proof.sql" > "$run_root/pg$major-device-proof.log" 2>&1
shasum -a 256 -c "$run_root/candidate-source.sha256" > "$run_root/pg$major-device-proof-source-after.log"
if rg -n 'WARNING:|ERROR:|FATAL:' "$run_root/pg$major-device-proof.log" "$run_root/pg$major-device-proof-server.log"; then
    exit 1
fi
