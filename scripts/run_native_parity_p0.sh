#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$repo_root/scripts/pg_versions.sh"

artifact_root="${1:?usage: scripts/run_native_parity_p0.sh ARTIFACT_DIR [CONNECTION]}"
connection="${2:-${PG_ACCEL_BENCH_CONNECTION:-postgresql://localhost:28818/postgres}}"
pg_major="${PG_ACCEL_PG_MAJOR:-18}"
matrix_source="$repo_root/benchmarks/native-parity-p0.tsv"
bench="$repo_root/target/release/pg_accel_bench"
pg_config_path="$(pg_accel_pg_config_for_pg "$pg_major")"

if [[ -e "$artifact_root" ]]; then
    echo "error: native-parity artifact path already exists: $artifact_root" >&2
    exit 1
fi
if [[ ! -x "$bench" ]]; then
    echo "error: release benchmark harness is missing at $bench" >&2
    echo "       run: cargo build --release -p pg_accel_bench" >&2
    exit 1
fi
if [[ ! -f "$matrix_source" ]]; then
    echo "error: native-parity matrix is missing at $matrix_source" >&2
    exit 1
fi

require_exact_candidate() {
    local status head_tree index_tree
    status="$(git -C "$repo_root" status --porcelain=v1 --untracked-files=all --ignore-submodules=none)"
    if [[ -n "$status" ]]; then
        printf 'error: native-parity requires an exact clean candidate; worktree changes:\n%s\n' \
            "$status" >&2
        return 1
    fi
    head_tree="$(git -C "$repo_root" rev-parse --verify 'HEAD^{tree}')"
    index_tree="$(git -C "$repo_root" write-tree)"
    if [[ "$head_tree" != "$index_tree" ]]; then
        echo "error: native-parity index tree does not match HEAD tree" >&2
        return 1
    fi
}

reject_build_processes() {
    local matches
    matches="$(pgrep -fl '(^|/)(cargo|rustc|clang|clang\+\+|cc|c\+\+|cmake|ninja|make|ctest|run_testfloat|testfloat_gen)( |$)' || true)"
    if [[ -n "$matches" ]]; then
        printf 'error: refusing a non-exclusive benchmark run; build processes are active:\n%s\n' "$matches" >&2
        return 1
    fi
}

reject_cpu_contention() {
    local threshold="${PG_ACCEL_BENCH_MAX_FOREIGN_CPU_PERCENT:-50}"
    local matches
    if ! [[ "$threshold" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
        echo "error: PG_ACCEL_BENCH_MAX_FOREIGN_CPU_PERCENT must be numeric" >&2
        return 1
    fi
    matches="$(LC_ALL=C ps -axo pid=,ppid=,%cpu=,command= | awk \
        -v threshold="$threshold" -v harness_pid="$$" \
        '$1 != harness_pid && ($3 + 0) >= threshold { print }')"
    if [[ -n "$matches" ]]; then
        printf 'error: refusing a contaminated benchmark run; foreign processes are using at least %s%% CPU:\n%s\n' \
            "$threshold" "$matches" >&2
        return 1
    fi
}

hash_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1"
    else
        shasum -a 256 "$1"
    fi
}

require_exact_candidate
reject_build_processes
reject_cpu_contention

mkdir -p "$artifact_root/cells"
cp "$matrix_source" "$artifact_root/matrix.tsv"
date -u +%Y-%m-%dT%H:%M:%SZ > "$artifact_root/started_at.txt"
git -C "$repo_root" status --porcelain=v1 > "$artifact_root/source_status.txt"
git -C "$repo_root" diff --binary HEAD > "$artifact_root/source.patch"
{
    printf 'pg_config=%s\n' "$pg_config_path"
    printf 'pg_version=%s\n' "$("$pg_config_path" --version)"
    printf 'connection=%s\n' "$connection"
    printf 'max_foreign_cpu_percent=%s\n' "${PG_ACCEL_BENCH_MAX_FOREIGN_CPU_PERCENT:-50}"
    printf 'git_head=%s\n' "$(git -C "$repo_root" rev-parse HEAD)"
    printf 'git_head_tree=%s\n' "$(git -C "$repo_root" rev-parse 'HEAD^{tree}')"
    printf 'git_index_tree=%s\n' "$(git -C "$repo_root" write-tree)"
    hash_file "$bench"
    hash_file "$artifact_root/source.patch"
} > "$artifact_root/runtime_provenance.txt"

while IFS=$'\t' read -r ordinal workload rows cohort reason; do
    [[ "$ordinal" == "ordinal" ]] && continue
    reject_build_processes
    reject_cpu_contention
    cell="$artifact_root/cells/${ordinal}-${workload}-${rows}"
    mkdir -p "$cell"
    printf '[native-parity-p0] %s %s @ %s (%s; %s)\n' \
        "$ordinal" "$workload" "$rows" "$cohort" "$reason"
    PG_CONFIG="$pg_config_path" PG_ACCEL_PG_MAJOR="$pg_major" "$bench" crash-repro \
        --workload "$workload" \
        --rows "$rows" \
        --iterations 30 \
        --warmup 5 \
        --seed 42 \
        --connection "$connection" \
        --capture-plans \
        --native-parity-pairing \
        --timing raw \
        --cache-mode warm \
        --skip-guc-verify \
        --artifacts-dir "$cell" \
        --format json > "$cell/stdout.json" 2> "$cell/stderr.log"
    date -u +%Y-%m-%dT%H:%M:%SZ > "$cell/exclusive-complete.txt"
done < "$artifact_root/matrix.tsv"

reject_build_processes
reject_cpu_contention
PYTHONDONTWRITEBYTECODE=1 python3 "$repo_root/scripts/strengthened_native_parity.py" \
    "$artifact_root" --output "$artifact_root/native_parity.json"
date -u +%Y-%m-%dT%H:%M:%SZ > "$artifact_root/completed_at.txt"

(
    cd "$artifact_root"
    find . -type f ! -name SHA256SUMS | LC_ALL=C sort | while IFS= read -r path; do
        hash_file "$path"
    done > SHA256SUMS
)

echo "native-parity P0 gate passed: $artifact_root"
