#!/usr/bin/env bash
# Exercise the instrumented pg_accel_bench production CLI against a disposable
# database. This is coverage evidence only: instrumented timings are never
# accepted as benchmark or release-performance evidence.

set -euo pipefail

readonly COVERAGE_LIVE_SCHEMA_VERSION=1
readonly SAFE_DATABASE_PREFIX="pgaccel_cov_"

# Stateful command validation is bound to the live harness's disposable target.
# The environment defaults make the validator independently testable; executable
# runs overwrite them with canonical paths after argument validation.
coverage_live_expected_connection="${PGACCEL_COVERAGE_EXPECTED_CONNECTION:-}"
coverage_live_expected_artifact_root="${PGACCEL_COVERAGE_EXPECTED_ARTIFACT_ROOT:-}"

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: coverage_live_rust.sh \
  --repo-root PATH --build-dir PATH --artifact-dir PATH \
  --bench-bin PATH --profile-dir PATH --psql-bin PATH \
  --admin-connection CONN --connection CONN --database-name NAME \
  --candidate-sha SHA --source-tree TREE --object-sha256 SHA256

Runs bounded, warm-cache, raw-timing workflows through an already-instrumented
pg_accel_bench binary. The target database must not already exist. It is created
for this run and force-dropped on every exit path. Instrumented performance is
never eligible as release-performance evidence. PG_ACCEL_EXPECTED_DYLIB must
name the instrumented extension object loaded by the target PostgreSQL cluster.
EOF
}

sha256_file() {
    local path="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$path" | awk '{print $1}'
    else
        shasum -a 256 "$path" | awk '{print $1}'
    fi
}

sha256_text() {
    if command -v sha256sum >/dev/null 2>&1; then
        printf '%s' "$1" | sha256sum | awk '{print $1}'
    else
        printf '%s' "$1" | shasum -a 256 | awk '{print $1}'
    fi
}

normalize_sha256() {
    printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

require_nonempty_file() {
    local path="$1"
    [ -f "$path" ] && [ -s "$path" ] || die "required evidence is missing or empty: $path"
}

assert_profile_for_label() {
    local label="$1"
    local directory="$2"
    [ -d "$directory" ] || die "profile directory is missing: $directory"
    if ! find "$directory" -type f -name "${label}-*.profraw" -size +0c -print -quit \
        2>/dev/null | grep -q .; then
        die "instrumented command produced no nonempty raw profile: $label"
    fi
}

# This validator is deliberately reusable from the Python adversarial tests.
# Every command is an argv array; the harness never evaluates command text.
assert_safe_bench_command() {
    [ "$#" -gt 0 ] || die "empty benchmark command"
    case "$1" in
        --help | setup | run | validate | provenance | crash-repro | phase9-gate | \
            fp64-calibrate | report | resume) ;;
        *) die "benchmark command is outside the coverage allowlist: $1" ;;
    esac

    local -a argv=("$@")
    local index token next
    local command="${argv[0]}"
    local workload_value=""
    local rows_value=""
    local iterations_value=""
    local warmup_value=""
    local cache_value=""
    local timing_value=""
    local artifacts_value=""
    local connection_value=""
    local format_value=""
    local seed_value=""
    local multipliers_value=""
    local max_size_value=""
    local output_dir_value=""
    local category_seen=0
    local dry_run_seen=0
    local capture_plans_seen=0
    local skip_guc_verify_seen=0
    local capture_planner_stages_seen=0
    local native_parity_pairing_seen=0
    for ((index = 1; index < ${#argv[@]}; index++)); do
        token="${argv[index]}"
        case "$token" in
            sudo | osascript | purge | clear-jit | gpu-test-cold | cold | both | \
                --realistic-gucs | --realistic-gucs=*)
                die "forbidden coverage command or mode: $token"
                ;;
            metal-ship-gate | phase6-gate)
                die "unbounded or cold-cache gate is forbidden in live Rust coverage: $token"
                ;;
            --cache-mode)
                [ $((index + 1)) -lt ${#argv[@]} ] || die "--cache-mode requires a value"
                next="${argv[index + 1]}"
                [ "$next" = "warm" ] || die "coverage cache mode must be warm, got: $next"
                cache_value="$next"
                index=$((index + 1))
                ;;
            --cache-mode=*)
                [ "${token#*=}" = "warm" ] || \
                    die "coverage cache mode must be warm, got: ${token#*=}"
                cache_value="${token#*=}"
                ;;
            --timing)
                [ $((index + 1)) -lt ${#argv[@]} ] || die "--timing requires a value"
                next="${argv[index + 1]}"
                [ "$next" = "raw" ] || die "coverage timing mode must be raw, got: $next"
                timing_value="$next"
                index=$((index + 1))
                ;;
            --timing=*)
                [ "${token#*=}" = "raw" ] || \
                    die "coverage timing mode must be raw, got: ${token#*=}"
                timing_value="${token#*=}"
                ;;
            --workload | --rows | --iterations | --warmup | --artifacts-dir | --connection | \
                --format | --seed | --multipliers | --max-size | --output-dir | --category)
                [ $((index + 1)) -lt ${#argv[@]} ] || die "$token requires a value"
                next="${argv[index + 1]}"
                case "$token" in
                    --workload) workload_value="$next" ;;
                    --rows) rows_value="$next" ;;
                    --iterations) iterations_value="$next" ;;
                    --warmup) warmup_value="$next" ;;
                    --artifacts-dir) artifacts_value="$next" ;;
                    --connection) connection_value="$next" ;;
                    --format) format_value="$next" ;;
                    --seed) seed_value="$next" ;;
                    --multipliers) multipliers_value="$next" ;;
                    --max-size) max_size_value="$next" ;;
                    --output-dir) output_dir_value="$next" ;;
                    --category) category_seen=1 ;;
                esac
                index=$((index + 1))
                ;;
            --workload=*) workload_value="${token#*=}" ;;
            --rows=*) rows_value="${token#*=}" ;;
            --iterations=*) iterations_value="${token#*=}" ;;
            --warmup=*) warmup_value="${token#*=}" ;;
            --artifacts-dir=*) artifacts_value="${token#*=}" ;;
            --connection=*) connection_value="${token#*=}" ;;
            --format=*) format_value="${token#*=}" ;;
            --seed=*) seed_value="${token#*=}" ;;
            --multipliers=*) multipliers_value="${token#*=}" ;;
            --max-size=*) max_size_value="${token#*=}" ;;
            --output-dir=*) output_dir_value="${token#*=}" ;;
            --category=*) category_seen=1 ;;
            --dry-run) dry_run_seen=1 ;;
            --capture-plans) capture_plans_seen=1 ;;
            --skip-guc-verify) skip_guc_verify_seen=1 ;;
            --capture-planner-stages) capture_planner_stages_seen=1 ;;
            --native-parity-pairing) native_parity_pairing_seen=1 ;;
            *) die "unrecognized benchmark option or positional token: $token" ;;
        esac
    done

    if [ "$command" != "crash-repro" ] && \
        { [ "$capture_planner_stages_seen" -ne 0 ] || \
          [ "$native_parity_pairing_seen" -ne 0 ]; }; then
        die "planner-stage and native-parity coverage options are restricted to the exact crash-repro contract"
    fi

    # Setup and non-dry-run `run` are deliberately narrower than the general
    # CLI allowlist. Pinning the one tiny raster lane and its exact sampling
    # budget prevents a future coverage edit from turning this evidence path
    # into an unbounded benchmark sweep.
    case "$command" in
        setup)
            [ -n "$coverage_live_expected_connection" ] || \
                die "coverage setup has no bound target connection"
            [ "$connection_value" = "$coverage_live_expected_connection" ] || \
                die "coverage setup must use the harness target connection"
            [ "$workload_value" = "raster_ndvi" ] || \
                die "coverage setup is limited to raster_ndvi"
            [ "$rows_value" = "100" ] || \
                die "coverage setup requires exactly 100 rows"
            [ "$seed_value" = "42" ] || \
                die "coverage setup requires deterministic seed 42"
            [ "$category_seen" -eq 0 ] || \
                die "coverage setup cannot use a category sweep"
            ;;
        run)
            if [ "$dry_run_seen" -eq 0 ]; then
                [ -n "$coverage_live_expected_connection" ] || \
                    die "live coverage run has no bound target connection"
                [ -n "$coverage_live_expected_artifact_root" ] || \
                    die "live coverage run has no bound artifact root"
                [ "$connection_value" = "$coverage_live_expected_connection" ] || \
                    die "live coverage run must use the harness target connection"
                [ "$artifacts_value" = \
                    "$coverage_live_expected_artifact_root/normal-run-raster" ] || \
                    die "live coverage run must use its harness artifact directory"
                [ "$workload_value" = "raster_ndvi" ] || \
                    die "live coverage run is limited to raster_ndvi"
                [ "$iterations_value" = "10" ] || \
                    die "live coverage run requires exactly 10 measured iterations"
                [ "$warmup_value" = "5" ] || \
                    die "live coverage run requires exactly 5 warmups"
                [ "$cache_value" = "warm" ] || \
                    die "live coverage run requires an explicit warm cache mode"
                [ "$timing_value" = "raw" ] || \
                    die "live coverage run requires explicit raw timing"
                [ "$seed_value" = "42" ] || \
                    die "live coverage run requires deterministic seed 42"
                [ "$format_value" = "csv" ] || \
                    die "live coverage run requires CSV output"
                [ "$capture_plans_seen" -eq 1 ] || \
                    die "live coverage run requires plan capture"
                [ "$skip_guc_verify_seen" -eq 1 ] || \
                    die "live coverage run requires explicit GUC-verification bypass"
                [ "$category_seen" -eq 0 ] || \
                    die "live coverage run cannot use a category sweep"
            fi
            ;;
        crash-repro)
            [ -n "$coverage_live_expected_connection" ] || \
                die "live coverage crash-repro has no bound target connection"
            [ -n "$coverage_live_expected_artifact_root" ] || \
                die "live coverage crash-repro has no bound artifact root"
            [ "$connection_value" = "$coverage_live_expected_connection" ] || \
                die "live coverage crash-repro must use the harness target connection"
            local expected_crash_artifact=""
            local crash_cell="${workload_value}@${rows_value}"
            case "$crash_cell" in
                grouped_agg_int4@1000000) expected_crash_artifact="selected" ;;
                window_full_output_decline@10000) expected_crash_artifact="declined" ;;
                reduce_f64_minmax@100000) expected_crash_artifact="fp64-native" ;;
                mixed_join_agg_int4@100000) expected_crash_artifact="mixed-resident" ;;
                ssbm_resident_int4_star@100000) expected_crash_artifact="ssbm-resident" ;;
                hash_join@100000) expected_crash_artifact="hash-join" ;;
                h3_cell_to_parent@100000) expected_crash_artifact="h3-parent" ;;
                spatial_resident_agg_candidate@1000000) expected_crash_artifact="spatial-resident" ;;
                raster_resident_exact_reclass@10000) expected_crash_artifact="raster-resident" ;;
                spatial_mega_1kv@80000) expected_crash_artifact="spatial-mega" ;;
                raster_reclass@100) expected_crash_artifact="raster-reclass" ;;
                *) die "live coverage crash-repro cell is outside the bounded matrix" ;;
            esac
            [ "$artifacts_value" = \
                "$coverage_live_expected_artifact_root/$expected_crash_artifact" ] || \
                die "live coverage crash-repro must use its exact harness artifact directory"
            [ "$iterations_value" = "1" ] || \
                die "live coverage crash-repro requires exactly one measured iteration"
            [ "$warmup_value" = "0" ] || \
                die "live coverage crash-repro requires zero warmups"
            [ "$cache_value" = "warm" ] || \
                die "live coverage crash-repro requires an explicit warm cache mode"
            [ "$timing_value" = "raw" ] || \
                die "live coverage crash-repro requires explicit raw timing"
            [ "$format_value" = "json" ] || \
                die "live coverage crash-repro requires JSON output"
            [ "$seed_value" = "42" ] || \
                die "live coverage crash-repro requires deterministic seed 42"
            [ "$capture_plans_seen" -eq 1 ] || \
                die "live coverage crash-repro requires plan capture"
            [ "$skip_guc_verify_seen" -eq 1 ] || \
                die "live coverage crash-repro requires explicit GUC-verification bypass"
            [ "$category_seen" -eq 0 ] || \
                die "live coverage crash-repro cannot use a category sweep"
            [ "$dry_run_seen" -eq 0 ] || \
                die "live coverage crash-repro cannot be a dry run"
            case "$crash_cell" in
                window_full_output_decline@10000)
                    [ "$capture_planner_stages_seen" -eq 1 ] || \
                        die "live coverage crash-repro exact native decline requires planner-stage capture"
                    [ "$native_parity_pairing_seen" -eq 1 ] || \
                        die "live coverage crash-repro exact native decline requires same-backend native-parity pairing"
                    ;;
                *)
                    [ "$capture_planner_stages_seen" -eq 0 ] || \
                        die "live coverage crash-repro planner-stage capture is restricted to the exact native decline"
                    [ "$native_parity_pairing_seen" -eq 0 ] || \
                        die "live coverage crash-repro native-parity pairing is restricted to the exact native decline"
                    ;;
            esac
            ;;
        phase9-gate)
            [ -n "$coverage_live_expected_connection" ] || \
                die "live coverage phase9-gate has no bound target connection"
            [ -n "$coverage_live_expected_artifact_root" ] || \
                die "live coverage phase9-gate has no bound artifact root"
            [ "$connection_value" = "$coverage_live_expected_connection" ] || \
                die "live coverage phase9-gate must use the harness target connection"
            [ "$artifacts_value" = "$coverage_live_expected_artifact_root/phase9" ] || \
                die "live coverage phase9-gate must use its harness artifact directory"
            ;;
        fp64-calibrate)
            [ -n "$coverage_live_expected_connection" ] || \
                die "live coverage fp64-calibrate has no bound target connection"
            [ -n "$coverage_live_expected_artifact_root" ] || \
                die "live coverage fp64-calibrate has no bound artifact root"
            [ "$connection_value" = "$coverage_live_expected_connection" ] || \
                die "live coverage fp64-calibrate must use the harness target connection"
            [ "$max_size_value" = "100k" ] || \
                die "live coverage fp64-calibrate requires exactly max-size 100k"
            [ "$warmup_value" = "0" ] || \
                die "live coverage fp64-calibrate requires zero warmups"
            [ "$seed_value" = "42" ] || \
                die "live coverage fp64-calibrate requires deterministic seed 42"
            [ "$cache_value" = "warm" ] || \
                die "live coverage fp64-calibrate requires an explicit warm cache mode"
            [ "$timing_value" = "raw" ] || \
                die "live coverage fp64-calibrate requires explicit raw timing"
            [ "$capture_plans_seen" -eq 1 ] || \
                die "live coverage fp64-calibrate requires plan capture"
            [ "$skip_guc_verify_seen" -eq 1 ] || \
                die "live coverage fp64-calibrate requires explicit GUC-verification bypass"
            case "$multipliers_value" in
                16)
                    [ "$artifacts_value" = \
                        "$coverage_live_expected_artifact_root/fp64-calibration" ] || \
                        die "live coverage fp64-calibrate must use its harness artifact directory"
                    ;;
                0.5)
                    # Exact parser-negative branch retained for CLI error coverage.
                    [ "$artifacts_value" = \
                        "$coverage_live_expected_artifact_root/fp64-calibration-invalid" ] || \
                        die "live coverage invalid fp64 probe must use its harness artifact directory"
                    ;;
                *) die "live coverage fp64-calibrate multiplier is outside the bounded contract" ;;
            esac
            ;;
        provenance)
            [ -n "$coverage_live_expected_connection" ] || \
                die "live coverage provenance has no bound target connection"
            case "$connection_value" in
                "$coverage_live_expected_connection" | \
                    "host=/__pg_accel_coverage_missing_socket__ connect_timeout=1 dbname=missing") ;;
                *) die "live coverage provenance must use the harness target or fixed missing socket" ;;
            esac
            ;;
        resume)
            [ -n "$coverage_live_expected_connection" ] || \
                die "live coverage resume has no bound target connection"
            [ -n "$coverage_live_expected_artifact_root" ] || \
                die "live coverage resume has no bound artifact root"
            [ "$dry_run_seen" -eq 1 ] || \
                die "live coverage resume is limited to dry-run planning"
            [ "$connection_value" = "$coverage_live_expected_connection" ] || \
                die "live coverage resume must use the harness target connection"
            [ "$output_dir_value" = \
                "$coverage_live_expected_artifact_root/resume-output" ] || \
                die "live coverage resume must use its harness output directory"
            [ "$format_value" = "json" ] || \
                die "live coverage resume requires JSON output"
            case "$artifacts_value" in
                "$coverage_live_expected_artifact_root/declined" | \
                    "$coverage_live_expected_artifact_root/resume-missing") ;;
                *) die "live coverage resume source is outside the harness artifact root" ;;
            esac
            ;;
    esac
}

if [ "${PGACCEL_COVERAGE_LIVE_LIBRARY_ONLY:-0}" = "1" ]; then
    # shellcheck disable=SC2317  # exit is the executable-script fallback
    return 0 2>/dev/null || exit 0
fi

repo_root=""
build_dir=""
artifact_dir=""
bench_bin=""
profile_dir=""
psql_bin=""
admin_connection=""
connection=""
database_name=""
candidate_sha=""
source_tree=""
expected_object_sha=""
expected_extension_bin="${PG_ACCEL_EXPECTED_DYLIB:-}"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --repo-root) repo_root="${2:-}"; shift 2 ;;
        --build-dir) build_dir="${2:-}"; shift 2 ;;
        --artifact-dir) artifact_dir="${2:-}"; shift 2 ;;
        --bench-bin) bench_bin="${2:-}"; shift 2 ;;
        --profile-dir) profile_dir="${2:-}"; shift 2 ;;
        --psql-bin) psql_bin="${2:-}"; shift 2 ;;
        --admin-connection) admin_connection="${2:-}"; shift 2 ;;
        --connection) connection="${2:-}"; shift 2 ;;
        --database-name) database_name="${2:-}"; shift 2 ;;
        --candidate-sha) candidate_sha="${2:-}"; shift 2 ;;
        --source-tree) source_tree="${2:-}"; shift 2 ;;
        --object-sha256) expected_object_sha="${2:-}"; shift 2 ;;
        -h | --help) usage; exit 0 ;;
        *) printf 'error: unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done

for required in repo_root build_dir artifact_dir bench_bin profile_dir psql_bin \
    admin_connection connection database_name candidate_sha source_tree expected_object_sha; do
    value="${!required}"
    [ -n "$value" ] || { usage >&2; die "missing required argument: --${required//_/-}"; }
    case "$value" in
        *$'\n'* | *$'\r'* | *$'\t'*) die "control character in --${required//_/-}" ;;
    esac
done

[[ "$candidate_sha" =~ ^[0-9a-fA-F]{40,64}$ ]] || die "invalid candidate SHA"
[[ "$source_tree" =~ ^[0-9a-fA-F]{40,64}$ ]] || die "invalid source tree identity"
[[ "$expected_object_sha" =~ ^[0-9a-fA-F]{64}$ ]] || die "invalid object SHA-256"
[[ "$database_name" =~ ^${SAFE_DATABASE_PREFIX}[a-z0-9_]{1,40}$ ]] || \
    die "database name must match ${SAFE_DATABASE_PREFIX}[a-z0-9_]{1,40}"

repo_root="$(cd "$repo_root" && pwd -P)"
build_dir="$(cd "$build_dir" && pwd -P)"
[ -x "$bench_bin" ] && [ -f "$bench_bin" ] || die "benchmark object is not executable: $bench_bin"
bench_bin="$(cd "$(dirname "$bench_bin")" && pwd -P)/$(basename "$bench_bin")"
[ -n "$expected_extension_bin" ] || \
    die "PG_ACCEL_EXPECTED_DYLIB must bind the instrumented extension object"
[ -f "$expected_extension_bin" ] || \
    die "instrumented extension object is missing: $expected_extension_bin"
expected_extension_bin="$(cd "$(dirname "$expected_extension_bin")" && pwd -P)/$(basename "$expected_extension_bin")"
[ -x "$psql_bin" ] && [ -f "$psql_bin" ] || die "psql is not executable: $psql_bin"
psql_bin="$(cd "$(dirname "$psql_bin")" && pwd -P)/$(basename "$psql_bin")"
case "$bench_bin" in
    "$build_dir"/*) ;;
    *) die "benchmark object must be beneath the explicit build directory" ;;
esac
case "$expected_extension_bin" in
    "$build_dir"/*) ;;
    *) die "instrumented extension object must be beneath the explicit build directory" ;;
esac

cd "$repo_root"
actual_candidate="$(git rev-parse --verify HEAD)"
actual_tree="$(git rev-parse 'HEAD^{tree}')"
[ "$actual_candidate" = "$candidate_sha" ] || \
    die "candidate mismatch: expected $candidate_sha, found $actual_candidate"
[ "$actual_tree" = "$source_tree" ] || \
    die "source tree mismatch: expected $source_tree, found $actual_tree"
[ -z "$(git status --porcelain --untracked-files=normal)" ] || \
    die "live Rust coverage requires an exact clean source tree"

actual_object_sha="$(sha256_file "$bench_bin")"
[ "$actual_object_sha" = "$(normalize_sha256 "$expected_object_sha")" ] || \
    die "instrumented benchmark object hash mismatch"
expected_extension_sha="$(sha256_file "$expected_extension_bin")"

if [ -e "$artifact_dir" ]; then
    [ -d "$artifact_dir" ] || die "artifact path is not a directory: $artifact_dir"
    [ -z "$(find "$artifact_dir" -type f -print -quit)" ] || \
        die "artifact directory must start without files: $artifact_dir"
else
    mkdir -p "$artifact_dir"
fi
artifact_dir="$(cd "$artifact_dir" && pwd -P)"
coverage_live_expected_connection="$connection"
coverage_live_expected_artifact_root="$artifact_dir"
mkdir -p "$profile_dir"
profile_dir="$(cd "$profile_dir" && pwd -P)"
[ -z "$(find "$profile_dir" -type f -name '*.profraw' -print -quit)" ] || \
    die "profile directory must start without raw profiles: $profile_dir"
case "$profile_dir" in
    "$artifact_dir"/* | "$build_dir"/*) ;;
    *) die "profile directory must be beneath the artifact or build directory" ;;
esac

mkdir -p "$artifact_dir"/{logs,selected,declined,fp64-native,mixed-resident,ssbm-resident,hash-join,h3-parent,spatial-resident,raster-resident,spatial-mega,raster-reclass,phase9,fp64-calibration,resume-output,resume-missing}
ledger="$artifact_dir/command-ledger.tsv"
external_ledger="$artifact_dir/external-command-ledger.tsv"
printf 'label\texpected_exit\tactual_exit\tprofile_files\tlog\tcommand\n' > "$ledger"
printf 'label\texpected_exit\tactual_exit\tlog\tcommand\n' > "$external_ledger"

source_hashes="$artifact_dir/source-hashes.tsv"
: > "$source_hashes"
while IFS= read -r -d '' source; do
    [ -f "$source" ] || die "tracked coverage source disappeared: $source"
    printf '%s\t%s\n' "$(sha256_file "$source")" "$source" >> "$source_hashes"
done < <(git ls-files -z -- Cargo.toml Cargo.lock pg_accel_bench/Cargo.toml pg_accel_bench/src)
require_nonempty_file "$source_hashes"
printf '%s  %s\n' "$actual_object_sha" "$bench_bin" > "$artifact_dir/object.sha256"

provenance_path="$artifact_dir/provenance.json"
env COVERAGE_LIVE_SCHEMA_VERSION="$COVERAGE_LIVE_SCHEMA_VERSION" \
COVERAGE_LIVE_CANDIDATE="$actual_candidate" COVERAGE_LIVE_TREE="$actual_tree" \
COVERAGE_LIVE_OBJECT_SHA="$actual_object_sha" COVERAGE_LIVE_REPO_ROOT="$repo_root" \
COVERAGE_LIVE_BUILD_DIR="$build_dir" COVERAGE_LIVE_BENCH_BIN="$bench_bin" \
COVERAGE_LIVE_EXTENSION_BIN="$expected_extension_bin" \
COVERAGE_LIVE_EXTENSION_SHA="$expected_extension_sha" \
COVERAGE_LIVE_PROFILE_DIR="$profile_dir" COVERAGE_LIVE_ARTIFACT_DIR="$artifact_dir" \
COVERAGE_LIVE_CONNECTION_SHA="$(sha256_text "$connection")" \
COVERAGE_LIVE_ADMIN_CONNECTION_SHA="$(sha256_text "$admin_connection")" \
    python3 - "$provenance_path" <<'PY'
import json
import os
import pathlib
import sys

document = {
    "schema_version": int(os.environ["COVERAGE_LIVE_SCHEMA_VERSION"]),
    "candidate_sha": os.environ["COVERAGE_LIVE_CANDIDATE"],
    "source_tree": os.environ["COVERAGE_LIVE_TREE"],
    "instrumented_object": os.environ["COVERAGE_LIVE_BENCH_BIN"],
    "instrumented_object_sha256": os.environ["COVERAGE_LIVE_OBJECT_SHA"],
    "instrumented_extension": os.environ["COVERAGE_LIVE_EXTENSION_BIN"],
    "instrumented_extension_sha256": os.environ["COVERAGE_LIVE_EXTENSION_SHA"],
    "repo_root": os.environ["COVERAGE_LIVE_REPO_ROOT"],
    "build_dir": os.environ["COVERAGE_LIVE_BUILD_DIR"],
    "profile_dir": os.environ["COVERAGE_LIVE_PROFILE_DIR"],
    "artifact_dir": os.environ["COVERAGE_LIVE_ARTIFACT_DIR"],
    "connection_sha256": os.environ["COVERAGE_LIVE_CONNECTION_SHA"],
    "admin_connection_sha256": os.environ["COVERAGE_LIVE_ADMIN_CONNECTION_SHA"],
    "instrumented_coverage_only": True,
    "performance_evidence_eligible": False,
    "cache_policy": "warm-only",
    "timing_policy": "raw command path under instrumentation; timings are discarded",
}
pathlib.Path(sys.argv[1]).write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
PY

rendered_command=""
render_command() {
    local executable="$1"
    shift
    local token shown
    printf -v rendered_command '%q' "$executable"
    for token in "$@"; do
        shown="$token"
        if [ "$shown" = "$connection" ]; then
            shown="<explicit-target-connection>"
        elif [ "$shown" = "$admin_connection" ]; then
            shown="<explicit-admin-connection>"
        fi
        printf -v shown '%q' "$shown"
        rendered_command+=" $shown"
    done
}

run_bench() {
    local label="$1"
    local expected="$2"
    shift 2
    assert_safe_bench_command "$@"
    local log="$artifact_dir/logs/${label}.log"
    local rc profile_count
    render_command "$bench_bin" "$@"
    set +e
    LLVM_PROFILE_FILE="$profile_dir/${label}-%p-%m.profraw" \
        CARGO_TARGET_DIR="$build_dir" "$bench_bin" "$@" 2>&1 | tee "$log"
    rc=${PIPESTATUS[0]}
    set -e
    assert_profile_for_label "$label" "$profile_dir"
    profile_count="$(find "$profile_dir" -type f -name "${label}-*.profraw" -size +0c | wc -l | tr -d ' ')"
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$label" "$expected" "$rc" "$profile_count" "$log" "$rendered_command" >> "$ledger"
    require_nonempty_file "$log"
    case "$expected" in
        any01) [ "$rc" -eq 0 ] || [ "$rc" -eq 1 ] || die "$label exited $rc, expected 0 or 1" ;;
        *) [ "$rc" -eq "$expected" ] || die "$label exited $rc, expected $expected" ;;
    esac
}

run_bench_input() {
    local label="$1"
    local expected="$2"
    local input="$3"
    shift 3
    require_nonempty_file "$input"
    assert_safe_bench_command "$@"
    local log="$artifact_dir/logs/${label}.log"
    local rc profile_count
    render_command "$bench_bin" "$@"
    rendered_command+=" < $(printf '%q' "$input")"
    set +e
    LLVM_PROFILE_FILE="$profile_dir/${label}-%p-%m.profraw" \
        CARGO_TARGET_DIR="$build_dir" "$bench_bin" "$@" < "$input" 2>&1 | tee "$log"
    rc=${PIPESTATUS[0]}
    set -e
    assert_profile_for_label "$label" "$profile_dir"
    profile_count="$(find "$profile_dir" -type f -name "${label}-*.profraw" -size +0c | wc -l | tr -d ' ')"
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$label" "$expected" "$rc" "$profile_count" "$log" "$rendered_command" >> "$ledger"
    require_nonempty_file "$log"
    [ "$rc" -eq "$expected" ] || die "$label exited $rc, expected $expected"
}

run_psql() {
    local label="$1"
    local expected="$2"
    local conn="$3"
    shift 3
    local log="$artifact_dir/logs/${label}.log"
    local rc
    render_command "$psql_bin" "$conn" "$@"
    set +e
    "$psql_bin" "$conn" "$@" 2>&1 | tee "$log"
    rc=${PIPESTATUS[0]}
    set -e
    printf '%s\t%s\t%s\t%s\t%s\n' \
        "$label" "$expected" "$rc" "$log" "$rendered_command" >> "$external_ledger"
    [ "$rc" -eq "$expected" ] || die "$label exited $rc, expected $expected"
}

database_created=0
cleanup_on_exit() {
    local prior_status=$?
    local cleanup_status=0
    trap - EXIT INT TERM
    if [ "$database_created" -eq 1 ]; then
        set +e
        "$psql_bin" "$admin_connection" -X -v ON_ERROR_STOP=1 \
            -c "DROP DATABASE IF EXISTS \"$database_name\" WITH (FORCE)" \
            > "$artifact_dir/logs/database-cleanup-fallback.log" 2>&1
        cleanup_status=$?
        set -e
    fi
    if [ "$cleanup_status" -ne 0 ]; then
        printf 'error: fallback database cleanup failed with exit %s\n' "$cleanup_status" >&2
        prior_status=1
    fi
    exit "$prior_status"
}
trap cleanup_on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Dispatch, workload listing, validation, and fail-closed CLI parsing require no DB.
run_bench cli_help 0 --help
run_bench workload_list 0 run --dry-run --workload grouped_agg_int4 \
    --iterations 10 --warmup 5 --timing raw --cache-mode warm
run_bench validate_fp64_registry 0 validate --category fp64_matrix --rows 100000
run_bench invalid_workload 1 validate --workload coverage_missing_workload --rows 10000
grep -q 'unknown workload' "$artifact_dir/logs/invalid_workload.log" || \
    die "invalid workload failure did not reach the CLI error branch"
run_bench validate_all_bounded 0 validate --rows 100
grep -Eq '\[validate\] all [0-9]+ workload\(s\) passed validation' \
    "$artifact_dir/logs/validate_all_bounded.log" || \
    die "bounded all-workload validation did not retain its completion proof"
run_bench workload_matrix_dry_run 0 run --dry-run \
    --iterations 10 --warmup 5 --timing raw --cache-mode warm
grep -Eq '^=== All [0-9]+ workload\(s\) validated ===$' \
    "$artifact_dir/logs/workload_matrix_dry_run.log" || \
    die "all-workload dry run did not retain its completion proof"

# The database name is validated above and must be absent so cleanup can never
# destroy pre-existing state.
run_psql database_preflight 0 "$admin_connection" -X -A -t -v ON_ERROR_STOP=1 \
    -c "SELECT count(*) FROM pg_database WHERE datname = '$database_name'"
preexisting="$(tr -d '[:space:]' < "$artifact_dir/logs/database_preflight.log")"
[ "$preexisting" = "0" ] || die "coverage database already exists: $database_name"
run_psql database_create 0 "$admin_connection" -X -v ON_ERROR_STOP=1 \
    -c "CREATE DATABASE \"$database_name\""
database_created=1
run_psql database_identity 0 "$connection" -X -A -t -v ON_ERROR_STOP=1 \
    -c 'SELECT current_database()'
observed_database="$(tr -d '[:space:]' < "$artifact_dir/logs/database_identity.log")"
[ "$observed_database" = "$database_name" ] || \
    die "target connection resolved to $observed_database, expected $database_name"
run_psql database_extensions 0 "$connection" -X -v ON_ERROR_STOP=1 \
    -f "$repo_root/sql/init/01-create-extensions.sql"

run_bench provenance_success 0 provenance --connection "$connection"
run_bench provenance_failure 1 provenance \
    --connection 'host=/__pg_accel_coverage_missing_socket__ connect_timeout=1 dbname=missing'
grep -Eq 'provenance.*failure|could not connect|No such file' \
    "$artifact_dir/logs/provenance_failure.log" || \
    die "provenance failure branch did not emit diagnostic evidence"

# Exercise the dedicated setup command with a fixed 100-row/16x16-tile raster
# fixture. The command validator rejects every other setup workload and size.
run_bench bounded_setup 0 setup --workload raster_ndvi --rows 100 --seed 42 \
    --connection "$connection"
grep -q '\[setup\] raster_ndvi -- seed 42 .* 100 rows' \
    "$artifact_dir/logs/bounded_setup.log" || \
    die "bounded setup did not retain its workload/row proof"

# Selected aggregate and resident-domain lanes, intentional native declines,
# and the fixed bounded Phase 9 matrix cover runner/config/stats/artifact/report
# paths. Selected winners may return 1 because their release ship gates demand
# performance evidence that instrumented warm-only coverage cannot provide. The
# durable reports are consumed below and must still prove selection and dispatch.
run_bench selected_crash_repro any01 crash-repro --workload grouped_agg_int4 --rows 1000000 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/selected"
run_bench declined_crash_repro 0 crash-repro --workload window_full_output_decline --rows 10000 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --capture-planner-stages --native-parity-pairing \
    --artifacts-dir "$artifact_dir/declined"
run_bench fp64_native_crash_repro 0 crash-repro --workload reduce_f64_minmax --rows 100000 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/fp64-native"

# One exact cell per remaining resident/domain path keeps the live matrix small
# while reaching the real loaders, descriptors, private data, executors, and
# extension adapters. Released resident spatial/raster cells must select and
# dispatch; neighboring unreleased spatial/raster shapes must retain an exact
# planner-reported native decline.
run_bench mixed_resident_crash_repro any01 crash-repro \
    --workload mixed_join_agg_int4 --rows 100000 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/mixed-resident"
run_bench ssbm_resident_crash_repro any01 crash-repro \
    --workload ssbm_resident_int4_star --rows 100000 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/ssbm-resident"
run_bench hash_join_crash_repro any01 crash-repro --workload hash_join --rows 100000 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/hash-join"
run_bench h3_parent_crash_repro any01 crash-repro \
    --workload h3_cell_to_parent --rows 100000 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/h3-parent"
run_bench spatial_resident_crash_repro any01 crash-repro \
    --workload spatial_resident_agg_candidate --rows 1000000 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/spatial-resident"
run_bench raster_resident_crash_repro any01 crash-repro \
    --workload raster_resident_exact_reclass --rows 10000 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/raster-resident"
run_bench spatial_mega_decline 0 crash-repro --workload spatial_mega_1kv --rows 80000 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/spatial-mega"
run_bench raster_reclass_decline 0 crash-repro --workload raster_reclass --rows 100 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/raster-reclass"
for domain_report in \
    "$artifact_dir/mixed-resident/report.json" \
    "$artifact_dir/ssbm-resident/report.json" \
    "$artifact_dir/hash-join/report.json" \
    "$artifact_dir/h3-parent/report.json" \
    "$artifact_dir/spatial-resident/report.json" \
    "$artifact_dir/raster-resident/report.json" \
    "$artifact_dir/spatial-mega/report.json" \
    "$artifact_dir/raster-reclass/report.json"; do
    require_nonempty_file "$domain_report"
done

run_bench phase9_bounded 0 phase9-gate --connection "$connection" \
    --artifacts-dir "$artifact_dir/phase9"

# Cover the normal `run_all_with_config` path (distinct from crash-repro and
# selected-cell gates) with the raster suite's fixed 100-row smoke scale. Ten
# measured iterations and five warmups satisfy its real statistical contract
# while remaining a small, warm-only coverage workload.
run_bench normal_run_raster 0 run --workload raster_ndvi \
    --iterations 10 --warmup 5 --seed 42 --connection "$connection" --format csv \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/normal-run-raster"
require_nonempty_file "$artifact_dir/normal-run-raster/report.json"
grep -q '^workload,category,kernel_class,' \
    "$artifact_dir/logs/normal_run_raster.log" || \
    die "normal bounded run did not emit its requested CSV report"

# Re-consume a stored report through stdin, then cover malformed report input.
run_bench_input report_from_artifact 0 "$artifact_dir/fp64-native/report.json" \
    report --format markdown
grep -q '^# pg_accel Benchmark Report$' \
    "$artifact_dir/logs/report_from_artifact.log" || \
    die "stored report did not render as markdown"
run_bench_input report_json_from_artifact 0 "$artifact_dir/fp64-native/report.json" \
    report --format json
python3 -m json.tool "$artifact_dir/logs/report_json_from_artifact.log" >/dev/null || \
    die "stored report did not render as valid JSON"
run_bench_input report_csv_from_artifact 0 "$artifact_dir/fp64-native/report.json" \
    report --format csv
grep -q '^workload,category,kernel_class,' \
    "$artifact_dir/logs/report_csv_from_artifact.log" || \
    die "stored report did not render as CSV"
printf '{not valid json}\n' > "$artifact_dir/malformed-report.json"
run_bench_input report_failure 1 "$artifact_dir/malformed-report.json" report --format json
grep -qi 'error' "$artifact_dir/logs/report_failure.log" || \
    die "malformed report did not reach the CLI error branch"

# A completed artifact has an empty retry plan. A missing manifest must fail.
run_bench resume_empty 0 resume --artifacts-dir "$artifact_dir/declined" \
    --connection "$connection" --output-dir "$artifact_dir/resume-output" --format json --dry-run
run_bench resume_missing_evidence 1 resume --artifacts-dir "$artifact_dir/resume-missing" \
    --connection "$connection" --output-dir "$artifact_dir/resume-output" --format json --dry-run
grep -q 'resume manifest not readable' "$artifact_dir/logs/resume_missing_evidence.log" || \
    die "missing resume evidence did not fail closed"

# One bounded valid sweep reaches fp64 calibration orchestration. Its 0/1 exit
# is intentionally not a coverage verdict: instrumentation can change parity.
run_bench fp64_calibration any01 fp64-calibrate --connection "$connection" \
    --multipliers 16 --max-size 100k --warmup 0 --seed 42 --capture-plans \
    --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/fp64-calibration"
require_nonempty_file "$artifact_dir/fp64-calibration/fp64_calibration_summary.json"
run_bench fp64_invalid_multiplier 1 fp64-calibrate --connection "$connection" \
    --multipliers 0.5 --max-size 100k --warmup 0 --seed 42 --capture-plans \
    --timing raw --cache-mode warm \
    --skip-guc-verify --artifacts-dir "$artifact_dir/fp64-calibration-invalid"
grep -q 'fp64 multiplier must be finite' "$artifact_dir/logs/fp64_invalid_multiplier.log" || \
    die "invalid fp64 multiplier did not reach the parser failure branch"

# Independently consume the durable outputs. No speed or latency value is used
# as a pass condition under instrumentation.
python3 - \
    "$artifact_dir/selected/report.json" \
    "$artifact_dir/declined/report.json" \
    "$artifact_dir/fp64-native/report.json" \
    "$artifact_dir/mixed-resident/report.json" \
    "$artifact_dir/ssbm-resident/report.json" \
    "$artifact_dir/hash-join/report.json" \
    "$artifact_dir/h3-parent/report.json" \
    "$artifact_dir/spatial-resident/report.json" \
    "$artifact_dir/raster-resident/report.json" \
    "$artifact_dir/spatial-mega/report.json" \
    "$artifact_dir/raster-reclass/report.json" \
    "$artifact_dir/phase9/report.json" \
    "$artifact_dir/normal-run-raster/report.json" \
    "$artifact_dir/fp64-calibration/fp64_calibration_summary.json" \
    "$artifact_dir/evidence-validation.json" \
    "$artifact_dir/selected/provenance.json" \
    "$expected_extension_sha" <<'PY'
import json
import pathlib
import sys

(
    selected_path,
    declined_path,
    fp64_path,
    mixed_path,
    ssbm_path,
    hash_path,
    h3_path,
    spatial_resident_path,
    raster_resident_path,
    spatial_path,
    raster_reclass_path,
    phase9_path,
    raster_path,
    calibration_path,
    output_path,
) = map(pathlib.Path, sys.argv[1:16])
extension_provenance_path = pathlib.Path(sys.argv[16])
expected_extension_sha = sys.argv[17]

def load(path):
    if not path.is_file() or path.stat().st_size == 0:
        raise SystemExit(f"missing evidence: {path}")
    return json.loads(path.read_text())

def retained_artifact_path(report_path, relative_name, description):
    if not isinstance(relative_name, str) or not relative_name:
        raise SystemExit(f"{description}: artifact name is missing")
    relative = pathlib.Path(relative_name)
    if relative.is_absolute() or ".." in relative.parts:
        raise SystemExit(f"{description}: artifact path escapes the run directory")
    artifact = report_path.parent / relative
    if not artifact.is_file() or artifact.stat().st_size == 0:
        raise SystemExit(f"{description}: artifact is missing or empty: {artifact}")
    return artifact

def require_correctness_artifact(report_path, relative_name, expected_name, expected_rows):
    description = f"{expected_name} correctness diff"
    artifact_path = retained_artifact_path(report_path, relative_name, description)
    artifact = load(artifact_path)
    if not isinstance(artifact, dict) or artifact.get("schema_version") != 1:
        raise SystemExit(f"{description}: schema mismatch")
    if (artifact.get("workload"), artifact.get("rows")) != (expected_name, expected_rows):
        raise SystemExit(f"{description}: workload identity mismatch")
    if artifact.get("status") != "pass" or artifact.get("error") is not None:
        raise SystemExit(f"{description}: status is not pass")
    accel_rows = artifact.get("accel_rows")
    baseline_rows = artifact.get("baseline_rows")
    if type(accel_rows) is not int or type(baseline_rows) is not int or accel_rows != baseline_rows:
        raise SystemExit(f"{description}: result row counts are not equal")
    if (
        artifact.get("accel_minus_baseline_count") != 0
        or artifact.get("baseline_minus_accel_count") != 0
        or artifact.get("accel_minus_baseline_samples") != []
        or artifact.get("baseline_minus_accel_samples") != []
    ):
        raise SystemExit(f"{description}: bidirectional result diff is not empty")
    if type(artifact.get("order_sensitive")) is not bool or artifact.get("sample_limit") != 20:
        raise SystemExit(f"{description}: comparison contract mismatch")
    for query_field in ("accel_query_sql", "baseline_query_sql"):
        if not isinstance(artifact.get(query_field), str) or not artifact[query_field].strip():
            raise SystemExit(f"{description}: {query_field} is missing")

def one(report_path, expected_name, expected_rows, expected_iterations=1, expected_warmup=0):
    report = load(report_path)
    if report.get("crashes") != []:
        raise SystemExit(f"{expected_name}: crash evidence is not empty")
    workloads = report.get("workloads")
    if not isinstance(workloads, list) or len(workloads) != 1:
        raise SystemExit(f"{expected_name}: expected exactly one workload result")
    row = workloads[0]
    if (row.get("name"), row.get("rows")) != (expected_name, expected_rows):
        raise SystemExit(f"{expected_name}: workload identity mismatch")
    methodology = report.get("methodology", {})
    if methodology.get("cache_mode") != "warm" or methodology.get("timing_mode") != "raw-wallclock":
        raise SystemExit(f"{expected_name}: unsafe methodology")
    if methodology.get("iterations") != expected_iterations or methodology.get("warmup") != expected_warmup:
        raise SystemExit(f"{expected_name}: sampling budget mismatch")
    if not isinstance(row.get("iterations"), list) or len(row["iterations"]) != expected_iterations:
        raise SystemExit(f"{expected_name}: exact measured output was not retained")
    if not row.get("plan_snippet"):
        raise SystemExit(f"{expected_name}: captured plan snippet is missing")
    require_correctness_artifact(
        report_path,
        row.get("correctness_diff_artifact"),
        expected_name,
        expected_rows,
    )
    plans_path = report_path.parent / "plans.txt"
    if not plans_path.is_file() or plans_path.stat().st_size == 0:
        raise SystemExit(f"{expected_name}: captured plans artifact is missing or empty")
    return row

def require_selected_dispatch(row, expected_name):
    if row.get("planner_declined") or not row.get("plan_selected") or not row.get("gpu_kernel_dispatched"):
        raise SystemExit(f"{expected_name}: selected dispatch was not proven")
    if not row.get("dispatch_counter_captured") or row.get("gpu_kernel_execution_delta", 0) <= 0:
        raise SystemExit(f"{expected_name}: positive dispatch counter evidence is missing")
    if row.get("accel_output_rows_consumed", 0) <= 0:
        raise SystemExit(f"{expected_name}: accelerated output was not consumed")
    if row.get("pg_accel_stock_exec_delta") != 0:
        raise SystemExit(f"{expected_name}: stock executor fallback was observed")

def require_planner_decline(row, expected_name, expected_reason=None):
    if row.get("plan_selected") or row.get("gpu_kernel_dispatched") or not row.get("planner_declined"):
        raise SystemExit(f"{expected_name}: native decline was not proven")
    evidence = row.get("native_decline_evidence")
    if not isinstance(evidence, dict) or not evidence.get("reason"):
        raise SystemExit(f"{expected_name}: planner decline reason is missing")
    if evidence.get("source") != "planner_reported":
        raise SystemExit(f"{expected_name}: decline source is not planner_reported")
    if expected_reason is not None and evidence.get("reason") != expected_reason:
        raise SystemExit(
            f"{expected_name}: expected decline reason {expected_reason!r}, got {evidence!r}"
        )

selected = one(selected_path, "grouped_agg_int4", 1000000)
require_selected_dispatch(selected, "grouped_agg_int4")

declined_report = load(declined_path)
declined = one(declined_path, "window_full_output_decline", 10000)
require_planner_decline(declined, "window_full_output_decline")
declined_methodology = declined_report.get("methodology", {})
if (
    declined_methodology.get("native_parity_pairing") is not True
    or declined_methodology.get("native_parity_repetitions_per_arm") != 2
):
    raise SystemExit("window_full_output_decline: native-parity methodology is missing")
pair_captures = declined.get("native_parity_pair_captures")
if not isinstance(pair_captures, list) or len(pair_captures) != 1:
    raise SystemExit("window_full_output_decline: exact native-parity pair was not retained")
pair_capture = pair_captures[0]
sequence = pair_capture.get("sequence") if isinstance(pair_capture, dict) else None
if (
    not isinstance(sequence, list)
    or len(sequence) != 4
    or sequence.count("accel") != 2
    or sequence.count("disabled_postgresql") != 2
    or len(pair_capture.get("accel_ms", [])) != 2
    or len(pair_capture.get("parallel_ms", [])) != 2
):
    raise SystemExit("window_full_output_decline: ABBA/BAAB raw components are incomplete")
planner_captures = declined.get("planner_stage_captures")
if not isinstance(planner_captures, list) or len(planner_captures) != 1:
    raise SystemExit("window_full_output_decline: planner-stage capture was not retained")
planner_capture = planner_captures[0]
if (
    not isinstance(planner_capture, dict)
    or planner_capture.get("error") is not None
    or not planner_capture.get("stages")
    or not planner_capture.get("substages")
):
    raise SystemExit("window_full_output_decline: planner-stage evidence is incomplete")

fp64 = one(fp64_path, "reduce_f64_minmax", 100000)
require_planner_decline(fp64, "reduce_f64_minmax")

resident_selected_cells = [
    (mixed_path, "mixed_join_agg_int4", 100000),
    (ssbm_path, "ssbm_resident_int4_star", 100000),
    (hash_path, "hash_join", 100000),
    (h3_path, "h3_cell_to_parent", 100000),
    (spatial_resident_path, "spatial_resident_agg_candidate", 1000000),
    (raster_resident_path, "raster_resident_exact_reclass", 10000),
]
for path, name, rows in resident_selected_cells:
    require_selected_dispatch(one(path, name, rows), name)

for path, name, rows, reason in [
    (spatial_path, "spatial_mega_1kv", 80000, "generic_descriptor_capability"),
    (raster_reclass_path, "raster_reclass", 100, "shape_unsupported_rte"),
]:
    row = one(path, name, rows)
    require_planner_decline(row, name, reason)

phase9 = load(phase9_path)
phase9_contracts = {
    "window_full_output_decline": "no_gpu_resident_pipeline",
    "window_row_number": "no_gpu_resident_pipeline",
    "window_rank": "no_gpu_resident_pipeline",
    "window_dense_rank": "no_gpu_resident_pipeline",
    "window_running_sum": "no_gpu_resident_pipeline",
    "window_analytics": "no_gpu_resident_pipeline",
    "window_reducing_decline": "no_gpu_resident_pipeline",
    "semi_join_null_decline": "no_gpu_resident_pipeline",
    "in_join_null_decline": "no_gpu_resident_pipeline",
    "anti_join_null_decline": "no_gpu_resident_pipeline",
    "not_in_join_null_decline": "shape_sublink",
    "aggregate_semantic_modifier_decline": "shape_aggregate_modifier",
    "aggregate_ordered_set_decline": "shape_aggregate_modifier",
    "numeric_agg_decline": "shape_numeric_accumulator_unavailable",
    "avg_nonfloat_decline": "shape_numeric_accumulator_unavailable",
    "setop_intersect_decline": "setop_no_gpu_kernel",
    "recursive_union_decline": "recursiveunion_no_gpu_kernel",
    "mergejoin_decline": "mergejoin_no_gpu_kernel",
    "gpu_sort_multikey": "sort_multikey_no_gpu_kernel",
    "gpu_nlj_between": "shape_non_equality_join",
}
phase9_workloads = phase9.get("workloads")
if phase9.get("crashes") != [] or not isinstance(phase9_workloads, list):
    raise SystemExit("bounded Phase 9 report is incomplete")
phase9_names = [row.get("name") for row in phase9_workloads if isinstance(row, dict)]
if len(phase9_names) != len(phase9_contracts) or set(phase9_names) != set(phase9_contracts):
    raise SystemExit("bounded Phase 9 workload identities do not match the exact contract")
phase9_methodology = phase9.get("methodology", {})
if (
    phase9_methodology.get("cache_mode") != "warm"
    or phase9_methodology.get("timing_mode") != "raw-wallclock"
    or phase9_methodology.get("iterations") != 1
    or phase9_methodology.get("warmup") != 0
):
    raise SystemExit("bounded Phase 9 methodology does not match the exact contract")
for row in phase9_workloads:
    name = row["name"]
    if row.get("rows") != 10000:
        raise SystemExit(f"Phase 9 row scale mismatch: {name}")
    require_planner_decline(row, name, phase9_contracts[name])
    if not row.get("dispatch_counter_captured") or row.get("gpu_kernel_execution_delta") != 0:
        raise SystemExit(f"Phase 9 zero-dispatch counter evidence is missing: {name}")
    if row.get("function_srf_kernel_dispatched") or row.get("pg_accel_stock_exec_delta", 0) != 0:
        raise SystemExit(f"Phase 9 execution classification mismatch: {name}")
    if not isinstance(row.get("iterations"), list) or len(row["iterations"]) != 1:
        raise SystemExit(f"Phase 9 measured output is incomplete: {name}")
    if not row.get("plan_snippet"):
        raise SystemExit(f"Phase 9 plan evidence is missing: {name}")
    require_correctness_artifact(
        phase9_path,
        row.get("correctness_diff_artifact"),
        name,
        10000,
    )

raster_report = load(raster_path)
raster = one(raster_path, "raster_ndvi", 100, expected_iterations=10, expected_warmup=5)
raster_methodology = raster_report.get("methodology", {})
if raster_methodology.get("iterations") != 10 or raster_methodology.get("warmup") != 5:
    raise SystemExit("bounded normal run sampling contract mismatch")
if len(raster.get("iterations", [])) != 10:
    raise SystemExit("bounded normal run did not retain ten measured iterations")
require_planner_decline(raster, "raster_ndvi", "shape_unsupported_rte")
if not raster.get("correctness_diff_artifact") or not raster.get("plan_snippet"):
    raise SystemExit("bounded raster run is missing correctness or plan evidence")

calibration = load(calibration_path)
if calibration.get("sizes") != [100000] or calibration.get("multipliers") != [16.0]:
    raise SystemExit("bounded fp64 calibration identity mismatch")
if not isinstance(calibration.get("candidates"), list) or len(calibration["candidates"]) != 1:
    raise SystemExit("bounded fp64 calibration summary is incomplete")

extension_provenance = load(extension_provenance_path)
if extension_provenance.get("errors") != [] or extension_provenance.get("status") not in {"pass", "warning"}:
    raise SystemExit("selected cell did not retain accepted extension provenance")
for role in ("expected_binary", "installed_binary"):
    probe = extension_provenance.get(role)
    if not isinstance(probe, dict) or probe.get("sha256") != expected_extension_sha:
        raise SystemExit(f"{role} does not match the instrumented extension object")
loaded = extension_provenance.get("loaded_binaries")
if not isinstance(loaded, list) or not loaded:
    raise SystemExit("live backend extension mapping provenance is missing")
if any(not isinstance(probe, dict) or probe.get("sha256") != expected_extension_sha for probe in loaded):
    raise SystemExit("live backend mapped an extension other than the instrumented object")

validation = {
    "schema_version": 1,
    "performance_evidence_eligible": False,
    "selected_cell": "grouped_agg_int4@1000000",
    "resident_selected_cells": [f"{name}@{rows}" for _, name, rows in resident_selected_cells],
    "native_parity_cell": "window_full_output_decline@10000",
    "planner_stage_cell": "window_full_output_decline@10000",
    "domain_decline_cells": ["spatial_mega_1kv@80000", "raster_reclass@100"],
    "native_decline_cells": ["window_full_output_decline@10000", "reduce_f64_minmax@100000"],
    "phase9_cells": len(phase9_workloads),
    "bounded_normal_run": "raster_ndvi@100",
    "bounded_normal_run_iterations": len(raster["iterations"]),
    "fp64_candidates": len(calibration["candidates"]),
    "all_outputs_consumed": True,
    "extension_object_sha256": expected_extension_sha,
    "loaded_extension_hash_bound": True,
}
output_path.write_text(json.dumps(validation, indent=2, sort_keys=True) + "\n")
PY

# Drop the disposable database before sealing evidence so the cleanup log and
# exit are part of the immutable manifest.
run_psql database_cleanup 0 "$admin_connection" -X -v ON_ERROR_STOP=1 \
    -c "DROP DATABASE IF EXISTS \"$database_name\" WITH (FORCE)"
database_created=0

raw_profile_archive="$artifact_dir/raw-profiles"
mkdir -p "$raw_profile_archive"
if [ "$profile_dir" != "$raw_profile_archive" ]; then
    while IFS= read -r -d '' profile; do
        cp -p "$profile" "$raw_profile_archive/$(basename "$profile")"
    done < <(find "$profile_dir" -type f -name '*.profraw' -size +0c -print0)
fi
if ! find "$raw_profile_archive" -type f -name '*.profraw' -size +0c -print -quit | grep -q .; then
    die "no raw profiles were archived"
fi

profile_manifest="$artifact_dir/profile-manifest.tsv"
: > "$profile_manifest"
while IFS= read -r -d '' profile; do
    printf '%s\t%s\t%s\n' "$(sha256_file "$profile")" "$(stat -f '%z' "$profile" 2>/dev/null || stat -c '%s' "$profile")" \
        "${profile#"$artifact_dir"/}" >> "$profile_manifest"
done < <(find "$raw_profile_archive" -type f -name '*.profraw' -print0 | sort -z)
require_nonempty_file "$profile_manifest"

evidence_manifest="$artifact_dir/evidence.sha256"
: > "$evidence_manifest"
while IFS= read -r -d '' evidence; do
    [ "$evidence" = "$evidence_manifest" ] && continue
    printf '%s  %s\n' "$(sha256_file "$evidence")" "${evidence#"$artifact_dir"/}" >> "$evidence_manifest"
done < <(find "$artifact_dir" -type f -print0 | sort -z)
require_nonempty_file "$evidence_manifest"

printf 'coverage-live-rust: PASS (%s; performance evidence eligible=false)\n' "$artifact_dir"
