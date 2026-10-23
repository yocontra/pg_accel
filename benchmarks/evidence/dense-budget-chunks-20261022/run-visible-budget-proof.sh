#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/dense-budget-chunks-20261022"
major=$1
stage=$2
provenance=${3:?absolute frozen pg_test installed-build provenance required}
case "$major" in 18) version=18.4;; 19) version=19beta1;; *) exit 2;; esac
case "$stage" in ''|*[!a-zA-Z0-9_-]*) echo 'invalid stage' >&2; exit 2;; esac
case "$provenance" in /*) ;; *) echo 'absolute provenance required' >&2; exit 2;; esac
test -z "${PGOPTIONS:-}"
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version"
bindir="$prefix/bin"
run_dir="$task_root/visible-pg$major-$stage"
socket_dir="$task_root/vs$major"
test ! -e "$run_dir"
test ! -e "$socket_dir"
test -f "$provenance/source.sha256"
test -f "$provenance/installed-library.sha256"
test "$(wc -l < "$provenance/source.sha256" | tr -d ' ')" = 9
source_paths=(
  pg_accel/src/engine/executor/agg/descriptor.rs
  pg_accel/src/engine/executor/agg/mod.rs
  pg_accel/src/gpu/grouped_agg.rs
  pg_accel/src/engine/ffi/planner_hooks/generic_groupagg.rs
  pg_accel/src/engine/ffi/planner_hooks/shape/cost.rs
  pg_accel/src/engine/ffi/planner_hooks/shape/mod.rs
  pg_accel/src/engine/residency/store.rs
  pg_accel/src/engine/residency/mod.rs
  pg_accel/src/tests/phase2_cache.rs
)
for source_path in "${source_paths[@]}"; do
  test "$(awk -v wanted="$source_path" '$2 == wanted && NF == 2 {n++} END {print n+0}' "$provenance/source.sha256")" = 1
done
mkdir -p "$run_dir/cache" "$socket_dir"
export ACPP_APPDB_DIR="$run_dir/cache"
(cd "$provenance" && shasum -a 256 -c source.sha256) > "$run_dir/source-before.log"
cp "$provenance/source.sha256" "$run_dir/installed-source.sha256"
cp "$provenance/installed-library.sha256" "$run_dir/installed-library.sha256"
test "$(wc -l < "$run_dir/installed-library.sha256" | tr -d ' ')" = 1
test "$(awk '{print $2}' "$run_dir/installed-library.sha256")" = "$prefix/lib/pg_accel.dylib"
shasum -a 256 -c "$run_dir/installed-library.sha256" > "$run_dir/module-before.log"
shasum -a 256 -c "$task_root/visible-harness.sha256" > "$run_dir/harness-before.log"
cp "$task_root/visible-harness.sha256" "$run_dir/harness.sha256"
printf 'Expected installed pg_test build provenance=%s\n' "$provenance" > "$run_dir/provenance.log"
pgdata="$run_dir/pgdata"
port=$((32600+major))
cleanup() {
  local result=$?
  trap - EXIT
  set +e
  "$bindir/pg_ctl" -D "$pgdata" -m fast -w -t 30 stop > "$run_dir/stop.log" 2>&1
  local stopped=$?
  "$bindir/pg_ctl" -D "$pgdata" status > "$run_dir/final-status.log" 2>&1
  local status=$?
  if [[ $stopped -ne 0 || $status -ne 3 || -e "$pgdata/postmaster.pid" ]]; then result=1; fi
  shasum -a 256 -c "$run_dir/installed-library.sha256" > "$run_dir/module-after.log" || result=1
  shasum -a 256 -c "$run_dir/harness.sha256" > "$run_dir/harness-after.log" || result=1
  (cd "$provenance" && shasum -a 256 -c source.sha256) > "$run_dir/source-after.log" || result=1
  rmdir "$socket_dir" || result=1
  printf 'visible budget proof PG%s terminal=%s\n' "$major" "$result" > "$run_dir/terminal.log"
  exit "$result"
}
"$bindir/initdb" -D "$pgdata" --no-locale --encoding=UTF8 --auth=trust > "$run_dir/initdb.log" 2>&1
trap cleanup EXIT
"$bindir/pg_ctl" -D "$pgdata" -l "$run_dir/server.log" -w -t 30 \
  -o "-p $port -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel" start > "$run_dir/start.log" 2>&1
connection="host=$socket_dir port=$port dbname=postgres connect_timeout=10"
python3 "$task_root/qualification/bounded-command.py" "$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1 \
  -c 'CREATE EXTENSION pg_accel' -f "$task_root/visible-budget-proof.sql" > "$run_dir/result.log" 2>&1
printf 'PG%s visible budget proof PASS\n' "$major"
