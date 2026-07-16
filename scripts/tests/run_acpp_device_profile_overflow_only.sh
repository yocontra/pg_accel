#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
default_acpp="$repo_root/.pgaccel/acpp/metal/bin/acpp"
if [[ ! -x "$default_acpp" ]]; then
    git_common_dir="$(git -C "$repo_root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
    if [[ -n "$git_common_dir" ]]; then
        shared_root="$(cd "$git_common_dir/.." && pwd)"
        default_acpp="$shared_root/.pgaccel/acpp/metal/bin/acpp"
    fi
fi
acpp="${ACPP:-$default_acpp}"
fixture="$repo_root/scripts/tests/fixtures/acpp_device_profile_overflow_only.cpp"
dormancy_fixture="$repo_root/scripts/tests/fixtures/acpp_device_profile_dormancy.cpp"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/acpp-overflow-only.XXXXXX")"
cleanup() {
    chmod -R u+w "$work_dir" 2>/dev/null || true
    rm -rf "$work_dir"
}
trap cleanup EXIT

if [[ -n "${ACPP_TEST_DYLD_LIBRARY_PATH:-}" ]]; then
    export DYLD_LIBRARY_PATH="$ACPP_TEST_DYLD_LIBRARY_PATH"
fi

if [[ ! -x "$acpp" ]]; then
    echo "AdaptiveCpp driver not found at $acpp" >&2
    exit 1
fi

"$acpp" -O0 -g -fprofile-instr-generate -fcoverage-mapping \
    "$fixture" -o "$work_dir/probe"
"$acpp" -O2 "$dormancy_fixture" -o "$work_dir/dormancy-probe"

mkdir -p "$work_dir/home" "$work_dir/success/profiles"

run_probe() {
    local case_dir="$1"
    local output_path="$2"
    local mode="$3"
    set +e
    HOME="$work_dir/home" \
    LLVM_PROFILE_FILE="$case_dir/host.profraw" \
    ACPP_VISIBILITY_MASK=metal \
    ACPP_METAL_DEVICE_PROFILE_DIR="$output_path" \
        "$work_dir/probe" "$mode" >"$case_dir/probe.log" 2>&1
    local status=$?
    set -e
    cat "$case_dir/probe.log"
    return "$status"
}

if ! run_probe "$work_dir/success" "$work_dir/success/profiles" overflow; then
    echo "overflow-only profile unexpectedly failed" >&2
    exit 1
fi
overflow_count="$(find "$work_dir/success/profiles" -maxdepth 1 -name '*.overflow' | wc -l | tr -d ' ')"
proftext_count="$(find "$work_dir/success/profiles" -maxdepth 1 -name '*.proftext' | wc -l | tr -d ' ')"
if [[ "$overflow_count" != 1 || "$proftext_count" != 0 ]]; then
    echo "overflow-only profile mismatch: overflow=$overflow_count proftext=$proftext_count" >&2
    exit 1
fi
echo "overflow-only profile: PASS (overflow=1 proftext=0)"

expect_flush_failure() {
    local label="$1"
    local case_dir="$2"
    local output_path="$3"
    local mode="$4"
    local status=0
    run_probe "$case_dir" "$output_path" "$mode" || status=$?
    local retained_profiles
    retained_profiles="$(find "$case_dir" -type f \( -name '*.proftext' -o -name '*.overflow' \) | wc -l | tr -d ' ')"
    if [[ "$status" == 0 || "$retained_profiles" != 0 ]]; then
        echo "device profile flush failure mismatch: case=$label exit=$status accepted=$retained_profiles" >&2
        exit 1
    fi
    echo "device profile flush failure: PASS (case=$label exit=$status accepted=0)"
}

mkdir -p "$work_dir/regular-file"
: >"$work_dir/regular-file/output"
expect_flush_failure regular-file "$work_dir/regular-file" \
    "$work_dir/regular-file/output" overflow

mkdir -p "$work_dir/unwritable-overflow/output"
chmod a-w "$work_dir/unwritable-overflow/output"
expect_flush_failure unwritable-overflow "$work_dir/unwritable-overflow" \
    "$work_dir/unwritable-overflow/output" overflow

mkdir -p "$work_dir/unwritable-proftext/output"
chmod a-w "$work_dir/unwritable-proftext/output"
expect_flush_failure unwritable-proftext "$work_dir/unwritable-proftext" \
    "$work_dir/unwritable-proftext/output" ordinary

mkdir -p "$work_dir/short-write/output"
expect_flush_failure short-write "$work_dir/short-write" \
    "$work_dir/short-write/output" short-write

mkdir -p "$work_dir/dormancy/home" "$work_dir/dormancy/profiles"
HOME="$work_dir/dormancy/home" \
LLVM_PROFILE_FILE="$work_dir/dormancy/host.profraw" \
ACPP_VISIBILITY_MASK=metal \
ACPP_METAL_DEVICE_PROFILE_DIR="$work_dir/dormancy/profiles" \
    "$work_dir/dormancy-probe"
dormancy_count="$(find "$work_dir/dormancy/profiles" -maxdepth 1 -type f | wc -l | tr -d ' ')"
if [[ "$dormancy_count" != 0 ]]; then
    echo "normal-build device profile dormancy mismatch: files=$dormancy_count" >&2
    exit 1
fi
echo "normal-build device profile dormancy: PASS (files=0)"
