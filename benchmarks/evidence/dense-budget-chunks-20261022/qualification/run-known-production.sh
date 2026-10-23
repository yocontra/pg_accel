#!/usr/bin/env bash
# Known gates only: chunk/maximality and exact impossible-footprint controls remain separate.
set -euo pipefail
ulimit -n 4096
cd /Users/contra/Projects/pg_accel
root="$PWD/.codex/scratch/dense-budget-chunks-20261022/qualification"
major=${1:?PG major required}
stage=${2:?fresh stage required}
provenance=${3:?absolute immutable installed-build provenance directory required}
case "$major" in 18) version=18.4;; 19) version=19beta1;; *) exit 2;; esac
case "$stage" in ''|*[!a-zA-Z0-9_-]*) echo 'invalid stage' >&2; exit 2;; esac
case "$provenance" in /*) ;; *) echo 'absolute provenance required' >&2; exit 2;; esac
test -z "${PGOPTIONS:-}"
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version"
bindir="$prefix/bin"
run_dir="$root/pg$major-$stage"
# Keep Unix socket path below PostgreSQL's platform limit.
socket_dir="$PWD/.codex/scratch/dense-budget-chunks-20261022/qs$major"
test ! -e "$run_dir"
test ! -e "$socket_dir"
test -f "$provenance/source.sha256"
test -f "$provenance/installed-library.sha256"
test -f "$root/harness.sha256"
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
test "$(wc -l < "$provenance/source.sha256" | tr -d ' ')" = 9
for source_path in "${source_paths[@]}"; do
    test "$(awk -v wanted="$source_path" '$2 == wanted && NF == 2 {n++} END {print n+0}' "$provenance/source.sha256")" = 1
done
mkdir -p "$run_dir/cache" "$socket_dir"
export ACPP_APPDB_DIR="$run_dir/cache"
pgdata="$run_dir/pgdata"
port=$((32500+major))
shasum -a 256 -c "$root/harness.sha256" > "$run_dir/harness-before.log"
(cd "$provenance" && shasum -a 256 -c source.sha256) > "$run_dir/installed-source-check.log"
cp "$provenance/source.sha256" "$run_dir/installed-source.sha256"
cp "$provenance/installed-library.sha256" "$run_dir/installed-library.sha256"
# Require the receipt to identify precisely this prefix's single loaded module.
test "$(wc -l < "$run_dir/installed-library.sha256" | tr -d ' ')" = 1
test "$(awk '{print $2}' "$run_dir/installed-library.sha256")" = "$prefix/lib/pg_accel.dylib"
shasum -a 256 -c "$run_dir/installed-library.sha256" > "$run_dir/module-before.log"
test "$(shasum -a 256 sql/tests/121_integer_where_aggregates.sql | awk '{print $1}')" = d9732308961e22a96aaf9a26262cb11e69db584a4ad605ba01d2b53b9be701d0
shasum -a 256 sql/tests/121_integer_where_aggregates.sql > "$run_dir/sql121.sha256"
printf 'Installed provenance=%s; worktree source is not substituted for installed identity.\n' "$provenance" > "$run_dir/provenance.log"
"$bindir/initdb" -D "$pgdata" --no-locale --encoding=UTF8 --auth=trust > "$run_dir/initdb.log" 2>&1
cleanup() {
    local result=$?
    trap - EXIT
    set +e
    "$bindir/pg_ctl" -D "$pgdata" -m fast -w -t 30 stop > "$run_dir/stop.log" 2>&1
    local stopped=$?
    "$bindir/pg_ctl" -D "$pgdata" status > "$run_dir/final-status.log" 2>&1
    local status=$?
    if [ "$stopped" -ne 0 ] || [ "$status" -ne 3 ] || [ -e "$pgdata/postmaster.pid" ]; then
        echo 'owned cluster cleanup not confirmed' >&2
        result=1
    fi
    shasum -a 256 -c "$run_dir/installed-library.sha256" > "$run_dir/module-after.log" || result=1
    shasum -a 256 -c "$root/harness.sha256" > "$run_dir/harness-after.log" || result=1
    (cd "$provenance" && shasum -a 256 -c source.sha256) > "$run_dir/installed-source-after.log" || result=1
    printf 'KNOWN_GATES_TERMINAL=%s; full chunk qualification not claimed\n' "$result" > "$run_dir/terminal.log"
    exit "$result"
}
trap cleanup EXIT
"$bindir/pg_ctl" -D "$pgdata" -l "$run_dir/server.log" -w -t 30 \
    -o "-p $port -k $socket_dir -c listen_addresses='' -c shared_preload_libraries=pg_accel" start > "$run_dir/start.log" 2>&1
connection="host=$socket_dir port=$port dbname=postgres connect_timeout=10"
psql=(python3 "$root/bounded-command.py" "$bindir/psql" "$connection" -X -v ON_ERROR_STOP=1)
"${psql[@]}" -c 'CREATE EXTENSION pg_accel' > "$run_dir/extension.log" 2>&1
result=0
set +e
"${psql[@]}" -c "SET pg_accel.resident_memory_budget_mb=64; SET pg_accel.kernel_timeout_ms=60000; SET statement_timeout='20min';" \
    -f "$root/profile.sql" -f "$root/fresh-derived-positive.sql" > "$run_dir/fresh-derived64.log" 2>&1
status=$?
set -e
printf 'fresh-derived-positive\t64\t%s\n' "$status" >> "$run_dir/results.tsv"
if [ "$status" -ne 0 ]; then result=1; fi
for budget in 64 128 256 DEFAULT; do
    set +e
    "${psql[@]}" -c "SET pg_accel.resident_memory_budget_mb=$budget; SET pg_accel.kernel_timeout_ms=60000; SET statement_timeout='20min';" \
        -f "$root/profile.sql" -f sql/tests/121_integer_where_aggregates.sql > "$run_dir/sql121-$budget.log" 2>&1
    status=$?
    set -e
    printf 'sql121\t%s\t%s\n' "$budget" "$status" >> "$run_dir/results.tsv"
    if [ "$status" -ne 0 ]; then result=1; fi
done
set +e
"${psql[@]}" -c "SET pg_accel.resident_memory_budget_mb=64; SET pg_accel.kernel_timeout_ms=60000; SET statement_timeout='20min';" \
    -f "$root/profile.sql" -f "$root/fixed-owner-negative.sql" > "$run_dir/fixed-owner32.log" 2>&1
status=$?
set -e
printf 'fixed-owner-negative\t32\t%s\n' "$status" >> "$run_dir/results.tsv"
if [ "$status" -ne 0 ]; then result=1; fi
shasum -a 256 -c "$run_dir/sql121.sha256" > "$run_dir/sql121-after.log"
exit "$result"
