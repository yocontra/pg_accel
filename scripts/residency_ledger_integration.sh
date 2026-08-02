#!/usr/bin/env bash
# Exercise the production PgLwLock residency ledger with real PostgreSQL
# backends. This intentionally does not use cargo-pgrx's `pg_test` feature,
# whose process-local Mutex cannot validate shared-memory behavior.

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=scripts/pg_versions.sh
source "$repo_root/scripts/pg_versions.sh"

connection="${1:-}"
if [ -z "$connection" ]; then
    pg="${PG_ACCEL_PG_MAJOR:-$(pg_accel_default_pg_major)}"
    port="$(pg_accel_pgrx_port_for_pg "$pg")"
    connection="host=localhost port=$port dbname=postgres"
fi

psql_bin="${PSQL:-psql}"
suffix="$$_$(date +%s)"
seed_db="pg_accel_ledger_seed_$suffix"
db_a="pg_accel_ledger_a_$suffix"
db_b="pg_accel_ledger_b_$suffix"
artifact_dir="${PG_ACCEL_LEDGER_ARTIFACT_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/pgaccel-ledger.XXXXXX")}"
mkdir -p "$artifact_dir"

fifo_a="$artifact_dir/session-a.fifo"
fifo_b="$artifact_dir/session-b.fifo"
client_a=""
client_b=""
fifos_open=0
succeeded=0

run_base() {
    "$psql_bin" "$connection" -X -qAt -v ON_ERROR_STOP=1 -c "$1"
}

run_db() {
    local database="$1"
    local sql="$2"
    {
        printf '\\connect %s\n' "$database"
        printf '%s\n' "$sql"
    } | "$psql_bin" "$connection" -X -qAt -v ON_ERROR_STOP=1
}

terminate_client() {
    local client_pid="$1"
    if [ -n "$client_pid" ] && kill -0 "$client_pid" 2>/dev/null; then
        kill "$client_pid" 2>/dev/null || true
        wait "$client_pid" 2>/dev/null || true
    fi
}

cleanup() {
    if [ "$fifos_open" -eq 1 ]; then
        exec 3>&-
        exec 4>&-
    fi
    terminate_client "$client_a"
    terminate_client "$client_b"
    run_base "DROP DATABASE IF EXISTS $db_a WITH (FORCE)" >/dev/null 2>&1 || true
    run_base "DROP DATABASE IF EXISTS $db_b WITH (FORCE)" >/dev/null 2>&1 || true
    run_base "DROP DATABASE IF EXISTS $seed_db WITH (FORCE)" >/dev/null 2>&1 || true
    if [ "$succeeded" -eq 1 ] && [ -z "${PG_ACCEL_LEDGER_ARTIFACT_DIR:-}" ]; then
        rm -rf "$artifact_dir"
    elif [ "$succeeded" -ne 1 ]; then
        echo "residency-ledger integration artifacts: $artifact_dir" >&2
    fi
}
trap cleanup EXIT

wait_for_phase() {
    local database="$1"
    local phase="$2"
    local deadline=$((SECONDS + 60))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if [ "$(run_db "$database" "SELECT count(*) FROM ledger_sync WHERE phase = '$phase'")" = "1" ]; then
            return 0
        fi
        sleep 0.1
    done
    echo "error: timed out waiting for $database phase $phase" >&2
    return 1
}

wait_for_live_bytes() {
    local database="$1"
    local expected="$2"
    local deadline=$((SECONDS + 30))
    local actual=""
    while [ "$SECONDS" -lt "$deadline" ]; do
        actual="$(run_db "$database" 'SELECT pg_accel_resident_live_bytes()')"
        if [ "$actual" = "$expected" ]; then
            return 0
        fi
        sleep 0.1
    done
    echo "error: resident live bytes were $actual, expected $expected" >&2
    return 1
}

read_phase() {
    local database="$1"
    local phase="$2"
    run_db "$database" \
        "SELECT generation || '|' || bytes || '|' || backend_pid FROM ledger_sync WHERE phase = '$phase'"
}

preload="$(run_base 'SHOW shared_preload_libraries')"
if [[ ",$preload," != *,pg_accel,* ]] && [[ "$preload" != *pg_accel* ]]; then
    echo "error: shared_preload_libraries does not contain pg_accel: $preload" >&2
    exit 1
fi

run_base "CREATE DATABASE $seed_db"
run_db "$seed_db" '
CREATE EXTENSION pg_accel;
CREATE TABLE ledger_probe (
    id integer PRIMARY KEY,
    g integer NOT NULL,
    v integer NOT NULL
);
INSERT INTO ledger_probe
SELECT n, n % 127, n * 3 FROM generate_series(1, 500000) AS n;
ANALYZE ledger_probe;
CREATE TABLE ledger_sync (
    phase text PRIMARY KEY,
    backend_pid integer NOT NULL,
    generation bigint NOT NULL,
    bytes bigint NOT NULL
);
CREATE TABLE ledger_results (
    phase text PRIMARY KEY,
    value bigint NOT NULL
);
CREATE TABLE ledger_plans (
    phase text NOT NULL,
    line text NOT NULL
);
CREATE TABLE ledger_counters (
    phase text PRIMARY KEY,
    kernels_before bigint NOT NULL,
    kernels_after bigint NOT NULL
);'

gpu_available="$(run_db "$seed_db" 'SELECT gpu_available::int FROM pg_accel_device_info()')"
if [ "$gpu_available" != "1" ]; then
    echo "error: residency-ledger integration requires a usable GPU device" >&2
    exit 1
fi

# Install the extension-owned invalidation trigger before cloning. Otherwise
# concurrent first pins in the two same-OID databases can exchange relcache
# invalidations while each trigger is still being created, obscuring the
# cross-database ledger assertion with a deliberate first-use eviction.
seed_pinned="$(run_db "$seed_db" \
    "SELECT pg_accel_pin('ledger_probe', ARRAY['g', 'v'])")"
if [ "$seed_pinned" -ne 500000 ]; then
    echo "error: seed trigger installation pinned $seed_pinned rows, expected 500000" >&2
    exit 1
fi

run_base "CREATE DATABASE $db_a TEMPLATE $seed_db"
run_base "CREATE DATABASE $db_b TEMPLATE $seed_db"
run_base "DROP DATABASE $seed_db"

oid_a="$(run_db "$db_a" "SELECT 'ledger_probe'::regclass::oid")"
oid_b="$(run_db "$db_b" "SELECT 'ledger_probe'::regclass::oid")"
if [ "$oid_a" != "$oid_b" ]; then
    echo "error: cloned cross-database probe relations do not share an OID" >&2
    exit 1
fi

baseline="$(run_db "$db_a" 'SELECT pg_accel_resident_live_bytes()')"

mkfifo "$fifo_a" "$fifo_b"
exec 3<>"$fifo_a"
exec 4<>"$fifo_b"
fifos_open=1
"$psql_bin" "$connection" -X -q -v ON_ERROR_STOP=1 <"$fifo_a" \
    >"$artifact_dir/session-a.log" 2>&1 &
client_a=$!
"$psql_bin" "$connection" -X -q -v ON_ERROR_STOP=1 <"$fifo_b" \
    >"$artifact_dir/session-b.log" 2>&1 &
client_b=$!

printf '\\connect %s\n' "$db_a" >&3
cat >&3 <<'SQL'
SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
SET pg_accel.auto_load = off;
SELECT pg_accel_pin('ledger_probe', ARRAY['g', 'v']);
PREPARE resident_probe AS
SELECT sum(group_total)::bigint AS total
FROM (
    SELECT g, sum(v)::bigint AS group_total
    FROM ledger_probe
    GROUP BY g
) AS grouped;
DO $$
DECLARE plan_row record;
BEGIN
    FOR plan_row IN EXECUTE
        'EXPLAIN (VERBOSE, COSTS OFF) EXECUTE resident_probe'
    LOOP
        INSERT INTO ledger_plans VALUES ('before_trigger_repair', plan_row."QUERY PLAN");
    END LOOP;
END $$;
SELECT pg_accel_kernel_executions() AS kernels_before \gset
EXECUTE resident_probe \gset
SELECT pg_accel_kernel_executions() AS kernels_after \gset
INSERT INTO ledger_results VALUES ('before_trigger_repair', :total);
INSERT INTO ledger_counters
VALUES ('before_trigger_repair', :kernels_before, :kernels_after);
INSERT INTO ledger_sync
SELECT 'ready', pg_backend_pid(), generation, raw_bytes + derived_bytes
FROM pg_accel_resident_status()
WHERE relid = 'ledger_probe'::regclass;
SQL

printf '\\connect %s\n' "$db_b" >&4
cat >&4 <<'SQL'
SELECT pg_accel_pin('ledger_probe', ARRAY['g', 'v']);
INSERT INTO ledger_sync
SELECT 'ready', pg_backend_pid(), generation, raw_bytes + derived_bytes
FROM pg_accel_resident_status()
WHERE relid = 'ledger_probe'::regclass;
SQL

wait_for_phase "$db_a" ready
wait_for_phase "$db_b" ready
IFS='|' read -r a_generation a_bytes _ <<<"$(read_phase "$db_a" ready)"
IFS='|' read -r b_generation b_bytes b_backend_pid <<<"$(read_phase "$db_b" ready)"
if [ "$a_bytes" -le 0 ] || [ "$b_bytes" -le 0 ]; then
    echo "error: both database backends must hold positive resident ledger charges" >&2
    exit 1
fi
expected_both=$((baseline + a_bytes + b_bytes))
wait_for_live_bytes "$db_a" "$expected_both"

initial_plan_contract="$(run_db "$db_a" "
SELECT
    count(*) FILTER (WHERE line LIKE '%Custom Scan (GpuAccelAgg)%') || '|' ||
    count(*) FILTER (WHERE line LIKE '%GPU Resident Operator Class: resident_groupagg%')
FROM ledger_plans
WHERE phase = 'before_trigger_repair'")"
initial_counter_contract="$(run_db "$db_a" "
SELECT kernels_before || '|' || kernels_after
FROM ledger_counters
WHERE phase = 'before_trigger_repair'")"
IFS='|' read -r initial_custom_scan initial_resident_groupagg <<<"$initial_plan_contract"
IFS='|' read -r initial_kernels_before initial_kernels_after <<<"$initial_counter_contract"
if [ "$initial_custom_scan" -lt 1 ] || [ "$initial_resident_groupagg" -lt 1 ]; then
    echo "error: prepared query did not select Custom Scan resident_groupagg" >&2
    exit 1
fi
if [ "$initial_kernels_after" -le "$initial_kernels_before" ]; then
    echo "error: selected prepared query did not dispatch a GPU kernel" >&2
    exit 1
fi

# Suppress the invalidation trigger for one committed write, restoring it in
# the same transaction. The catalog identity/fingerprint change must still
# make backend A discard its old snapshot before a prepared query can run.
run_db "$db_a" '
BEGIN;
ALTER TABLE ledger_probe DISABLE TRIGGER __pg_accel_residency_v2_7d9e;
UPDATE ledger_probe SET v = v + 1000 WHERE id = 1;
ALTER TABLE ledger_probe ENABLE ALWAYS TRIGGER __pg_accel_residency_v2_7d9e;
COMMIT;'

cat >&3 <<'SQL'
INSERT INTO ledger_sync
SELECT 'trigger_invalidated', pg_backend_pid(), generation, raw_bytes + derived_bytes
FROM pg_accel_resident_status()
WHERE relid = 'ledger_probe'::regclass;
SELECT pg_accel_refresh('ledger_probe');
DO $$
DECLARE plan_row record;
BEGIN
    FOR plan_row IN EXECUTE
        'EXPLAIN (VERBOSE, COSTS OFF) EXECUTE resident_probe'
    LOOP
        INSERT INTO ledger_plans VALUES ('after_trigger_repair', plan_row."QUERY PLAN");
    END LOOP;
END $$;
SELECT pg_accel_kernel_executions() AS kernels_before \gset
EXECUTE resident_probe \gset
SELECT pg_accel_kernel_executions() AS kernels_after \gset
INSERT INTO ledger_results VALUES ('after_trigger_repair', :total);
INSERT INTO ledger_counters
VALUES ('after_trigger_repair', :kernels_before, :kernels_after);
INSERT INTO ledger_sync
SELECT 'trigger_refreshed', pg_backend_pid(), generation, raw_bytes + derived_bytes
FROM pg_accel_resident_status()
WHERE relid = 'ledger_probe'::regclass;
SQL

wait_for_phase "$db_a" trigger_invalidated
wait_for_phase "$db_a" trigger_refreshed
IFS='|' read -r _ trigger_invalidated_bytes _ <<<"$(read_phase "$db_a" trigger_invalidated)"
IFS='|' read -r _ trigger_refreshed_bytes _ <<<"$(read_phase "$db_a" trigger_refreshed)"
prepared_before="$(run_db "$db_a" \
    "SELECT value FROM ledger_results WHERE phase = 'before_trigger_repair'")"
prepared_after="$(run_db "$db_a" \
    "SELECT value FROM ledger_results WHERE phase = 'after_trigger_repair'")"
repaired_plan_contract="$(run_db "$db_a" "
SELECT
    count(*) FILTER (WHERE line LIKE '%Custom Scan (GpuAccelAgg)%') || '|' ||
    count(*) FILTER (WHERE line LIKE '%GPU Resident Operator Class: resident_groupagg%')
FROM ledger_plans
WHERE phase = 'after_trigger_repair'")"
repaired_counter_contract="$(run_db "$db_a" "
SELECT kernels_before || '|' || kernels_after
FROM ledger_counters
WHERE phase = 'after_trigger_repair'")"
IFS='|' read -r repaired_custom_scan repaired_resident_groupagg <<<"$repaired_plan_contract"
IFS='|' read -r repaired_kernels_before repaired_kernels_after <<<"$repaired_counter_contract"
if [ "$trigger_invalidated_bytes" -ne 0 ]; then
    echo "error: restored trigger identity allowed backend A to retain stale residency" >&2
    exit 1
fi
if [ "$trigger_refreshed_bytes" -le 0 ] || [ "$prepared_after" -ne $((prepared_before + 1000)) ]; then
    echo "error: prepared query did not observe the trigger-suppressed committed write" >&2
    exit 1
fi
if [ "$repaired_custom_scan" -lt 1 ] || [ "$repaired_resident_groupagg" -lt 1 ] ||
   [ "$repaired_kernels_after" -le "$repaired_kernels_before" ]; then
    echo "error: repaired prepared query did not select and dispatch resident_groupagg" >&2
    exit 1
fi

worker_pids=()
for shard in 0 1 2 3; do
    (
        run_db "$db_a" \
            "UPDATE ledger_probe SET v = v + 1 WHERE id % 4 = $shard" \
            >"$artifact_dir/invalidation-$shard.log" 2>&1
    ) &
    worker_pids+=("$!")
done
for worker_pid in "${worker_pids[@]}"; do
    wait "$worker_pid"
done

cat >&3 <<'SQL'
INSERT INTO ledger_sync
SELECT 'invalidated', pg_backend_pid(), generation, raw_bytes + derived_bytes
FROM pg_accel_resident_status()
WHERE relid = 'ledger_probe'::regclass;
SELECT pg_accel_refresh('ledger_probe');
INSERT INTO ledger_sync
SELECT 'refreshed', pg_backend_pid(), generation, raw_bytes + derived_bytes
FROM pg_accel_resident_status()
WHERE relid = 'ledger_probe'::regclass;
SQL

cat >&4 <<'SQL'
INSERT INTO ledger_sync
SELECT 'after_foreign_change', pg_backend_pid(), generation, raw_bytes + derived_bytes
FROM pg_accel_resident_status()
WHERE relid = 'ledger_probe'::regclass;
SQL

wait_for_phase "$db_a" invalidated
wait_for_phase "$db_a" refreshed
wait_for_phase "$db_b" after_foreign_change
IFS='|' read -r invalidated_generation invalidated_bytes _ <<<"$(read_phase "$db_a" invalidated)"
IFS='|' read -r refreshed_generation refreshed_bytes _ <<<"$(read_phase "$db_a" refreshed)"
IFS='|' read -r b_after_generation b_after_bytes _ <<<"$(read_phase "$db_b" after_foreign_change)"

if [ "$invalidated_generation" -le "$a_generation" ] || [ "$invalidated_bytes" -ne 0 ]; then
    echo "error: concurrent same-database changes did not invalidate the resident snapshot" >&2
    exit 1
fi
if [ "$refreshed_generation" -lt "$invalidated_generation" ] || [ "$refreshed_bytes" -le 0 ]; then
    echo "error: invalidated resident snapshot did not reload at the current generation" >&2
    exit 1
fi
if [ "$b_after_generation" -ne "$b_generation" ] || [ "$b_after_bytes" -ne "$b_bytes" ]; then
    echo "error: a same-OID change in one database invalidated the other database" >&2
    exit 1
fi

expected_refreshed=$((baseline + refreshed_bytes + b_bytes))
wait_for_live_bytes "$db_a" "$expected_refreshed"

# A PostgreSQL FATAL exit must run the registered shared-memory cleanup and
# release exactly this backend's charge without disturbing the other database.
run_db "$db_a" "SELECT pg_terminate_backend($b_backend_pid)" >/dev/null
# psql reads commands from the control FIFO and may not notice an asynchronous
# FATAL until its next round trip. Force that round trip, then quit if libpq
# reconnects, so client behavior cannot hang the backend-cleanup assertion.
printf 'SELECT 1;\n\\quit\n' >&4
set +e
wait "$client_b"
set -e
client_b=""
if [ "$(run_db "$db_a" "SELECT count(*) FROM pg_stat_activity WHERE pid = $b_backend_pid")" -ne 0 ]; then
    echo "error: terminated ledger backend remains in pg_stat_activity" >&2
    exit 1
fi
wait_for_live_bytes "$db_a" "$((baseline + refreshed_bytes))"

# Reuse a released backend slot in the second database, then prove its normal
# exit also returns the temporary charge.
run_db "$db_b" "SELECT pg_accel_pin('ledger_probe', ARRAY['g', 'v']); SELECT pg_accel_resident_live_bytes()" \
    >"$artifact_dir/slot-reuse.log"
wait_for_live_bytes "$db_a" "$((baseline + refreshed_bytes))"

printf '\\quit\n' >&3
wait "$client_a"
client_a=""
wait_for_live_bytes "$db_a" "$baseline"

succeeded=1
echo "residency-ledger integration: PASS"
echo "  relation_oid=$oid_a baseline_bytes=$baseline"
echo "  database_a_bytes=$refreshed_bytes database_b_bytes=$b_bytes"
echo "  generation=$a_generation->$invalidated_generation->$refreshed_generation"
