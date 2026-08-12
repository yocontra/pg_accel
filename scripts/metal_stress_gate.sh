#!/usr/bin/env bash
set -euo pipefail

source scripts/pg_versions.sh

requested="${1:-}"
if [ -z "$requested" ]; then
    pg="$(pg_accel_default_pg_major)"
else
    pg="${requested#pg}"
fi
pg_accel_require_pgrx_support "$pg"
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
    set +e
    "$@" >> "$log" 2>&1
    local status=$?
    set -e
    if [ "$status" -ne 0 ]; then
        echo "${name}: FAIL status=${status} log=${log}" | tee -a "${artifact_dir}/summary.txt" >&2
        tail -80 "$log" | sed 's/^/  | /' >&2
        exit "$status"
    fi
    echo "${name}: PASS log=${log}" | tee -a "${artifact_dir}/summary.txt"
}

capture_acpp_provenance() {
    local source required_sha destination
    source=".pgaccel/acpp/current/pg_accel-acpp-provenance.txt"
    destination="${artifact_dir}/acpp-provenance.txt"
    required_sha="$(cat .acpp-version)"
    test -s "$source"
    grep -Fx "backend=metal" "$source"
    grep -Fx "acpp_required_sha=${required_sha}" "$source"
    grep -Fx "acpp_head=${required_sha}" "$source"
    grep -Fx "soft_fp64_required_tag=v2.0.1" "$source"
    grep -Fx "soft_fp64_desc=v2.0.1" "$source"
    grep -Fx "soft_fp64_package_version=2.0.1" "$source"
    awk '
        $0 == "soft_fp64_git_status_start" { inside = 1; start = 1; next }
        $0 == "soft_fp64_git_status_end" { inside = 0; finish = 1; next }
        inside { dirty = 1 }
        END { exit(start && finish && !dirty ? 0 : 1) }
    ' "$source"
    grep -F -- "-DSOFT_FP_BUILD_FP128=OFF" "$source"
    grep -F -- "-DSOFT_FP_BUILD_FP256=OFF" "$source"
    grep -F -- "-DSOFT_FP64_OCL=on" "$source"
    grep -F -- "-DSOFT_FP64_FENV=disabled" "$source"
    grep -F -- "-DCMAKE_CXX_FLAGS=-nostdinc++ -isystem " "$source"
    grep -F -- "/usr/include/c++/v1" "$source"
    cp "$source" "$destination"
    cmp "$source" "$destination"
}

assert_no_benchmark_crashes() {
    local cell_dir="$1"
    local crashes="${cell_dir}/crashes.json"
    if [ ! -f "$crashes" ]; then
        echo "error: missing crash artifact: $crashes" >&2
        return 1
    fi
    local count
    if ! count="$(python3 scripts/metal_stress_artifacts.py crash-count --path "$crashes")"; then
        echo "error: invalid benchmark crash artifact: $crashes" >&2
        return 1
    fi
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

run_logged "candidate-provenance" \
    python3 scripts/metal_stress_artifacts.py capture-candidate \
        --repo-root "$PWD" \
        --output "${artifact_dir}/candidate-provenance.json"
run_logged "acpp-provenance" capture_acpp_provenance

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

run_logged "install" just install-pg-accel "$pg"
run_logged "extension-smoke" \
    psql "$connection" -v ON_ERROR_STOP=1 \
        -f sql/init/01-create-extensions.sql \
        -c "SELECT * FROM pg_accel_stats();"
run_logged "sql-tests" \
    env PG_CONFIG="$pg_config" PG_ACCEL_PG_MAJOR="$pg" \
        PG_ACCEL_SQL_TEST_REQUIRE_EXTENSION=1 PG_ACCEL_RELEASE_MODE=1 \
        sql/tests/run_all.sh "$connection"
run_logged "clean-logs" just clean-logs "$pg"
run_logged "log-start-offsets" \
    python3 scripts/metal_stress_artifacts.py log-snapshot \
        --postgres-log "$pg_log" \
        --panic-log "$panic_log" \
        --output "${artifact_dir}/metal-log-start-offsets.json"

run_logged "standalone-gpu-tests" just gpu-test
run_logged "archive-cache-clear" just clear-jit
run_logged "archive-cache-before" \
    python3 scripts/metal_stress_artifacts.py snapshot \
        --point before_cold_archive_stress \
        --output "${artifact_dir}/metal-cache-before-archive.json"
run_logged "archive-fork-stress" \
    env PGACCEL_ARCHIVE_STRESS_CACHE_PRECLEARED=1 \
        PGACCEL_ARCHIVE_STRESS_RAW_LOG="${artifact_dir}/archive-fork-stress-raw.log" \
        just gpu-stress-archive "$fork_workers" "$fork_iters"
run_logged "archive-cache-after" \
    python3 scripts/metal_stress_artifacts.py snapshot \
        --point after_cold_archive_stress \
        --output "${artifact_dir}/metal-cache-after-archive.json"
run_logged "archive-artifacts" \
    python3 scripts/metal_stress_artifacts.py finalize \
        --before "${artifact_dir}/metal-cache-before-archive.json" \
        --after "${artifact_dir}/metal-cache-after-archive.json" \
        --archive-log "${artifact_dir}/archive-fork-stress-raw.log" \
        --output-dir "$artifact_dir"

run_benchmark_cell "gpu_reduce_sum" "100000" "warm"
run_benchmark_cell "gpu_nlj_between" "50000" "warm"
run_benchmark_cell "gpu_sort_topk_wide" "100000" "warm"
run_benchmark_cell "h3_bulk" "100000" "both"
run_benchmark_cell "spatial_filter" "100000" "warm"
run_benchmark_cell "raster_reclass" "100" "warm"
run_cancellation_probe
run_logged "postgres-log-audit" \
    python3 scripts/metal_stress_artifacts.py log-audit \
        --snapshot "${artifact_dir}/metal-log-start-offsets.json" \
        --output "${artifact_dir}/postgres-log-audit.json" \
        --excerpt "${artifact_dir}/postgres-log-tail.txt"

{
    echo "artifact_dir=${artifact_dir}"
    echo "iterations=${iterations}"
    echo "warmup=${warmup}"
    echo "fork_workers=${fork_workers}"
    echo "fork_iters=${fork_iters}"
    echo "gpu_test_timeout_s=${GPU_TEST_TIMEOUT_S}"
} | tee -a "${artifact_dir}/summary.txt"

echo "metal-stress: PASS" | tee -a "${artifact_dir}/summary.txt"
python3 scripts/metal_stress_artifacts.py index --artifact-dir "$artifact_dir"
echo "metal-stress: artifact index sealed at ${artifact_dir}/artifact_index.json"
