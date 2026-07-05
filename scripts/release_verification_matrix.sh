#!/usr/bin/env bash
set -euo pipefail

source scripts/pg_versions.sh

requested="${1:-}"
if [ -z "$requested" ]; then
    pg="$(pg_accel_default_pg_major)"
else
    pg="${requested#pg}"
fi
pg_accel_require_supported_pg "$pg"
if pg_accel_skip_if_preview_without_pgrx "$pg"; then
    echo "release-verify: skipping PostgreSQL $pg because pgrx support is unavailable"
    exit 0
fi
pg_accel_require_pgrx_pg_config "$pg"

ts="$(date +%Y%m%d-%H%M%S)"
artifact_dir="${RELEASE_VERIFY_ARTIFACT_DIR:-benchmarks/artifacts/release-verify-${ts}}"
mkdir -p "$artifact_dir"

pg_config="$(pg_accel_pg_config_for_pg "$pg")"
port="$(pg_accel_pgrx_port_for_pg "$pg")"
connection="host=localhost port=${port} dbname=postgres"
iterations="${RELEASE_VERIFY_ITERATIONS:-10}"
warmup="${RELEASE_VERIFY_WARMUP:-5}"

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

{
    echo "timestamp=${ts}"
    echo "pg=${pg}"
    echo "connection=${connection}"
    echo "uname=$(uname -sm)"
    cargo --version
    rustc --version
    git rev-parse HEAD
    git status --short
} > "${artifact_dir}/metadata.txt"

run_logged "install-pg-accel" just install-pg-accel "$pg"
run_logged "provenance" \
    env PG_CONFIG="$pg_config" PG_ACCEL_PG_MAJOR="$pg" \
    cargo run --release -p pg_accel_bench -- provenance \
        --connection "$connection"
run_logged "explain-audit" \
    env PG_CONFIG="$pg_config" PG_ACCEL_PG_MAJOR="$pg" \
    cargo run --release -p pg_accel_bench -- explain-audit \
        --connection "$connection"
run_logged "workload-validate" \
    cargo run --release -p pg_accel_bench -- validate
run_logged "pg-accel-stats" \
    psql "$connection" -v ON_ERROR_STOP=1 \
        -f sql/init/01-create-extensions.sql \
        -c "SELECT * FROM pg_accel_stats();"
run_logged "sql-tests" \
    env PG_CONFIG="$pg_config" PG_ACCEL_PG_MAJOR="$pg" \
        PG_ACCEL_SQL_TEST_REQUIRE_EXTENSION=1 PG_ACCEL_RELEASE_MODE=1 \
        sql/tests/run_all.sh "$connection"
run_logged "deferred-site-audit" \
    rg -n "planner-defer|planner-decline|unsupported|TODO|FIXME|deferred|Deferred" \
        TODO.md docs README.md pg_accel pg_accel_bench
run_logged "fork-stress" just gpu-stress-archive
run_logged "benchmark-sweep" \
    env PG_CONFIG="$pg_config" PG_ACCEL_PG_MAJOR="$pg" \
    cargo run --release -p pg_accel_bench -- run \
        --iterations "$iterations" \
        --warmup "$warmup" \
        --connection "$connection" \
        --format markdown \
        --realistic-gucs \
        --capture-plans \
        --timing raw \
        --cache-mode both \
        --artifacts-dir "${artifact_dir}/benchmark-sweep"

if [ "$(uname -s)" = "Darwin" ] && [ "$(uname -m)" = "arm64" ]; then
    run_logged "metal-stress" \
        env METAL_STRESS_ARTIFACT_DIR="${artifact_dir}/metal-stress" \
        just metal-stress "$pg"
fi

if command -v nvidia-smi >/dev/null 2>&1; then
    run_logged "cuda-stress" \
        env CUDA_STRESS_ARTIFACT_DIR="${artifact_dir}/cuda-stress" \
        just cuda-stress "$pg"
else
    echo "cuda-stress: skipped (no nvidia-smi on this host)" | tee -a "${artifact_dir}/summary.txt"
fi

echo "release-verify: PASS artifact_dir=${artifact_dir}" | tee -a "${artifact_dir}/summary.txt"
