#!/usr/bin/env bash
set -euo pipefail

pg="${1:-17}"
pg="${pg#pg}"

source scripts/pg_versions.sh
pg_accel_require_supported_pg "$pg"
if pg_accel_skip_if_preview_without_pgrx "$pg"; then
    echo "metal-stress: skipping PostgreSQL $pg because pgrx support is unavailable"
    exit 0
fi
pg_accel_require_pgrx_pg_config "$pg"

if [ "$(uname -s)" != "Darwin" ] || [ "$(uname -m)" != "arm64" ]; then
    echo "error: metal-stress requires an Apple Silicon macOS runner" >&2
    exit 1
fi

ts="$(date +%Y%m%d-%H%M%S)"
artifact_dir="${METAL_STRESS_ARTIFACT_DIR:-benchmarks/artifacts/metal-stress-${ts}}"
mkdir -p "$artifact_dir"

pg_config="$(pg_accel_pg_config_for_pg "$pg")"
port="$(pg_accel_pgrx_port_for_pg "$pg")"
connection="host=localhost port=${port} dbname=postgres"
data_dir="$(pg_accel_pgrx_data_dir_for_pg "$pg")"
pg_log="$(pg_accel_pgrx_log_for_pg "$pg")"
panic_log="${data_dir}/pg_accel_panic.log"

iterations="${METAL_STRESS_ITERATIONS:-20}"
warmup="${METAL_STRESS_WARMUP:-5}"
fork_workers="${METAL_STRESS_FORK_WORKERS:-8}"
fork_iters="${METAL_STRESS_FORK_ITERS:-20}"
export GPU_TEST_TIMEOUT_S="${METAL_STRESS_GPU_TEST_TIMEOUT_S:-300}"

run_logged() {
    local name="$1"
    shift
    local log="${artifact_dir}/${name}.log"
    echo "=== ${name} ===" | tee -a "${artifact_dir}/summary.txt"
    echo "+ $*" > "$log"
    "$@" >> "$log" 2>&1
}

json_crash_count() {
    python3 - "$1" <<'PY'
import json
import sys
path = sys.argv[1]
with open(path, encoding="utf-8") as fh:
    data = json.load(fh)
if isinstance(data, list):
    print(len(data))
elif isinstance(data, dict) and isinstance(data.get("crashes"), list):
    print(len(data["crashes"]))
else:
    print(0)
PY
}

assert_no_benchmark_crashes() {
    local cell_dir="$1"
    local crashes="${cell_dir}/crashes.json"
    if [ ! -f "$crashes" ]; then
        echo "error: missing crash artifact: $crashes" >&2
        return 1
    fi
    local count
    count="$(json_crash_count "$crashes")"
    if [ "$count" != "0" ]; then
        echo "error: benchmark crash artifact contains ${count} crash(es): $crashes" >&2
        return 1
    fi
}

run_benchmark_cell() {
    local name="$1"
    local rows="$2"
    local cache_mode="$3"
    local cell_dir="${artifact_dir}/bench-${name}-${rows}"
    local log="${artifact_dir}/bench-${name}-${rows}.log"

    echo "=== benchmark ${name} rows=${rows} cache=${cache_mode} ===" | tee -a "${artifact_dir}/summary.txt"
    mkdir -p "$cell_dir"
    set +e
    PG_CONFIG="$pg_config" PG_ACCEL_PG_MAJOR="$pg" \
        cargo run --release -p pg_accel_bench -- crash-repro \
            --workload "$name" \
            --rows "$rows" \
            --iterations "$iterations" \
            --warmup "$warmup" \
            --connection "$connection" \
            --format markdown \
            --capture-plans \
            --timing raw \
            --cache-mode "$cache_mode" \
            --artifacts-dir "$cell_dir" \
            > "$log" 2>&1
    local status=$?
    set -e

    assert_no_benchmark_crashes "$cell_dir"
    if [ "$status" -ne 0 ]; then
        echo "error: benchmark cell ${name}/${rows} exited ${status}; see $log" >&2
        return "$status"
    fi
}

run_cancellation_probe() {
    local log="${artifact_dir}/cancellation.log"
    echo "=== cancellation probe ===" | tee -a "${artifact_dir}/summary.txt"
    set +e
    psql "$connection" -v ON_ERROR_STOP=1 > "$log" 2>&1 <<'SQL'
CREATE EXTENSION IF NOT EXISTS h3;
CREATE EXTENSION IF NOT EXISTS postgis;
CREATE EXTENSION IF NOT EXISTS postgis_raster;
CREATE EXTENSION IF NOT EXISTS pg_accel;
SET pg_accel.enabled = on;
SET statement_timeout = '25ms';
SELECT count(*)
FROM (
  SELECT h3_latlng_to_cell(point(g % 360 - 180, g % 180 - 90), 7)
  FROM generate_series(1, 10000000) AS g
) AS stress_cancelled;
SQL
    local status=$?
    set -e
    if [ "$status" -eq 0 ]; then
        echo "error: cancellation probe completed without statement timeout" >&2
        return 1
    fi
    if ! grep -Eiq "statement timeout|canceling statement|cancelled statement" "$log"; then
        echo "error: cancellation probe failed for an unexpected reason; see $log" >&2
        return 1
    fi
    psql "$connection" -v ON_ERROR_STOP=1 -c "RESET statement_timeout; SELECT 1;" >> "$log" 2>&1
}

assert_clean_logs() {
    local combined="${artifact_dir}/postgres-log-tail.txt"
    {
        [ -f "$pg_log" ] && tail -400 "$pg_log" || true
        [ -f "$panic_log" ] && tail -400 "$panic_log" || true
    } > "$combined"

    if [ -s "$panic_log" ]; then
        echo "error: pg_accel panic log is non-empty: $panic_log" >&2
        return 1
    fi
    if grep -Eiq "PGACCEL PANIC|PANIC|segmentation fault|MTLCompilerService|resource leak|leaked resource|kernel failure" "$combined"; then
        echo "error: crash/panic/resource-leak pattern found in $combined" >&2
        return 1
    fi
}

just gpu-build > "${artifact_dir}/gpu-build.log" 2>&1

{
    echo "timestamp=${ts}"
    echo "pg=${pg}"
    echo "connection=${connection}"
    echo "uname=$(uname -sm)"
    sysctl -n machdep.cpu.brand_string 2>/dev/null || true
    cargo --version
    cargo llvm-cov --version 2>/dev/null || true
    ./.pgaccel/acpp/current/bin/acpp --acpp-version 2>/dev/null || true
    ./pgaccel-kernels/build/test_device 2>/dev/null || true
} > "${artifact_dir}/metadata.txt"

if ! grep -Eq "Backend:[[:space:]]+metal" "${artifact_dir}/metadata.txt"; then
    echo "error: metal-stress did not detect an AdaptiveCpp Metal device" >&2
    exit 1
fi

just install-pg-accel "$pg" > "${artifact_dir}/install.log" 2>&1
just clean-logs "$pg" > "${artifact_dir}/clean-logs.log" 2>&1

run_logged "standalone-gpu-tests" just gpu-test
run_logged "archive-fork-stress" just gpu-stress-archive "$fork_workers" "$fork_iters"

run_benchmark_cell "gpu_reduce_sum" "100000" "warm"
run_benchmark_cell "gpu_nlj_between" "50000" "warm"
run_benchmark_cell "gpu_sort_topk_wide" "100000" "warm"
run_benchmark_cell "h3_bulk" "100000" "both"
run_benchmark_cell "spatial_filter" "100000" "warm"
run_benchmark_cell "raster_reclass" "100" "warm"
run_cancellation_probe
assert_clean_logs

{
    echo "metal-stress: PASS"
    echo "artifact_dir=${artifact_dir}"
    echo "iterations=${iterations}"
    echo "warmup=${warmup}"
    echo "fork_workers=${fork_workers}"
    echo "fork_iters=${fork_iters}"
    echo "gpu_test_timeout_s=${GPU_TEST_TIMEOUT_S}"
} | tee -a "${artifact_dir}/summary.txt"
