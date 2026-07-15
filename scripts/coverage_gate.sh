#!/usr/bin/env bash
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root" || exit 1

pg="${1:-18}"
pg="${pg#pg}"
artifact_dir="${COVERAGE_ARTIFACT_DIR:-artifacts/coverage}"
build_root="${COVERAGE_BUILD_DIR:-target/coverage}"
scope_file="coverage/scope.json"
baseline_file="coverage/release-baseline.json"
manifest_file="coverage/sql-semantic-assertions.json"
minimum_default="${COVERAGE_MIN_PERCENT:-90}"
rust_minimum="${COVERAGE_MIN_RUST_LINES:-$minimum_default}"
cpp_minimum="${COVERAGE_MIN_CPP_LINES:-$minimum_default}"
sql_minimum="${COVERAGE_MIN_SQL_ASSERTIONS:-$minimum_default}"
test_threads="${RUST_TEST_THREADS:-1}"

# Artifact roots and a valid red schema exist before any tool, PostgreSQL,
# repository-state, or threshold prerequisite is checked.
mkdir -p "$artifact_dir"/{rust,cpp,sql,sql-reachability} "$build_root"
if command -v python3 >/dev/null 2>&1; then
    python3 scripts/coverage_tools.py init-artifacts \
        --artifact-dir "$artifact_dir" \
        --rust-threshold 90 --cpp-threshold 90 --sql-threshold 90 \
        > "$artifact_dir/init.log" 2>&1 || true
fi

artifact_dir="$(cd "$artifact_dir" && pwd -P)"
build_root="$(cd "$build_root" && pwd -P)"
aggregate_done=0
overall_status=0

# shellcheck disable=SC2329  # invoked indirectly by the EXIT trap
aggregate_on_exit() {
    local prior_status=$?
    if [ "$aggregate_done" -eq 0 ] && command -v python3 >/dev/null 2>&1; then
        python3 scripts/coverage_tools.py aggregate --artifact-dir "$artifact_dir" \
            --repo-root "$repo_root" \
            > "$artifact_dir/aggregate-on-exit.log" 2>&1 || true
    fi
    return "$prior_status"
}
trap aggregate_on_exit EXIT

mark_layer_error() {
    local layer="$1"
    local threshold="$2"
    local stage="$3"
    local message="$4"
    local exit_code="${5:-1}"
    python3 scripts/coverage_tools.py mark-layer-error \
        --artifact-dir "$artifact_dir" \
        --layer "$layer" \
        --threshold "$threshold" \
        --stage "$stage" \
        --message "$message" \
        --exit-code "$exit_code" >/dev/null 2>&1 || true
    overall_status=1
}

mark_all_layers() {
    local stage="$1"
    local message="$2"
    local exit_code="${3:-1}"
    mark_layer_error rust "$rust_minimum" "$stage" "$message" "$exit_code"
    mark_layer_error cpp "$cpp_minimum" "$stage" "$message" "$exit_code"
    mark_layer_error sql "$sql_minimum" "$stage" "$message" "$exit_code"
}

record_stage() {
    local layer="$1"
    local stage="$2"
    local exit_code="$3"
    local message="${4:-stage failed}"
    python3 scripts/coverage_tools.py record-stage \
        --artifact-dir "$artifact_dir" --layer "$layer" --stage "$stage" \
        --exit-code "$exit_code" --message "$message" >/dev/null 2>&1 || true
}

run_logged() {
    local log="$1"
    shift
    mkdir -p "$(dirname "$log")"
    "$@" 2>&1 | tee "$log"
    return "${PIPESTATUS[0]}"
}

merge_profiles() {
    local llvm_profdata="$1"
    local profile_dir="$2"
    local output="$3"
    local profiles=()
    while IFS= read -r -d '' profile; do
        profiles+=("$profile")
    done < <(find "$profile_dir" -type f -name '*.profraw' -print0 2>/dev/null)
    if [ "${#profiles[@]}" -eq 0 ]; then
        echo "error: no LLVM raw profiles were written under $profile_dir" >&2
        return 1
    fi
    "$llvm_profdata" merge -sparse "${profiles[@]}" -o "$output"
}

copy_profiles() {
    local source_dir="$1"
    local output_dir="$2"
    local index=0
    mkdir -p "$output_dir"
    find "$output_dir" -type f -name '*.profraw' -delete
    while IFS= read -r -d '' profile; do
        if [ -s "$profile" ]; then
            cp "$profile" "$output_dir/profile-${index}.profraw" || return 1
            index=$((index + 1))
        fi
    done < <(find "$source_dir" -type f -name '*.profraw' -print0 2>/dev/null)
    if [ "$index" -eq 0 ]; then
        echo "error: no nonempty LLVM raw profiles were retained from $source_dir" >&2
        return 1
    fi
}

llvm_export_artifacts() {
    local llvm_cov="$1"
    local object="$2"
    local profdata="$3"
    local output_dir="$4"
    local status=0
    "$llvm_cov" export "$object" -instr-profile="$profdata" -format=text \
        > "$output_dir/raw-coverage.json" \
        2> "$output_dir/llvm-cov-export.log" || status=1
    "$llvm_cov" export "$object" -instr-profile="$profdata" -format=lcov \
        > "$output_dir/raw-lcov.info" \
        2> "$output_dir/llvm-cov-lcov.log" || status=1
    "$llvm_cov" report "$object" -instr-profile="$profdata" \
        > "$output_dir/raw-summary.txt" \
        2> "$output_dir/llvm-cov-report.log" || status=1
    return "$status"
}

if ! command -v python3 >/dev/null 2>&1; then
    echo "error: python3 is required to validate coverage evidence" >&2
    exit 2
fi

if python3 scripts/coverage_tools.py validate-thresholds \
    "$rust_minimum" "$cpp_minimum" "$sql_minimum" \
    > "$artifact_dir/thresholds.log" 2>&1; then
    python3 scripts/coverage_tools.py init-artifacts \
        --artifact-dir "$artifact_dir" \
        --rust-threshold "$rust_minimum" \
        --cpp-threshold "$cpp_minimum" \
        --sql-threshold "$sql_minimum" \
        >> "$artifact_dir/init.log" 2>&1 || mark_all_layers initialization \
            "artifact initialization failed" 2
else
    cat "$artifact_dir/thresholds.log" >&2
    rust_minimum=90
    cpp_minimum=90
    sql_minimum=90
    mark_all_layers thresholds "release coverage threshold validation failed" 2
fi

case "$artifact_dir" in
    "$repo_root"/*) ;;
    *)
        mark_all_layers artifact_path \
            "COVERAGE_ARTIFACT_DIR must resolve inside the repository" 2
        overall_status=1
        ;;
esac
case "$build_root" in
    "$repo_root"/*) ;;
    *)
        mark_all_layers build_path \
            "COVERAGE_BUILD_DIR must resolve inside the repository" 2
        overall_status=1
        ;;
esac

cp "$scope_file" "$artifact_dir/scope.json" 2>/dev/null || true
cp "$baseline_file" "$artifact_dir/release-baseline.json" 2>/dev/null || true
cp "$manifest_file" "$artifact_dir/sql-semantic-assertions.json" 2>/dev/null || true

git_commit="unknown"
git_tree="unknown"
if command -v git >/dev/null 2>&1; then
    git_commit="$(git rev-parse --verify HEAD 2>/dev/null || printf unknown)"
    if [ -z "$(git status --porcelain --untracked-files=normal 2>/dev/null)" ]; then
        git_tree="clean"
    else
        git_tree="dirty"
        mark_all_layers provenance \
            "release coverage requires a clean source tree for exact-SHA provenance" 1
        git status --short > "$artifact_dir/dirty-tree.log" 2>&1 || true
    fi
else
    mark_all_layers provenance "git is unavailable for release provenance" 127
fi
if ! python3 scripts/coverage_tools.py capture-provenance \
    --repo-root "$repo_root" --scope "$scope_file" --baseline "$baseline_file" \
    --output "$artifact_dir/provenance.json" \
    > "$artifact_dir/provenance.log" 2>&1; then
    mark_all_layers provenance "exact clean-tree provenance capture failed" 1
fi

cat > "$artifact_dir/coverage-scope.txt" <<EOF
Gate: pg_accel three-layer release coverage
Git commit: ${git_commit}
Git tree: ${git_tree}
PostgreSQL major: ${pg}
Rust threshold: ${rust_minimum}% production source lines
C++ threshold: ${cpp_minimum}% host-object source lines
SQL threshold: ${sql_minimum}% fixed-manifest semantic assertions

Rust scope includes owned production Rust in pg_accel/src,
pg_accel_bench/src, and pg_accel/build.rs. Its denominator is the compiler-
derived pg${pg} build map produced without the pg_test feature. The same
configuration is tested before pg_test tests may supplement hits. Separately
compiled test files are pinned exclusions. Missing production mappings fail
closed.

C++ scope is host-object source coverage for every owned implementation under
pgaccel-kernels/src and executable inline header under pgaccel-kernels/include.
The complete registered CTest suite is separate GPU correctness evidence,
including the unchanged OOM-never invariant measured at 14.08GB peak RSS.
Host-object source coverage
does not claim device kernel-line execution.

SQL scope is unique successful PGACCEL_ASSERT_OK IDs divided by the pinned
coverage/sql-semantic-assertions.json declarations. The fixed floors are 52
files and 287 assertions. File completion markers, warnings, skips, caught
exceptions, duplicate IDs, source/hash drift, or nonzero exits cannot earn
semantic credit. SQL-triggered Rust source reachability is retained separately
under sql-reachability and has no release percentage threshold.
EOF

if ! python3 scripts/coverage_tools.py audit-scope \
    --scope "$scope_file" --repo-root "$repo_root" \
    > "$artifact_dir/scope-audit.log" 2>&1; then
    cat "$artifact_dir/scope-audit.log" >&2
    mark_all_layers scope_audit "checked-in coverage scope or manifest audit failed" 2
fi
if ! env PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
    -s scripts/tests -p 'test_coverage_tools.py' \
    > "$artifact_dir/tool-tests.log" 2>&1; then
    cat "$artifact_dir/tool-tests.log" >&2
    mark_all_layers tool_tests "coverage helper regression tests failed" 2
fi

# PostgreSQL setup is shared by Rust/pgrx and SQL execution, but a failure does
# not suppress the independent C++/GPU attempt.
pg_ready=1
# shellcheck source=/dev/null
if ! source scripts/pg_versions.sh; then
    pg_ready=0
fi
if [ "$pg_ready" -eq 1 ] && ! pg_accel_require_pgrx_support "$pg" \
    > "$artifact_dir/pg-support.log" 2>&1; then
    pg_ready=0
fi
if [ "$pg_ready" -eq 1 ] && ! pg_accel_require_pgrx_pg_config "$pg" \
    > "$artifact_dir/pg-config.log" 2>&1; then
    pg_ready=0
fi
if [ "$pg_ready" -eq 1 ] && ! run_logged "$artifact_dir/setup-pg-extensions.log" \
    scripts/setup_pg_extensions.sh "$pg"; then
    pg_ready=0
fi
if [ "$pg_ready" -eq 0 ]; then
    mark_layer_error rust "$rust_minimum" postgres_setup \
        "PostgreSQL/pgrx prerequisites failed" 1
    mark_layer_error sql "$sql_minimum" postgres_setup \
        "PostgreSQL/pgrx prerequisites failed" 1
fi

rust_coverage() (
    local output_dir="$artifact_dir/rust"
    local build_dir="$build_root/rust"
    local execution_status=0
    local profile_dir="$output_dir/profiles"
    mkdir -p "$output_dir" "$build_dir" "$profile_dir"
    find "$profile_dir" -type f -delete

    if ! command -v cargo >/dev/null 2>&1; then
        mark_layer_error rust "$rust_minimum" prerequisite \
            "cargo is unavailable" 127
        return 1
    fi
    if ! cargo llvm-cov --version > "$output_dir/cargo-llvm-cov-version.txt" 2>&1; then
        mark_layer_error rust "$rust_minimum" prerequisite \
            "cargo-llvm-cov is unavailable" 127
        return 1
    fi
    if [ "$pg_ready" -eq 0 ]; then
        return 1
    fi

    local coverage_env
    if ! coverage_env="$(CARGO_TARGET_DIR="$build_dir" cargo llvm-cov show-env --sh \
        --include-build-script \
        2> "$output_dir/show-env.log")"; then
        mark_layer_error rust "$rust_minimum" instrumentation \
            "cargo llvm-cov could not produce Rust instrumentation environment" 1
        return 1
    fi
    record_stage rust instrumentation 0
    eval "$coverage_env"
    if ! run_logged "$output_dir/clean.log" env CARGO_TARGET_DIR="$build_dir" \
        cargo llvm-cov clean --workspace; then
        mark_layer_error rust "$rust_minimum" clean \
            "stale Rust coverage artifacts could not be removed" 1
        return 1
    fi
    record_stage rust clean 0
    printf '{"postgres_major":%s,"default_features":false,"features":["pg%s"],"pg_test":false}\n' \
        "$pg" "$pg" > "$output_dir/production-config.json"
    if run_logged "$output_dir/production-build.log" env \
        CARGO_TARGET_DIR="$build_dir" \
        cargo build --workspace --locked --no-default-features \
            --features "pg${pg}"; then
        record_stage rust production_build 0
    else
        execution_status=1
        record_stage rust production_build 1 \
            "production pg${pg} build without pg_test failed"
    fi
    if run_logged "$output_dir/production-map.log" env \
        CARGO_TARGET_DIR="$build_dir" \
        cargo llvm-cov report --lcov --include-build-script \
            --output-path "$output_dir/production-map.info" \
        && [ -s "$output_dir/production-map.info" ]; then
        record_stage rust production_mapping 0
    else
        execution_status=1
        record_stage rust production_mapping 1 \
            "compiler-derived production LCOV map was not generated"
    fi
    if run_logged "$output_dir/production-test.log" env \
        CARGO_TARGET_DIR="$build_dir" RUST_TEST_THREADS="$test_threads" \
        cargo test --workspace --locked --no-default-features \
            --features "pg${pg}" --all-targets -- \
            --test-threads="$test_threads"; then
        record_stage rust production_tests 0
    else
        execution_status=1
        record_stage rust production_tests 1 \
            "workspace tests without pg_test failed"
    fi
    if run_logged "$output_dir/supplemental-test.log" env \
        CARGO_TARGET_DIR="$build_dir" RUST_TEST_THREADS="$test_threads" \
        cargo test --workspace --locked --no-default-features \
            --features "pg${pg} pg_test" --all-targets -- \
            --test-threads="$test_threads"; then
        record_stage rust supplemental_tests 0
    else
        execution_status=1
        record_stage rust supplemental_tests 1 \
            "supplemental pg_test workspace tests failed"
    fi
    if run_logged "$output_dir/pgrx-test.log" env \
        CARGO_TARGET_DIR="$build_dir" RUST_TEST_THREADS="$test_threads" \
        cargo pgrx test --package pg_accel "pg$pg"; then
        record_stage rust pgrx_tests 0
    else
        execution_status=1
        record_stage rust pgrx_tests 1 "cargo pgrx tests failed"
    fi
    local tools llvm_cov llvm_profdata
    if ! tools="$(rust_llvm_tools)"; then
        mark_layer_error rust "$rust_minimum" toolchain \
            "matching Rust llvm-cov/llvm-profdata tools are unavailable" 1
        return 1
    fi
    llvm_cov="$(printf '%s\n' "$tools" | sed -n '1p')"
    llvm_profdata="$(printf '%s\n' "$tools" | sed -n '2p')"
    if ! python3 scripts/coverage_tools.py validate-rust-toolchain \
        --rustc "$(command -v rustc)" --llvm-cov "$llvm_cov" \
        --llvm-profdata "$llvm_profdata" \
        --output "$output_dir/toolchain.json"; then
        mark_layer_error rust "$rust_minimum" toolchain \
            "rustc, llvm-cov, and llvm-profdata majors do not match" 1
        return 1
    fi
    record_stage rust toolchain 0
    if ! copy_profiles "$build_dir" "$profile_dir" \
        > "$output_dir/copy-profiles.log" 2>&1 \
        || ! merge_profiles "$llvm_profdata" "$profile_dir" \
            "$output_dir/coverage.profdata" \
            > "$output_dir/llvm-profdata.log" 2>&1; then
        mark_layer_error rust "$rust_minimum" profiles \
            "Rust raw profiles could not be retained and merged" 1
        return 1
    fi
    local report_status=0
    run_logged "$output_dir/lcov-report.log" env CARGO_TARGET_DIR="$build_dir" \
        cargo llvm-cov report --lcov \
            --include-build-script \
            --output-path "$output_dir/raw-lcov.info" || report_status=1
    run_logged "$output_dir/json-report.log" env CARGO_TARGET_DIR="$build_dir" \
        cargo llvm-cov report --json \
            --include-build-script \
            --output-path "$output_dir/raw-coverage.json" || report_status=1
    env CARGO_TARGET_DIR="$build_dir" cargo llvm-cov report --include-build-script \
        > "$output_dir/raw-summary.txt" 2> "$output_dir/summary-report.log" || report_status=1
    if [ "$report_status" -eq 0 ]; then
        record_stage rust coverage_report 0
    else
        execution_status=1
        record_stage rust coverage_report 1 "Rust coverage report generation failed"
    fi
    if [ ! -s "$output_dir/raw-lcov.info" ]; then
        mark_layer_error rust "$rust_minimum" report \
            "Rust LCOV report was not generated" 1
        return 1
    fi
    local summary_status=0
    python3 scripts/coverage_tools.py summarize \
        --layer rust --format lcov \
        --input "$output_dir/raw-lcov.info" \
        --production-map "$output_dir/production-map.info" \
        --scope "$scope_file" --repo-root "$repo_root" \
        --threshold "$rust_minimum" \
        --execution-status "$execution_status" \
        --output-dir "$output_dir" --artifact-dir "$artifact_dir" \
        || summary_status=$?
    if python3 scripts/coverage_tools.py seal-layer-evidence \
        --artifact-dir "$artifact_dir" --layer rust; then
        record_stage rust raw_evidence 0
    else
        record_stage rust raw_evidence 1 "Rust raw evidence sealing failed"
        summary_status=1
    fi
    return "$summary_status"
)

resolve_cpp_llvm_tools() {
    local acpp="$1"
    local configured_clang="${CPP_COVERAGE_CLANGXX:-}"
    if [ -z "$configured_clang" ]; then
        configured_clang="$($acpp --acpp-version 2>/dev/null | \
            awk -F': ' '/^[[:space:]]*default-clang:/ { print $2; exit }')"
    fi
    if [ -z "$configured_clang" ] || [ ! -x "$configured_clang" ]; then
        echo "error: cannot resolve the Clang executable used by AdaptiveCpp" >&2
        return 1
    fi
    local llvm_prefix="${CPP_COVERAGE_LLVM_PREFIX:-$(cd "$(dirname "$configured_clang")/.." && pwd)}"
    local llvm_cov="${CPP_COVERAGE_LLVM_COV:-$llvm_prefix/bin/llvm-cov}"
    local llvm_profdata="${CPP_COVERAGE_LLVM_PROFDATA:-$llvm_prefix/bin/llvm-profdata}"
    if [ ! -x "$llvm_cov" ] || [ ! -x "$llvm_profdata" ]; then
        echo "error: matching llvm-cov/llvm-profdata are unavailable under $llvm_prefix" >&2
        return 1
    fi
    printf '%s\n%s\n%s\n' "$configured_clang" "$llvm_cov" "$llvm_profdata"
}

cpp_coverage() (
    local output_dir="$artifact_dir/cpp"
    local build_dir="$build_root/cpp-build"
    local profile_dir="$output_dir/profiles"
    local acpp_prefix="${ACPP_PREFIX:-$(pg_accel_acpp_prefix 2>/dev/null || printf '%s' "$repo_root/.pgaccel/acpp/current")}"
    local acpp="$acpp_prefix/bin/acpp"
    local per_test_log_dir="$output_dir/per-test-logs"
    local execution_status=0
    mkdir -p "$output_dir" "$profile_dir" "$per_test_log_dir"
    find "$profile_dir" -type f -delete
    find "$per_test_log_dir" -type f -delete

    for command in cmake ctest; do
        if ! command -v "$command" >/dev/null 2>&1; then
            mark_layer_error cpp "$cpp_minimum" prerequisite \
                "$command is unavailable" 127
            return 1
        fi
    done
    if [ ! -x "$acpp" ]; then
        mark_layer_error cpp "$cpp_minimum" prerequisite \
            "AdaptiveCpp driver not found at $acpp" 127
        return 1
    fi
    local tools
    if ! tools="$(resolve_cpp_llvm_tools "$acpp" 2> "$output_dir/resolve-tools.log")"; then
        mark_layer_error cpp "$cpp_minimum" toolchain \
            "C++ LLVM tools could not be resolved" 1
        return 1
    fi
    local clangxx llvm_cov llvm_profdata
    clangxx="$(printf '%s\n' "$tools" | sed -n '1p')"
    llvm_cov="$(printf '%s\n' "$tools" | sed -n '2p')"
    llvm_profdata="$(printf '%s\n' "$tools" | sed -n '3p')"
    if ! python3 scripts/coverage_tools.py validate-toolchain \
        --clang "$clangxx" --llvm-cov "$llvm_cov" \
        --llvm-profdata "$llvm_profdata" \
        --output "$output_dir/toolchain.json"; then
        mark_layer_error cpp "$cpp_minimum" toolchain \
            "clang, llvm-cov, and llvm-profdata majors do not match" 1
        return 1
    fi
    record_stage cpp toolchain 0

    if ! cmake -E remove_directory "$build_dir" \
        > "$output_dir/clean.log" 2>&1; then
        mark_layer_error cpp "$cpp_minimum" clean \
            "stale C++ coverage build artifacts could not be removed" 1
        return 1
    fi
    record_stage cpp clean 0

    if ! run_logged "$output_dir/configure.log" cmake \
        -S pgaccel-kernels -B "$build_dir" \
        -DCMAKE_BUILD_TYPE=Debug \
        -DCMAKE_CXX_COMPILER="$clangxx" \
        -DAdaptiveCpp_DIR="$acpp_prefix/lib/cmake/AdaptiveCpp" \
        -DPGACCEL_ENABLE_COVERAGE=ON \
        -DPGACCEL_GPU_TEST_LOG_DIR="$per_test_log_dir"; then
        mark_layer_error cpp "$cpp_minimum" configure \
            "instrumented C++ CMake configure failed" 1
        return 1
    fi
    record_stage cpp configure 0
    if ! run_logged "$output_dir/build.log" cmake --build "$build_dir" --parallel; then
        mark_layer_error cpp "$cpp_minimum" build \
            "instrumented C++ build failed" 1
        return 1
    fi
    record_stage cpp build 0

    # Do not override per-test timeouts: test_oom_invariant retains its 900s
    # timeout and unchanged 2GB-per-family sweep (14.08GB measured peak RSS).
    if ! run_logged "$output_dir/ctest.log" env \
        LLVM_PROFILE_FILE="$profile_dir/pgaccel-cpp-%p-%m.profraw" \
        ctest --test-dir "$build_dir" --output-on-failure; then
        execution_status=1
        record_stage cpp ctest 1 "registered GPU CTest suite failed"
    else
        record_stage cpp ctest 0
    fi
    if python3 scripts/coverage_tools.py gpu-evidence \
        --execution-status "$execution_status" \
        --ctest-log "$output_dir/ctest.log" \
        --per-test-log-dir "$per_test_log_dir" \
        --baseline "$baseline_file" \
        --output "$output_dir/gpu-correctness-evidence.json"; then
        record_stage cpp gpu_evidence 0
    else
        execution_status=1
        record_stage cpp gpu_evidence 1 "GPU correctness/OOM evidence failed"
    fi

    local objects=()
    while IFS= read -r -d '' object; do
        objects+=("$object")
    done < <(find "$build_dir" -maxdepth 3 -type f \
        \( -name 'libpgaccel_kernels_shared.so' \
        -o -name 'libpgaccel_kernels_shared.dylib' \
        -o -name 'pgaccel_kernels_shared.dll' \) -print0)
    if [ "${#objects[@]}" -ne 1 ]; then
        mark_layer_error cpp "$cpp_minimum" object \
            "expected exactly one instrumented shared kernel host object; found ${#objects[@]}" 1
        return 1
    fi
    if ! merge_profiles "$llvm_profdata" "$profile_dir" \
        "$output_dir/coverage.profdata" \
        > "$output_dir/llvm-profdata.log" 2>&1; then
        mark_layer_error cpp "$cpp_minimum" profiles \
            "C++ raw profiles could not be merged" 1
        return 1
    fi
    local export_status=0
    llvm_export_artifacts "$llvm_cov" "${objects[0]}" \
        "$output_dir/coverage.profdata" "$output_dir" || export_status=1
    if [ "$export_status" -ne 0 ]; then
        execution_status=1
    fi
    if [ ! -s "$output_dir/raw-coverage.json" ]; then
        mark_layer_error cpp "$cpp_minimum" report \
            "C++ LLVM JSON report was not generated" 1
        return 1
    fi
    if [ "$export_status" -eq 0 ]; then
        record_stage cpp coverage_report 0
    else
        record_stage cpp coverage_report 1 "C++ coverage export failed"
    fi
    local summary_status=0
    python3 scripts/coverage_tools.py summarize \
        --layer cpp --format json \
        --input "$output_dir/raw-coverage.json" \
        --scope "$scope_file" --repo-root "$repo_root" \
        --threshold "$cpp_minimum" \
        --execution-status "$execution_status" \
        --output-dir "$output_dir" --artifact-dir "$artifact_dir" \
        || summary_status=$?
    if python3 scripts/coverage_tools.py seal-layer-evidence \
        --artifact-dir "$artifact_dir" --layer cpp; then
        record_stage cpp raw_evidence 0
    else
        record_stage cpp raw_evidence 1 "C++ raw evidence sealing failed"
        summary_status=1
    fi
    return "$summary_status"
)

rust_llvm_tools() {
    local sysroot host tool_dir
    sysroot="$(rustc --print sysroot 2>/dev/null)" || return 1
    host="$(rustc -vV 2>/dev/null | awk '/^host:/ { print $2; exit }')"
    tool_dir="$sysroot/lib/rustlib/$host/bin"
    if [ ! -x "$tool_dir/llvm-cov" ] || [ ! -x "$tool_dir/llvm-profdata" ]; then
        return 1
    fi
    printf '%s\n%s\n' "$tool_dir/llvm-cov" "$tool_dir/llvm-profdata"
}

sql_coverage() (
    local output_dir="$artifact_dir/sql"
    local reachability_dir="$artifact_dir/sql-reachability"
    local build_dir="$build_root/sql-build"
    local profile_dir="$reachability_dir/profiles"
    local test_run_dir="$output_dir/test-run"
    local execution_status=0
    local reachability_enabled=0
    local should_stop=0
    local llvm_cov=""
    local llvm_profdata=""
    mkdir -p "$output_dir" "$reachability_dir" "$profile_dir" "$test_run_dir/logs"
    find "$profile_dir" -type f -delete
    printf 'file\tstatus\texit_code\tlog\n' > "$test_run_dir/results.tsv"

    if ! command -v cargo >/dev/null 2>&1 || ! command -v just >/dev/null 2>&1; then
        mark_layer_error sql "$sql_minimum" prerequisite \
            "cargo or just is unavailable for SQL extension installation" 127
        execution_status=127
    elif [ "$pg_ready" -eq 0 ]; then
        execution_status=1
    else
        local tools coverage_env
        if cargo llvm-cov --version >/dev/null 2>&1 \
            && tools="$(rust_llvm_tools)" \
            && coverage_env="$(CARGO_TARGET_DIR="$build_dir" cargo llvm-cov show-env --sh \
                --include-build-script \
                2> "$reachability_dir/show-env.log")"; then
            llvm_cov="$(printf '%s\n' "$tools" | sed -n '1p')"
            llvm_profdata="$(printf '%s\n' "$tools" | sed -n '2p')"
            eval "$coverage_env"
            export LLVM_PROFILE_FILE="$profile_dir/pgaccel-sql-%p-%m.profraw"
            reachability_enabled=1
        else
            printf '%s\n' \
                "SQL-triggered Rust reachability unavailable; semantic assertions still run." \
                > "$reachability_dir/unavailable.log"
        fi

        stop_postgres() {
            if [ "$should_stop" -eq 0 ]; then
                return 0
            fi
            if run_logged "$output_dir/postgres-stop.log" \
                cargo pgrx stop --package pg_accel "pg$pg"; then
                should_stop=0
                return 0
            fi
            return 1
        }
        trap 'stop_postgres >/dev/null 2>&1 || true' EXIT
        trap 'stop_postgres >/dev/null 2>&1 || true; exit 130' INT TERM

        if run_logged "$output_dir/install.log" env CARGO_TARGET_DIR="$build_dir" \
            just install-pg-accel "$pg"; then
            record_stage sql extension_install 0
            should_stop=1
            local pg_config psql_bin port connection
            pg_config="$(pg_accel_pg_config_for_pg "$pg")"
            psql_bin="$("$pg_config" --bindir)/psql"
            if [ ! -x "$psql_bin" ]; then
                psql_bin="psql"
            fi
            port="$(pg_accel_pgrx_port_for_pg "$pg")"
            connection="host=localhost port=$port dbname=postgres"
            if run_logged "$output_dir/create-extensions.log" \
                "$psql_bin" "$connection" -v ON_ERROR_STOP=1 \
                    -f sql/init/01-create-extensions.sql; then
                record_stage sql extension_init 0
            else
                execution_status=1
                record_stage sql extension_init 1 "extension initialization failed"
            fi
            if run_logged "$output_dir/sql-tests.log" env \
                PG_ACCEL_PG_MAJOR="$pg" \
                PG_ACCEL_SQL_TEST_REQUIRE_EXTENSION=1 \
                PG_ACCEL_RELEASE_MODE=1 \
                PG_ACCEL_SQL_TEST_ARTIFACT_DIR="$test_run_dir" \
                sql/tests/run_all.sh "$connection"; then
                record_stage sql sql_tests 0
            else
                execution_status=1
                record_stage sql sql_tests 1 "SQL integration test runner failed"
            fi
        else
            execution_status=1
            record_stage sql extension_install 1 "extension installation failed"
        fi
        stop_postgres || execution_status=1
    fi

    python3 scripts/coverage_tools.py sql-inventory \
        --tests-dir sql/tests --manifest "$manifest_file" \
        --results "$test_run_dir/results.tsv" \
        --output-dir "$output_dir" \
        --threshold "$sql_minimum" \
        --execution-status "$execution_status" \
        --artifact-dir "$artifact_dir" || execution_status=1

    if python3 scripts/coverage_tools.py seal-layer-evidence \
        --artifact-dir "$artifact_dir" --layer sql; then
        record_stage sql raw_evidence 0
    else
        record_stage sql raw_evidence 1 "SQL raw evidence sealing failed"
        execution_status=1
    fi

    if [ "$reachability_enabled" -eq 1 ]; then
        local objects=()
        while IFS= read -r -d '' object; do
            objects+=("$object")
        done < <(find "$build_dir/release" -maxdepth 1 -type f \
            \( -name 'libpg_accel.so' -o -name 'libpg_accel.dylib' \
            -o -name 'pg_accel.dll' \) -print0 2>/dev/null)
        if [ "${#objects[@]}" -eq 1 ] \
            && merge_profiles "$llvm_profdata" "$profile_dir" \
                "$reachability_dir/coverage.profdata" \
                > "$reachability_dir/llvm-profdata.log" 2>&1 \
            && llvm_export_artifacts "$llvm_cov" "${objects[0]}" \
                "$reachability_dir/coverage.profdata" "$reachability_dir" \
            && python3 scripts/coverage_tools.py summarize-reachability \
                --input "$reachability_dir/raw-lcov.info" \
                --scope "$scope_file" --repo-root "$repo_root" \
                --output "$reachability_dir/reachability-summary.json"; then
            :
        else
            printf '%s\n' "SQL Rust reachability collection failed; this artifact has no threshold." \
                >> "$reachability_dir/unavailable.log"
        fi
    fi
    return "$execution_status"
)

echo "=== Rust production source coverage ==="
rust_coverage || overall_status=1

echo "=== C++/SYCL host-object source coverage and GPU correctness ==="
cpp_coverage || overall_status=1

echo "=== SQL semantic assertion coverage ==="
sql_coverage || overall_status=1

if ! python3 scripts/coverage_tools.py aggregate --artifact-dir "$artifact_dir" \
    --repo-root "$repo_root"; then
    overall_status=1
fi
aggregate_done=1

exit "$overall_status"
