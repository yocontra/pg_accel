#!/usr/bin/env bash
# Canonical PostgreSQL version policy for pg_accel build/test tooling.
#
# PostgreSQL 18 is the default pgrx extension target. PostgreSQL 19 beta is a
# required release-matrix target backed by pgrx's real pg19 feature.

set -euo pipefail

PG_ACCEL_DEFAULT_PG_MAJOR="${PG_ACCEL_DEFAULT_PG_MAJOR:-18}"
PG_ACCEL_SUPPORTED_PG_MAJORS="${PG_ACCEL_SUPPORTED_PG_MAJORS:-18 19}"
PG_ACCEL_PREVIEW_PG_MAJORS="${PG_ACCEL_PREVIEW_PG_MAJORS:-19}"
PG_ACCEL_PGRX_VERSION="${PG_ACCEL_PGRX_VERSION:-0.19.1}"
PG_ACCEL_REPO_ROOT="${PG_ACCEL_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"

# Source-built PostgreSQL defaults match the pgrx extension support matrix.
PG_ACCEL_SOURCE_PG_MAJORS="${PG_ACCEL_SOURCE_PG_MAJORS:-18 19}"
PG_ACCEL_ACPP_PREFIX="${PG_ACCEL_ACPP_PREFIX:-$PG_ACCEL_REPO_ROOT/.pgaccel/acpp/current}"

source "$PG_ACCEL_REPO_ROOT/scripts/pg_source.sh"

pg_accel_supported_pg_majors() {
    printf '%s\n' $PG_ACCEL_SUPPORTED_PG_MAJORS
}

pg_accel_source_pg_majors() {
    printf '%s\n' $PG_ACCEL_SOURCE_PG_MAJORS
}

pg_accel_default_pg_major() {
    printf '%s\n' "$PG_ACCEL_DEFAULT_PG_MAJOR"
}

pg_accel_is_supported_pg() {
    local pg="${1#pg}"
    local supported
    for supported in $PG_ACCEL_SUPPORTED_PG_MAJORS; do
        if [ "$pg" = "$supported" ]; then
            return 0
        fi
    done
    return 1
}

pg_accel_require_supported_pg() {
    local pg="${1#pg}"
    if ! pg_accel_is_supported_pg "$pg"; then
        echo "error: PostgreSQL $pg is not in PG_ACCEL_SUPPORTED_PG_MAJORS=[$PG_ACCEL_SUPPORTED_PG_MAJORS]" >&2
        return 1
    fi
}

pg_accel_is_preview_pg() {
    local pg="${1#pg}"
    local preview
    for preview in $PG_ACCEL_PREVIEW_PG_MAJORS; do
        if [ "$pg" = "$preview" ]; then
            return 0
        fi
    done
    return 1
}

pg_accel_pgrx_feature_for_pg() {
    local pg="${1#pg}"
    printf 'pg%s\n' "$pg"
}

pg_accel_pgrx_supports_pg() {
    local pg="${1#pg}"
    local feature
    feature="$(pg_accel_pgrx_feature_for_pg "$pg")"
    grep -Eq "^[[:space:]]*${feature}[[:space:]]*=" "$PG_ACCEL_REPO_ROOT/pg_accel/Cargo.toml"
}

pg_accel_require_pgrx_support() {
    local pg="${1#pg}"
    pg_accel_require_supported_pg "$pg" || return 1
    if ! pg_accel_pgrx_supports_pg "$pg"; then
        echo "error: PostgreSQL $pg is supported by pg_accel policy but cannot resolve pgrx feature pg$pg" >&2
        return 1
    fi
}

pg_accel_highest_buildable_pg_major() {
    local pg
    local highest=""
    for pg in $PG_ACCEL_SUPPORTED_PG_MAJORS; do
        if pg_accel_pgrx_supports_pg "$pg"; then
            highest="$pg"
        fi
    done
    if [ -z "$highest" ]; then
        echo "error: none of PG_ACCEL_SUPPORTED_PG_MAJORS=[$PG_ACCEL_SUPPORTED_PG_MAJORS] resolves through Cargo/pgrx" >&2
        return 1
    fi
    printf '%s\n' "$highest"
}

pg_accel_pgrx_has_pg_config() {
    local pg="${1#pg}"
    cargo pgrx info pg-config "pg$pg" >/dev/null 2>&1
}

pg_accel_require_pgrx_pg_config() {
    local pg="${1#pg}"
    if ! pg_accel_pgrx_has_pg_config "$pg"; then
        echo "error: PostgreSQL $pg is not initialized in pgrx. Run: just setup-pgrx $pg" >&2
        return 1
    fi
}

pg_accel_highest_usable_pg_major() {
    local pg
    local highest=""
    for pg in $PG_ACCEL_SUPPORTED_PG_MAJORS; do
        if pg_accel_pgrx_supports_pg "$pg" && pg_accel_pgrx_has_pg_config "$pg"; then
            highest="$pg"
        fi
    done
    if [ -n "$highest" ]; then
        printf '%s\n' "$highest"
        return 0
    fi
    pg_accel_highest_buildable_pg_major
}

pg_accel_buildable_default_pg_major() {
    local default_pg
    default_pg="$(pg_accel_default_pg_major)"
    if pg_accel_pgrx_supports_pg "$default_pg"; then
        printf '%s\n' "$default_pg"
    else
        pg_accel_highest_usable_pg_major
    fi
}

pg_accel_skip_if_preview_without_pgrx() {
    local pg="${1#pg}"
    if pg_accel_require_pgrx_support "$pg"; then
        return 1
    fi
    # Legacy callers use this function in an `if`, where a nonzero return would
    # otherwise be swallowed. Missing support is a hard policy failure now.
    exit 1
}

pg_accel_pg_config_for_pg() {
    local pg="${1#pg}"
    cargo pgrx info pg-config "pg$pg"
}

pg_accel_source_pg_config_for_required_pg() {
    local pg="${1#pg}"
    local pg_config
    pg_config="$(pg_accel_source_pg_config_for_pg "$pg")"
    if [ ! -x "$pg_config" ]; then
        echo "error: source-built PostgreSQL $pg not found at $pg_config" >&2
        echo "       run: just setup-pg-source $pg" >&2
        return 1
    fi
    printf '%s\n' "$pg_config"
}

pg_accel_disable_uninstalled_pg_accel_preload() {
    local pg="${1#pg}"
    local pg_config="${2:-}"
    local pkglibdir conf tmp

    if [ -z "$pg_config" ]; then
        pg_config="$(pg_accel_pg_config_for_pg "$pg" 2>/dev/null || true)"
    fi
    [ -n "$pg_config" ] && [ -x "$pg_config" ] || return 0

    pkglibdir="$("$pg_config" --pkglibdir)"
    if [ -e "$pkglibdir/pg_accel.so" ] || [ -e "$pkglibdir/pg_accel.dylib" ]; then
        return 0
    fi

    conf="$(pg_accel_pgrx_data_dir_for_pg "$pg")/postgresql.conf"
    [ -f "$conf" ] || return 0
    if ! grep -Eq "^[[:space:]]*shared_preload_libraries[[:space:]]*=[[:space:]]*'pg_accel'[[:space:]]*$" "$conf"; then
        return 0
    fi

    tmp="$(mktemp "$conf.XXXXXX")"
    sed "s/^\([[:space:]]*shared_preload_libraries[[:space:]]*=[[:space:]]*'pg_accel'[[:space:]]*\)$/# \1 # disabled by setup-pgrx until pg_accel is installed/" "$conf" > "$tmp"
    cp "$conf" "$conf.pgaccel-preload.bak"
    mv "$tmp" "$conf"
    echo "disabled stale pg_accel preload in $conf; backup at $conf.pgaccel-preload.bak"
}

pg_accel_acpp_prefix() {
    printf '%s\n' "$PG_ACCEL_ACPP_PREFIX"
}

pg_accel_pgrx_data_dir_for_pg() {
    local pg="${1#pg}"
    printf '%s/.pgrx/data-%s\n' "$HOME" "$pg"
}

pg_accel_pgrx_log_for_pg() {
    local pg="${1#pg}"
    printf '%s/.pgrx/%s.log\n' "$HOME" "$pg"
}

pg_accel_pgrx_port_for_pg() {
    local pg="${1#pg}"
    local data_dir conf port
    data_dir="$(pg_accel_pgrx_data_dir_for_pg "$pg")"
    conf="$data_dir/postgresql.conf"
    if [ -f "$conf" ]; then
        port="$(awk -F= '/^port[[:space:]]*=/ { gsub(/[[:space:]]/, "", $2); print $2 }' "$conf" | tail -1)"
        if [ -n "${port:-}" ]; then
            printf '%s\n' "$port"
            return 0
        fi
    fi
    printf '288%s\n' "$pg"
}
