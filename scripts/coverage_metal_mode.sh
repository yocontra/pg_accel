#!/usr/bin/env bash
set -euo pipefail

output="${1:-}"
if [ -z "$output" ]; then
    echo "error: coverage Metal mode requires an output path" >&2
    exit 1
fi

mode="${PGACCEL_HOSTED_METAL_COMPATIBILITY:-0}"
case "$mode" in
    0)
        python3 - "$output" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text(
    json.dumps(
        {
            "schema_version": 1,
            "mode": "full_device",
            "host_reference_common_extended": False,
            "performance_evidence_eligible": False,
        },
        indent=2,
        sort_keys=True,
    )
    + "\n",
    encoding="utf-8",
)
PY
        ;;
    1)
        [ "$(uname -s)" = "Darwin" ] || {
            echo "error: hosted Metal compatibility mode requires Darwin" >&2
            exit 1
        }
        [ "$(uname -m)" = "arm64" ] || {
            echo "error: hosted Metal compatibility mode requires arm64" >&2
            exit 1
        }
        cpu_brand="$(sysctl -n machdep.cpu.brand_string)"
        logical_cpus="$(sysctl -n hw.logicalcpu)"
        memory_bytes="$(sysctl -n hw.memsize)"
        displays="$(system_profiler SPDisplaysDataType)"
        [ "$cpu_brand" = "Apple M1 (Virtual)" ] || {
            echo "error: hosted Metal compatibility mode requires the GitHub virtual M1" >&2
            exit 1
        }
        [ "$logical_cpus" = "3" ] || {
            echo "error: hosted Metal compatibility mode requires the 3-vCPU runner" >&2
            exit 1
        }
        case "$memory_bytes" in
            ''|*[!0-9]*)
                echo "error: hosted Metal compatibility memory is not numeric" >&2
                exit 1
                ;;
        esac
        if [ "$memory_bytes" -lt 7000000000 ] || [ "$memory_bytes" -gt 8589934592 ]; then
            echo "error: hosted Metal compatibility memory is outside the virtual-M1 envelope" >&2
            exit 1
        fi
        printf '%s\n' "$displays" | grep -Fq "Apple Paravirtual device" || {
            echo "error: hosted Metal compatibility mode requires Apple Paravirtual device" >&2
            exit 1
        }
        python3 - "$output" "$cpu_brand" "$logical_cpus" "$memory_bytes" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text(
    json.dumps(
        {
            "schema_version": 1,
            "mode": "hosted_virtual_m1_compatibility",
            "cpu_brand": sys.argv[2],
            "logical_cpus": int(sys.argv[3]),
            "memory_bytes": int(sys.argv[4]),
            "gpu_device": "Apple Paravirtual device",
            "gpu_basic_tier": True,
            "host_reference_common_extended": True,
            "reason": "common_extended_metallib_exceeds_900_kib_archive_oom_guard",
            "performance_evidence_eligible": False,
        },
        indent=2,
        sort_keys=True,
    )
    + "\n",
    encoding="utf-8",
)
PY
        ;;
    *)
        echo "error: PGACCEL_HOSTED_METAL_COMPATIBILITY must be 0 or 1" >&2
        exit 1
        ;;
esac
