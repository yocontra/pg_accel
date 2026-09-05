#!/usr/bin/env bash
# Capture failure-safe Metal/pgx diagnostics without turning partial output
# into release evidence. Intended for an `if: always()` CI step.

set -u

pg="${1:-}"
diagnostic_dir="${2:-}"
step_outcomes="${3:-unknown}"
if [ -z "$pg" ] || [ -z "$diagnostic_dir" ]; then
    echo "usage: $0 <pg-major> <output-dir> [step-outcomes]" >&2
    exit 2
fi

mkdir -p "$diagnostic_dir"
{
    date -u
    uname -a
    sysctl -n machdep.cpu.brand_string
    sysctl -n hw.logicalcpu
    sysctl -n hw.memsize
    git rev-parse HEAD
    git status --short
    printf 'step_outcomes=%s\n' "$step_outcomes"
} > "$diagnostic_dir/runner-and-step-metadata.txt" 2>&1 || true

pgrx_log="$HOME/.pgrx/${pg}.log"
if [ -f "$pgrx_log" ]; then
    cp "$pgrx_log" "$diagnostic_dir/pgrx-pg${pg}.log" || true
else
    printf 'missing: %s\n' "$pgrx_log" \
        > "$diagnostic_dir/pgrx-pg${pg}.missing.txt"
fi

acpp_info=.pgaccel/acpp/current/bin/acpp-info
if [ -x "$acpp_info" ]; then
    "$acpp_info" > "$diagnostic_dir/acpp-info.txt" 2>&1 || true
else
    printf 'AdaptiveCpp runtime is unavailable\n' \
        > "$diagnostic_dir/acpp-info.missing.txt"
fi

acpp_provenance=.pgaccel/acpp/current/pg_accel-acpp-provenance.txt
if [ -f "$acpp_provenance" ]; then
    cp "$acpp_provenance" "$diagnostic_dir/" || true
fi

cache_dir="${ACPP_APPDB_DIR:-$HOME/.acpp/apps/global/jit-cache}"
if [ -d "$cache_dir" ]; then
    find "$cache_dir" -maxdepth 1 -type f -exec wc -c {} + 2>/dev/null |
        LC_ALL=C sort -n > "$diagnostic_dir/jit-cache-files.txt" || true
else
    printf 'missing: %s\n' "$cache_dir" \
        > "$diagnostic_dir/jit-cache.missing.txt"
fi

# Diagnostics are best-effort by design. A failed release gate remains failed;
# a missing optional diagnostic must not obscure its original status.
exit 0
