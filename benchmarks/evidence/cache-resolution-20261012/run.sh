#!/usr/bin/env bash
set -u
set -o pipefail
ulimit -n 4096
cd /Users/contra/Projects/pg_accel
qualification_root="$PWD/.codex/scratch/cache-resolution-live-20261012"
build_dir="$qualification_root/build"
export ACPP_APPDB_DIR="$qualification_root/runtime-base"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX27.0.sdk
export PYTHONDONTWRITEBYTECODE=1
unset PGACCEL_ACPP_CACHE_DIR
mkdir -p "$qualification_root/raw"
run_stage() {
    local label="$1"
    shift
    printf '%s START %s\n' "$(date -u +%FT%TZ)" "$label"
    printf '%q ' "$@" >> "$qualification_root/commands.log"
    printf '\n' >> "$qualification_root/commands.log"
    "$@" > "$qualification_root/$label.log" 2>&1
    local result=$?
    printf '%s END %s exit=%s\n' "$(date -u +%FT%TZ)" "$label" "$result"
    printf '%s %s\n' "$label" "$result" >> "$qualification_root/exit-statuses.txt"
    return "$result"
}
printf 'ACPP_APPDB_DIR=%s\nSDKROOT=%s\nFD_LIMIT=%s\n' "$ACPP_APPDB_DIR" "$SDKROOT" "$(ulimit -n)" > "$qualification_root/environment.txt"
rg --files pgaccel-kernels/src pgaccel-kernels/include pgaccel-kernels/test scripts -g '*.cpp' -g '*.hpp' -g '*.h' -g '*.py' -g '*.sh' | LC_ALL=C sort | xargs shasum -a 256 > "$qualification_root/compile-inputs.sha256"
shasum -a 256 pgaccel-kernels/CMakeLists.txt Justfile .github/workflows/ci.yml .acpp-version Cargo.toml > "$qualification_root/config-inputs.sha256"
touch "$qualification_root/build-marker"
run_stage configure cmake -S pgaccel-kernels -B "$build_dir" -DCMAKE_BUILD_TYPE=Release -DCMAKE_CXX_COMPILER=/usr/bin/c++ -DCMAKE_OSX_SYSROOT="$SDKROOT" -DAdaptiveCpp_DIR="$PWD/.pgaccel/acpp/current/lib/cmake/AdaptiveCpp" -DPGACCEL_GPU_TEST_LOG_DIR="$qualification_root/raw" || exit 1
run_stage build cmake --build "$build_dir" --parallel 8 || exit 1
run_stage ctest-manifest ctest --test-dir "$build_dir" --show-only=json-v1 || exit 1
run_stage ctest ctest --test-dir "$build_dir" --output-on-failure --parallel 1
ctest_result=$?
export PGACCEL_ARCHIVE_PATH_TEST="$build_dir/test_archive_cache_path"
run_stage python python3 -m unittest scripts.tests.test_acpp_cache scripts.tests.test_metal_stress_artifacts.CacheMeasurementTests scripts.tests.test_metal_stress_artifacts.ArchiveParserTests scripts.tests.test_metal_stress_artifacts.ArtifactIndexTests scripts.tests.test_metal_stress_artifacts.CrashArtifactTests scripts.tests.test_metal_stress_artifacts.LogAuditTests scripts.tests.test_metal_stress_artifacts.ReleaseWorkflowTests -v
python_result=$?
run_stage shell bash -n scripts/capture_metal_ci_diagnostics.sh
shell_result=$?
run_stage just just --list
just_result=$?
run_stage actionlint actionlint -config-file .github/actionlint.yaml -shellcheck= .github/workflows/ci.yml
actionlint_result=$?
run_stage cpu-audit python3 scripts/cpu_cheat_audit.py --json-report "$qualification_root/cpu-audit.json" --abi-manifest scripts/cpu_cheat_abi_manifest.txt --objects "$build_dir/libpgaccel_kernels_shared.dylib" --build-marker "$qualification_root/build-marker" --metadata-binding "$build_dir/libpgaccel_kernels_shared.dylib.metadata-binding.json" --headers pgaccel-kernels/include/*.h -- pgaccel-kernels/src/*.cpp
audit_result=$?
shasum -a 256 -c "$qualification_root/compile-inputs.sha256" > "$qualification_root/source-identity-after.log" 2>&1
identity_result=$?
printf 'SUMMARY ctest=%s python=%s shell=%s just=%s actionlint=%s audit=%s source_identity=%s\n' "$ctest_result" "$python_result" "$shell_result" "$just_result" "$actionlint_result" "$audit_result" "$identity_result"
if (( ctest_result || python_result || shell_result || just_result || actionlint_result || identity_result )); then exit 1; fi
# The audit is expected to retain seven known release blockers; its report is
# inspected separately. A nonzero audit is never represented as a pass.
exit 0
