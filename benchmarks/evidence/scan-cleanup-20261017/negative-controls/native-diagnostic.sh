#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
run_root="$PWD/.codex/scratch/scan-cleanup-20261017"
bindir="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4/bin"
pgdata="$run_root/device-proof-pgdata18"
socket_dir="$run_root/socket-proof18"
export ACPP_APPDB_DIR="$run_root/cache-native-diagnostic"
mkdir -p "$ACPP_APPDB_DIR"
test ! -e "$pgdata/postmaster.pid"
"$bindir/pg_ctl" -D "$pgdata" -l "$run_root/native-diagnostic-server.log" \
    -o "-p 29818 -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel" \
    -w start > "$run_root/native-diagnostic-start.log" 2>&1
trap '"$bindir/pg_ctl" -D "$pgdata" -m fast -w stop > "$run_root/native-diagnostic-stop.log" 2>&1' EXIT
"$bindir/psql" -X -h "$socket_dir" -p 29818 -d pgrx_tests \
    -f "$run_root/native-diagnostic.sql" > "$run_root/native-diagnostic.log" 2>&1
