#!/usr/bin/env bash
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root" || exit 1

# shellcheck source=scripts/pg_versions.sh
source scripts/pg_versions.sh

pg="${1:-$(pg_accel_default_pg_major)}"
pg="${pg#pg}"
artifact_dir="${COVERAGE_ARTIFACT_DIR:-artifacts/coverage}"
build_root="${COVERAGE_BUILD_DIR:-target/coverage}"
scope_file="coverage/scope.json"
minimum_default="${COVERAGE_MIN_LINES:-90}"
rust_minimum="${COVERAGE_MIN_RUST_LINES:-$minimum_default}"
cpp_minimum="${COVERAGE_MIN_CPP_LINES:-$minimum_default}"
sql_minimum="${COVERAGE_MIN_SQL_LINES:-$minimum_default}"
test_threads="${RUST_TEST_THREADS:-1}"

pg_accel_require_pgrx_support "$pg"
pg_accel_require_pgrx_pg_config "$pg"

for command in cargo cmake ctest git python3; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "error: required coverage command is unavailable: $command" >&2
        exit 1
    fi
done
if ! cargo llvm-cov --version >/dev/null 2>&1; then
    echo "error: cargo-llvm-cov is not installed for the active Rust toolchain." >&2
    echo "       run: cargo install cargo-llvm-cov --locked" >&2
    exit 1
fi
if [ -n "$(git status --porcelain --untracked-files=normal)" ]; then
    echo "error: release coverage requires a clean source tree for exact-SHA provenance" >&2
    git status --short >&2
    exit 1
fi

mkdir -p "$artifact_dir" "$build_root"
artifact_dir="$(cd "$artifact_dir" && pwd -P)"
build_root="$(cd "$build_root" && pwd -P)"
case "$artifact_dir" in
    "$repo_root"/*) ;;
    *)
        echo "error: COVERAGE_ARTIFACT_DIR must resolve inside the repository" >&2
        exit 1
        ;;
esac
case "$build_root" in
    "$repo_root"/*) ;;
    *)
        echo "error: COVERAGE_BUILD_DIR must resolve inside the repository" >&2
        exit 1
        ;;
esac
if ! python3 scripts/coverage_tools.py validate-thresholds \
    "$rust_minimum" "$cpp_minimum" "$sql_minimum"; then
    exit 1
fi
for layer in rust cpp sql; do
    if [ -d "$artifact_dir/$layer" ]; then
        find "$artifact_dir/$layer" -depth -mindepth 1 -delete
    fi
    mkdir -p "$artifact_dir/$layer"
done
cp "$scope_file" "$artifact_dir/scope.json"

cat > "$artifact_dir/coverage-scope.txt" <<EOF
Gate: pg_accel three-layer release coverage
Git commit: $(git rev-parse --verify HEAD)
Git tree: clean
PostgreSQL major: ${pg}
Rust threshold: ${rust_minimum}% lines
C++/SYCL threshold: ${cpp_minimum}% lines
SQL-extension threshold: ${sql_minimum}% lines

Rust scope: owned production Rust in pg_accel/src and pg_accel_bench/src,
excluding separately compiled test-only source files. Execution includes the
workspace test targets and pgrx pg_test feature.

C++/SYCL scope: all owned implementation files under pgaccel-kernels/src plus
owned inline headers reported by LLVM. Every src/*.cpp file must have a source
mapping, as must each owned header containing executable definitions. Execution
is the registered standalone CTest suite against the instrumented
pgaccel_kernels_shared library.

SQL-extension scope: the same owned pg_accel production Rust source is rebuilt
with rustc source coverage and reached exclusively by sql/tests/[0-9]*.sql in
a live PostgreSQL backend. test-inventory.json binds source hashes and explicit
PASS/PASSED behavior markers to retained psql logs. File or marker counts are
supporting traceability, not the SQL layer percentage.

Generated, vendored, AdaptiveCpp runtime/toolchain, PostgreSQL, PostGIS, H3,
test-source, shell, and benchmark artifact files are outside their respective
owned production-code denominators. No production planner, executor, FFI,
dispatch, domain, or kernel implementation file is allowlisted out.
EOF

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
    done < <(find "$profile_dir" -type f -name '*.profraw' -print0)
    if [ "${#profiles[@]}" -eq 0 ]; then
        echo "error: no LLVM raw profiles were written under $profile_dir" >&2
        return 1
    fi
    "$llvm_profdata" merge -sparse "${profiles[@]}" -o "$output"
}

llvm_export_artifacts() {
    local llvm_cov="$1"
    local object="$2"
    local profdata="$3"
    local output_dir="$4"
    local status=0
    if ! "$llvm_cov" export "$object" \
        -instr-profile="$profdata" -format=text \
        > "$output_dir/raw-coverage.json" \
        2> "$output_dir/llvm-cov-export.log"; then
        status=1
    fi
    if ! "$llvm_cov" export "$object" \
        -instr-profile="$profdata" -format=lcov \
        > "$output_dir/raw-lcov.info" \
        2> "$output_dir/llvm-cov-lcov.log"; then
        status=1
    fi
    if ! "$llvm_cov" report "$object" -instr-profile="$profdata" \
        > "$output_dir/raw-summary.txt" \
        2> "$output_dir/llvm-cov-report.log"; then
        status=1
    fi
    return "$status"
}

rust_coverage() (
    local output_dir="$artifact_dir/rust"
    local build_dir="$build_root/rust"
    local execution_status=0
    mkdir -p "$output_dir" "$build_dir"

    local coverage_env
    if ! coverage_env="$(CARGO_TARGET_DIR="$build_dir" cargo llvm-cov show-env --sh)"; then
        echo "error: cargo llvm-cov could not produce Rust instrumentation environment" >&2
        return 1
    fi
    eval "$coverage_env"
    if ! run_logged "$output_dir/clean.log" \
        env CARGO_TARGET_DIR="$build_dir" cargo llvm-cov clean --workspace; then
        execution_status=1
    fi
    if ! run_logged "$output_dir/test.log" \
        env CARGO_TARGET_DIR="$build_dir" \
            RUST_TEST_THREADS="$test_threads" \
            cargo test \
                --workspace \
                --locked \
                --no-default-features \
                --features "pg${pg} pg_test" \
                --all-targets \
                -- \
                --test-threads="$test_threads"; then
        execution_status=1
    fi
    if ! run_logged "$output_dir/pgrx-test.log" env \
        CARGO_TARGET_DIR="$build_dir" RUST_TEST_THREADS="$test_threads" \
        cargo pgrx test --package pg_accel "pg$pg"; then
        execution_status=1
    fi
    if ! run_logged "$output_dir/json-report.log" \
        env CARGO_TARGET_DIR="$build_dir" cargo llvm-cov report \
            --json --output-path "$output_dir/raw-coverage.json"; then
        execution_status=1
    fi
    if ! run_logged "$output_dir/lcov-report.log" \
        env CARGO_TARGET_DIR="$build_dir" cargo llvm-cov report \
            --lcov --output-path "$output_dir/raw-lcov.info"; then
        execution_status=1
    fi
    if ! env CARGO_TARGET_DIR="$build_dir" cargo llvm-cov report \
        > "$output_dir/raw-summary.txt" 2> "$output_dir/summary-report.log"; then
        execution_status=1
    fi
    if [ ! -s "$output_dir/raw-coverage.json" ]; then
        echo "error: Rust LLVM JSON report was not generated" >&2
        return 1
    fi
    python3 scripts/coverage_tools.py summarize \
        --layer rust \
        --input "$output_dir/raw-coverage.json" \
        --scope "$scope_file" \
        --repo-root "$repo_root" \
        --threshold "$rust_minimum" \
        --execution-status "$execution_status" \
        --output-dir "$output_dir"
)

resolve_cpp_llvm_tools() {
    local acpp="$1"
    local configured_clang="${CPP_COVERAGE_CLANGXX:-}"
    if [ -z "$configured_clang" ]; then
        configured_clang="$($acpp --acpp-version 2>/dev/null | awk -F': ' '/^[[:space:]]*default-clang:/ { print $2; exit }')"
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
    local clang_major cov_major
    clang_major="$($configured_clang --version | sed -nE '1s/.*clang version ([0-9]+).*/\1/p')"
    cov_major="$($llvm_cov --version | sed -nE '1s/.*LLVM version ([0-9]+).*/\1/p')"
    if [ -z "$clang_major" ] || [ -z "$cov_major" ] || [ "$clang_major" != "$cov_major" ]; then
        echo "error: C++ coverage LLVM mismatch: clang=$clang_major llvm-cov=$cov_major" >&2
        return 1
    fi
    printf '%s\n%s\n%s\n' "$configured_clang" "$llvm_cov" "$llvm_profdata"
}

cpp_coverage() (
    local output_dir="$artifact_dir/cpp"
    local build_dir="$build_root/cpp-build"
    local profile_dir="$output_dir/profiles"
    local acpp_prefix="${ACPP_PREFIX:-$(pg_accel_acpp_prefix)}"
    local acpp="$acpp_prefix/bin/acpp"
    local execution_status=0
    mkdir -p "$output_dir" "$profile_dir"
    find "$profile_dir" -type f -delete

    if [ ! -x "$acpp" ]; then
        echo "error: AdaptiveCpp driver not found at $acpp" >&2
        return 1
    fi
    local tools
    if ! tools="$(resolve_cpp_llvm_tools "$acpp")"; then
        return 1
    fi
    local clangxx llvm_cov llvm_profdata
    clangxx="$(printf '%s\n' "$tools" | sed -n '1p')"
    llvm_cov="$(printf '%s\n' "$tools" | sed -n '2p')"
    llvm_profdata="$(printf '%s\n' "$tools" | sed -n '3p')"
    {
        "$acpp" --acpp-version
        "$clangxx" --version
        "$llvm_cov" --version
        "$llvm_profdata" --version
    } > "$output_dir/toolchain.txt" 2>&1

    if ! run_logged "$output_dir/configure.log" cmake \
        -S pgaccel-kernels \
        -B "$build_dir" \
        -DCMAKE_BUILD_TYPE=Debug \
        -DCMAKE_CXX_COMPILER="$clangxx" \
        -DAdaptiveCpp_DIR="$acpp_prefix/lib/cmake/AdaptiveCpp" \
        -DPGACCEL_ENABLE_COVERAGE=ON; then
        return 1
    fi
    if ! run_logged "$output_dir/build.log" cmake --build "$build_dir" --parallel; then
        return 1
    fi
    if ! run_logged "$output_dir/ctest.log" env \
        LLVM_PROFILE_FILE="$profile_dir/pgaccel-cpp-%p-%m.profraw" \
        ctest --test-dir "$build_dir" --output-on-failure \
            --timeout "${GPU_TEST_TIMEOUT_S:-300}"; then
        execution_status=1
    fi

    local objects=()
    while IFS= read -r -d '' object; do
        objects+=("$object")
    done < <(find "$build_dir" -maxdepth 3 -type f \
        \( -name 'libpgaccel_kernels_shared.so' \
        -o -name 'libpgaccel_kernels_shared.dylib' \
        -o -name 'pgaccel_kernels_shared.dll' \) -print0)
    if [ "${#objects[@]}" -ne 1 ]; then
        echo "error: expected exactly one instrumented shared kernel object, found ${#objects[@]}" >&2
        return 1
    fi
    if ! merge_profiles "$llvm_profdata" "$profile_dir" "$output_dir/coverage.profdata" \
        > "$output_dir/llvm-profdata.log" 2>&1; then
        cat "$output_dir/llvm-profdata.log" >&2
        return 1
    fi
    if ! llvm_export_artifacts "$llvm_cov" "${objects[0]}" \
        "$output_dir/coverage.profdata" "$output_dir"; then
        execution_status=1
    fi
    if [ ! -s "$output_dir/raw-coverage.json" ]; then
        echo "error: C++ LLVM JSON report was not generated" >&2
        return 1
    fi
    python3 scripts/coverage_tools.py summarize \
        --layer cpp \
        --input "$output_dir/raw-coverage.json" \
        --scope "$scope_file" \
        --repo-root "$repo_root" \
        --threshold "$cpp_minimum" \
        --execution-status "$execution_status" \
        --output-dir "$output_dir"
)

rust_llvm_tools() {
    local sysroot host tool_dir
    sysroot="$(rustc --print sysroot)"
    host="$(rustc -vV | awk '/^host:/ { print $2; exit }')"
    tool_dir="$sysroot/lib/rustlib/$host/bin"
    if [ ! -x "$tool_dir/llvm-cov" ] || [ ! -x "$tool_dir/llvm-profdata" ]; then
        echo "error: rustc llvm-tools-preview is unavailable for $host" >&2
        echo "       run: rustup component add llvm-tools-preview" >&2
        return 1
    fi
    printf '%s\n%s\n' "$tool_dir/llvm-cov" "$tool_dir/llvm-profdata"
}

sql_coverage() (
    local output_dir="$artifact_dir/sql"
    local build_dir="$build_root/sql-build"
    local profile_dir="$output_dir/profiles"
    local test_run_dir="$output_dir/test-run"
    local execution_status=0
    local should_stop=0
    mkdir -p "$output_dir" "$profile_dir" "$test_run_dir/logs"
    find "$profile_dir" -type f -delete
    printf 'file\tstatus\texit_code\tlog\n' > "$test_run_dir/results.tsv"

    local tools llvm_cov llvm_profdata
    if ! tools="$(rust_llvm_tools)"; then
        return 1
    fi
    llvm_cov="$(printf '%s\n' "$tools" | sed -n '1p')"
    llvm_profdata="$(printf '%s\n' "$tools" | sed -n '2p')"
    {
        rustc -vV
        "$llvm_cov" --version
        "$llvm_profdata" --version
    } > "$output_dir/toolchain.txt" 2>&1

    local coverage_env
    if ! coverage_env="$(CARGO_TARGET_DIR="$build_dir" cargo llvm-cov show-env --sh)"; then
        echo "error: cargo llvm-cov could not produce instrumentation environment" >&2
        return 1
    fi
    eval "$coverage_env"
    export LLVM_PROFILE_FILE="$profile_dir/pgaccel-sql-%p-%m.profraw"
    if ! run_logged "$output_dir/clean.log" \
        env CARGO_TARGET_DIR="$build_dir" cargo llvm-cov clean --workspace; then
        execution_status=1
    fi

    stop_instrumented_postgres() {
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
    trap 'stop_instrumented_postgres >/dev/null 2>&1 || true' EXIT
    trap 'stop_instrumented_postgres >/dev/null 2>&1 || true; exit 130' INT TERM

    should_stop=1
    if run_logged "$output_dir/install.log" env CARGO_TARGET_DIR="$build_dir" \
        just install-pg-accel "$pg"; then
        # Build scripts can also be instrumented. Remove their profiles after
        # PostgreSQL starts; backend and postmaster profiles are written later.
        find "$profile_dir" -type f -delete
        local pg_config psql_bin port connection
        pg_config="$(pg_accel_pg_config_for_pg "$pg")"
        psql_bin="$("$pg_config" --bindir)/psql"
        if [ ! -x "$psql_bin" ]; then
            psql_bin="psql"
        fi
        port="$(pg_accel_pgrx_port_for_pg "$pg")"
        connection="host=localhost port=$port dbname=postgres"
        if ! run_logged "$output_dir/create-extensions.log" \
            "$psql_bin" "$connection" -v ON_ERROR_STOP=1 \
                -f sql/init/01-create-extensions.sql; then
            execution_status=1
        fi
        if ! run_logged "$output_dir/sql-tests.log" env \
            PG_ACCEL_PG_MAJOR="$pg" \
            PG_ACCEL_SQL_TEST_REQUIRE_EXTENSION=1 \
            PG_ACCEL_RELEASE_MODE=1 \
            PG_ACCEL_SQL_TEST_ARTIFACT_DIR="$test_run_dir" \
            sql/tests/run_all.sh "$connection"; then
            execution_status=1
        fi
    else
        execution_status=1
    fi

    if ! stop_instrumented_postgres; then
        execution_status=1
    fi

    if ! python3 scripts/coverage_tools.py sql-inventory \
        --tests-dir sql/tests \
        --results "$test_run_dir/results.tsv" \
        --output "$output_dir/test-inventory.json" \
        > "$output_dir/test-inventory.log" 2>&1; then
        execution_status=1
    fi

    local objects=()
    while IFS= read -r -d '' object; do
        objects+=("$object")
    done < <(find "$build_dir/release" -maxdepth 1 -type f \
        \( -name 'libpg_accel.so' -o -name 'libpg_accel.dylib' \
        -o -name 'pg_accel.dll' \) -print0 2>/dev/null)
    if [ "${#objects[@]}" -ne 1 ]; then
        echo "error: expected exactly one instrumented pg_accel shared object, found ${#objects[@]}" >&2
        return 1
    fi
    if ! merge_profiles "$llvm_profdata" "$profile_dir" "$output_dir/coverage.profdata" \
        > "$output_dir/llvm-profdata.log" 2>&1; then
        cat "$output_dir/llvm-profdata.log" >&2
        return 1
    fi
    if ! llvm_export_artifacts "$llvm_cov" "${objects[0]}" \
        "$output_dir/coverage.profdata" "$output_dir"; then
        execution_status=1
    fi
    if [ ! -s "$output_dir/raw-coverage.json" ]; then
        echo "error: SQL-extension LLVM JSON report was not generated" >&2
        return 1
    fi
    python3 scripts/coverage_tools.py summarize \
        --layer sql \
        --input "$output_dir/raw-coverage.json" \
        --scope "$scope_file" \
        --repo-root "$repo_root" \
        --threshold "$sql_minimum" \
        --execution-status "$execution_status" \
        --output-dir "$output_dir"
)

overall_status=0

if ! python3 scripts/coverage_tools.py audit-scope \
    --scope "$scope_file" --repo-root "$repo_root" \
    | tee "$artifact_dir/scope-audit.log"; then
    overall_status=1
fi
if ! env PYTHONDONTWRITEBYTECODE=1 \
    python3 -m unittest discover -s scripts/tests -p 'test_coverage_tools.py' \
    > "$artifact_dir/tool-tests.log" 2>&1; then
    cat "$artifact_dir/tool-tests.log" >&2
    overall_status=1
fi

if ! run_logged "$artifact_dir/setup-pg-extensions.log" \
    scripts/setup_pg_extensions.sh "$pg"; then
    overall_status=1
fi

echo "=== Rust coverage ==="
if ! rust_coverage; then
    overall_status=1
fi

echo "=== C++/SYCL coverage ==="
if ! cpp_coverage; then
    overall_status=1
fi

echo "=== SQL-extension coverage ==="
if ! sql_coverage; then
    overall_status=1
fi

if ! python3 scripts/coverage_tools.py aggregate --artifact-dir "$artifact_dir"; then
    overall_status=1
fi

exit "$overall_status"
