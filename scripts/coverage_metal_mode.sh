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
        acpp_prefix="${ACPP_PREFIX:-$PWD/.pgaccel/acpp/current}"
        acpp_info="${PGACCEL_ACPP_INFO:-$acpp_prefix/bin/acpp-info}"
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
        [ -x "$acpp_info" ] || {
            echo "error: hosted Metal compatibility mode requires executable AdaptiveCpp acpp-info" >&2
            exit 1
        }
        if ! runtime_info="$("$acpp_info" 2>&1)"; then
            echo "error: hosted Metal compatibility mode could not query the AdaptiveCpp runtime" >&2
            exit 1
        fi
        metal_devices="$(printf '%s\n' "$runtime_info" | tr -d '\r' | awk '
            /^Loaded backend [0-9]+: / {
                in_metal = ($0 ~ /: Metal$/)
                next
            }
            in_metal && /^[[:space:]]*Found device: / {
                sub(/^[[:space:]]*Found device: /, "")
                print
            }
        ')"
        [ "$metal_devices" = "Apple Paravirtual device" ] || {
            echo "error: hosted Metal compatibility mode requires exactly one AdaptiveCpp Metal Apple Paravirtual device" >&2
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
            "gpu_backend": "Metal",
            "gpu_runtime_probe": "acpp-info",
            "gpu_basic_tier": True,
            "host_reference_common_extended": True,
            "planner_calibration": "test_only_32_cu_reference",
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
