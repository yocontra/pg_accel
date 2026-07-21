#!/usr/bin/env bash
# Exercise the instrumented pg_accel_bench production CLI against a disposable
# database. This is coverage evidence only: instrumented timings are never
# accepted as benchmark or release-performance evidence.

set -euo pipefail

readonly COVERAGE_LIVE_SCHEMA_VERSION=1
readonly SAFE_DATABASE_PREFIX="pgaccel_cov_"

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
        --help | run | validate | provenance | crash-repro | phase9-gate | \
            fp64-calibrate | report | resume) ;;
        *) die "benchmark command is outside the coverage allowlist: $1" ;;
    esac

    local -a argv=("$@")
    local index token next
    for ((index = 0; index < ${#argv[@]}; index++)); do
        token="${argv[index]}"
        case "$token" in
            sudo | osascript | purge | clear-jit | gpu-test-cold | cold | both)
                die "forbidden coverage command or mode: $token"
                ;;
            metal-ship-gate | phase6-gate)
                die "unbounded or cold-cache gate is forbidden in live Rust coverage: $token"
                ;;
            --cache-mode)
                [ $((index + 1)) -lt ${#argv[@]} ] || die "--cache-mode requires a value"
                next="${argv[index + 1]}"
                [ "$next" = "warm" ] || die "coverage cache mode must be warm, got: $next"
                index=$((index + 1))
                ;;
            --cache-mode=*)
                [ "${token#*=}" = "warm" ] || \
                    die "coverage cache mode must be warm, got: ${token#*=}"
                ;;
            --timing)
                [ $((index + 1)) -lt ${#argv[@]} ] || die "--timing requires a value"
                next="${argv[index + 1]}"
                [ "$next" = "raw" ] || die "coverage timing mode must be raw, got: $next"
                index=$((index + 1))
                ;;
            --timing=*)
                [ "${token#*=}" = "raw" ] || \
                    die "coverage timing mode must be raw, got: ${token#*=}"
                ;;
        esac
    done
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
mkdir -p "$profile_dir"
profile_dir="$(cd "$profile_dir" && pwd -P)"
[ -z "$(find "$profile_dir" -type f -name '*.profraw' -print -quit)" ] || \
    die "profile directory must start without raw profiles: $profile_dir"
case "$profile_dir" in
    "$artifact_dir"/* | "$build_dir"/*) ;;
    *) die "profile directory must be beneath the artifact or build directory" ;;
esac

mkdir -p "$artifact_dir"/{logs,selected,declined,fp64-native,phase9,fp64-calibration,resume-output,resume-missing}
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
COVERAGE_LIVE_SCHEMA_VERSION="$COVERAGE_LIVE_SCHEMA_VERSION" \
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

# One selected grouped-aggregate lane, two intentional native declines, and the fixed
# bounded Phase 9 matrix cover runner/config/stats/artifact/report plan paths.
# The selected winner may return 1 because its release ship gate demands cold
# evidence that this warm-only coverage harness intentionally forbids. The
# durable report is consumed below and must still prove selection and dispatch.
run_bench selected_crash_repro any01 crash-repro --workload grouped_agg_int4 --rows 1000000 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/selected"
run_bench declined_crash_repro 0 crash-repro --workload window_full_output_decline --rows 10000 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/declined"
run_bench fp64_native_crash_repro 0 crash-repro --workload reduce_f64_minmax --rows 100000 \
    --iterations 1 --warmup 0 --seed 42 --connection "$connection" --format json \
    --capture-plans --timing raw --cache-mode warm --skip-guc-verify \
    --artifacts-dir "$artifact_dir/fp64-native"
run_bench phase9_bounded 0 phase9-gate --connection "$connection" \
    --artifacts-dir "$artifact_dir/phase9"

# Re-consume a stored report through stdin, then cover malformed report input.
run_bench_input report_from_artifact 0 "$artifact_dir/fp64-native/report.json" \
    report --format markdown
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
    --multipliers 0.5 --max-size 100k --warmup 0 --timing raw --cache-mode warm \
    --skip-guc-verify --artifacts-dir "$artifact_dir/fp64-calibration-invalid"
grep -q 'fp64 multiplier must be finite' "$artifact_dir/logs/fp64_invalid_multiplier.log" || \
    die "invalid fp64 multiplier did not reach the parser failure branch"

# Independently consume the durable outputs. No speed or latency value is used
# as a pass condition under instrumentation.
python3 - \
    "$artifact_dir/selected/report.json" \
    "$artifact_dir/declined/report.json" \
    "$artifact_dir/fp64-native/report.json" \
    "$artifact_dir/phase9/report.json" \
    "$artifact_dir/fp64-calibration/fp64_calibration_summary.json" \
    "$artifact_dir/evidence-validation.json" \
    "$artifact_dir/selected/provenance.json" \
    "$expected_extension_sha" <<'PY'
import json
import pathlib
import sys

selected_path, declined_path, fp64_path, phase9_path, calibration_path, output_path = map(pathlib.Path, sys.argv[1:7])
extension_provenance_path = pathlib.Path(sys.argv[7])
expected_extension_sha = sys.argv[8]

def load(path):
    if not path.is_file() or path.stat().st_size == 0:
        raise SystemExit(f"missing evidence: {path}")
    return json.loads(path.read_text())

def one(report, expected_name, expected_rows):
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
    if not isinstance(row.get("iterations"), list) or not row["iterations"]:
        raise SystemExit(f"{expected_name}: measured output was not retained")
    return row

selected = one(load(selected_path), "grouped_agg_int4", 1000000)
if not selected.get("plan_selected") or not selected.get("gpu_kernel_dispatched"):
    raise SystemExit("selected grouped-aggregate coverage cell did not select and dispatch")
if not selected.get("dispatch_counter_captured") or selected.get("gpu_kernel_execution_delta", 0) <= 0:
    raise SystemExit("selected grouped-aggregate coverage cell lacks dispatch evidence")
if selected.get("accel_output_rows_consumed", 0) <= 0:
    raise SystemExit("selected grouped-aggregate output was not consumed")

for path, name, rows in [
    (declined_path, "window_full_output_decline", 10000),
    (fp64_path, "reduce_f64_minmax", 100000),
]:
    row = one(load(path), name, rows)
    if not row.get("planner_declined") or row.get("gpu_kernel_dispatched") or row.get("plan_selected"):
        raise SystemExit(f"{name}: native decline was not proven")
    evidence = row.get("native_decline_evidence")
    if not isinstance(evidence, dict) or not evidence.get("reason"):
        raise SystemExit(f"{name}: planner decline reason is missing")

phase9 = load(phase9_path)
if phase9.get("crashes") != [] or len(phase9.get("workloads", [])) < 10:
    raise SystemExit("bounded Phase 9 report is incomplete")
for row in phase9["workloads"]:
    if row.get("rows") != 10000 or not row.get("planner_declined") or row.get("gpu_kernel_dispatched"):
        raise SystemExit(f"Phase 9 decline mismatch: {row.get('name')}")

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
    "native_decline_cells": ["window_full_output_decline@10000", "reduce_f64_minmax@100000"],
    "phase9_cells": len(phase9["workloads"]),
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
