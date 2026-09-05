#!/usr/bin/env bash
# Retain coverage from the manual Metal out-of-order overlap diagnostic.
#
# A real overlap is the preferred result. The pinned physical Apple Metal
# backend may instead report the known hazard-tracking serialization mode; that
# exact structural result is also useful coverage evidence. Every other exit or
# output shape fails closed.

set -uo pipefail

binary="${1:-}"
profile_dir="${2:-}"

if [ -z "$binary" ] || [ -z "$profile_dir" ]; then
    echo "usage: coverage_ooo_overlap.sh BINARY PROFILE_DIR" >&2
    exit 2
fi
if [ ! -x "$binary" ]; then
    echo "error: manual OOO overlap diagnostic is unavailable: $binary" >&2
    exit 1
fi
if [ ! -d "$profile_dir" ]; then
    echo "error: manual OOO overlap profile directory is unavailable: $profile_dir" >&2
    exit 1
fi

output=""
status=0
if output="$(env \
    LLVM_PROFILE_FILE="$profile_dir/pgaccel-ooo-%p-%m.profraw" \
    ACPP_METAL_DEVICE_PROFILE_DIR="$profile_dir" \
    "$binary" 2>&1)"; then
    status=0
else
    status=$?
fi
printf '%s\n' "$output"

mode=""
case "$status" in
    0)
        if ! grep -Fxq "test_ooo_overlap: OK" <<< "$output" \
            || [ "$(grep -Ec '^span_ms .*spans_overlap=yes improved=yes$' <<< "$output")" -ne 1 ] \
            || grep -Eq '^span_ms .*spans_overlap=no ' <<< "$output"; then
            echo "error: successful manual OOO overlap diagnostic did not prove real improved overlap" >&2
            exit 1
        fi
        mode="real-overlap success"
        ;;
    1)
        if ! grep -Fxq \
            "test_ooo_overlap: resident/reduce GPU spans did not overlap" \
            <<< "$output" \
            || [ "$(grep -Ec '^span_ms .*spans_overlap=no improved=(yes|no)$' <<< "$output")" -ne 1 ] \
            || grep -Eq '^span_ms .*spans_overlap=yes ' <<< "$output"; then
            echo "error: failed manual OOO overlap diagnostic did not report the pinned structural mode" >&2
            exit 1
        fi
        mode="expected no-overlap structural result"
        ;;
    *)
        echo "error: manual OOO overlap diagnostic exited $status; expected 0 or 1" >&2
        exit 1
        ;;
esac

if find "$profile_dir" -maxdepth 1 -type f -name '*.overflow' -print -quit \
    | grep -q .; then
    echo "error: manual OOO overlap diagnostic overflowed a device profile" >&2
    exit 1
fi

host_profile_count="$(find "$profile_dir" -maxdepth 1 -type f \
    -name 'pgaccel-ooo-*.profraw' -size +0c | wc -l | tr -d ' ')"
device_profile_count="$(find "$profile_dir" -maxdepth 1 -type f \
    -name '*.proftext' -size +0c | wc -l | tr -d ' ')"
if [ "$host_profile_count" -eq 0 ] || [ "$device_profile_count" -eq 0 ]; then
    echo "error: manual OOO overlap diagnostic did not emit host and device coverage" >&2
    exit 1
fi

echo "manual OOO overlap coverage: PASS ($mode; host_profiles=$host_profile_count device_profiles=$device_profile_count)"
