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
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/acpp-overflow-only.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT

if [[ -n "${ACPP_TEST_DYLD_LIBRARY_PATH:-}" ]]; then
    export DYLD_LIBRARY_PATH="$ACPP_TEST_DYLD_LIBRARY_PATH"
fi

if [[ ! -x "$acpp" ]]; then
    echo "AdaptiveCpp driver not found at $acpp" >&2
    exit 1
fi

"$acpp" -O0 -g -fprofile-instr-generate -fcoverage-mapping \
    "$fixture" -o "$work_dir/probe"

mkdir -p "$work_dir/home" "$work_dir/profiles"
HOME="$work_dir/home" \
LLVM_PROFILE_FILE="$work_dir/profiles/host.profraw" \
ACPP_VISIBILITY_MASK=metal \
ACPP_METAL_DEVICE_PROFILE_DIR="$work_dir/profiles" \
    "$work_dir/probe"

overflow_count="$(find "$work_dir/profiles" -maxdepth 1 -name '*.overflow' | wc -l | tr -d ' ')"
proftext_count="$(find "$work_dir/profiles" -maxdepth 1 -name '*.proftext' | wc -l | tr -d ' ')"
if [[ "$overflow_count" != 1 || "$proftext_count" != 0 ]]; then
    echo "overflow-only profile mismatch: overflow=$overflow_count proftext=$proftext_count" >&2
    exit 1
fi

echo "overflow-only profile: PASS (overflow=1 proftext=0)"
