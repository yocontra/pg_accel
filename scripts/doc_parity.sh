#!/usr/bin/env bash
# Strict documentation parity gate. The Python implementation keeps citation,
# GUC, adapter, and planner-capability parsing structured and testable.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$script_dir/doc_parity.py" "$@"
