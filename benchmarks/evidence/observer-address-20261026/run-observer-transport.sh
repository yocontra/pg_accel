#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/raw-integer-count-20261026"
major=${1:?major required}
case "$major" in 18) version=18.4;; 19) version=19beta1;; *) exit 2;; esac
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version"
run_dir="$task_root/observer-transport$major"
socket_dir="$PWD/.codex/scratch/obstrans$major"
test ! -e "$run_dir"
test ! -e "$socket_dir"
test -z "${PGOPTIONS:-}"
mkdir "$run_dir" "$socket_dir"
shasum -a256 Cargo.lock pg_accel_bench/Cargo.toml pg_accel_bench/src/integration_connection.rs "$task_root/run-observer-transport.sh" "$prefix/bin/postgres" > "$run_dir/inputs.sha256"
"$prefix/bin/initdb" -D "$run_dir/pgdata" --no-locale --encoding=UTF8 --auth=trust > "$run_dir/initdb.log" 2>&1
cleanup() {
 task_rc=$?
 trap - EXIT
 set +e
 "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -m fast -w -t30 stop > "$run_dir/stop.log" 2>&1
 "$prefix/bin/pg_ctl" -D "$run_dir/pgdata" status > "$run_dir/status.log" 2>&1
 test "$?" = 3 || task_rc=1
 test ! -e "$run_dir/pgdata/postmaster.pid" || task_rc=1
 shasum -a256 -c "$run_dir/inputs.sha256" > "$run_dir/inputs-after.log" || task_rc=1
 rmdir "$socket_dir" || task_rc=1
 printf 'terminal=%s\n' "$task_rc" > "$run_dir/terminal.log"
 exit "$task_rc"
}
trap cleanup EXIT
# Identity-only test: no extension, preload, GPU, or installation changes.
"$prefix/bin/pg_ctl" -D "$run_dir/pgdata" -l "$run_dir/server.log" -w -t60 \
 -o "-p 337$major -k $socket_dir -c listen_addresses='127.0.0.1,::1'" start > "$run_dir/start.log" 2>&1
export PG_ACCEL_TEST_PG_MAJOR="$major"
export RUST_TEST_THREADS=1
ulimit -n 4096
for transport in ipv4 ipv6 unix; do
 case "$transport" in ipv4) host=127.0.0.1;; ipv6) host=::1;; unix) host="$socket_dir";; esac
 export PG_ACCEL_TEST_CONNECTION="host=$host port=337$major dbname=postgres connect_timeout=10"
 /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 10m cargo test -p pg_accel_bench --features integration_tests observer_registration_uses_numeric_host_and_preserves_socket_identity -- --nocapture --test-threads=1 > "$run_dir/$transport.log" 2>&1
 rg -q 'test result: ok. 1 passed; 0 failed;' "$run_dir/$transport.log"
done
