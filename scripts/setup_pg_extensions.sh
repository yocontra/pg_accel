#!/usr/bin/env bash
# Install third-party PostgreSQL extensions required by pg_accel tests into
# the repo-local PostgreSQL tree.

set -euo pipefail

PG_ACCEL_REPO_ROOT="${PG_ACCEL_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"

source "$PG_ACCEL_REPO_ROOT/scripts/pg_versions.sh"

requested="${1:-}"
if [ -z "$requested" ]; then
    pg="$(pg_accel_default_pg_major)"
else
    pg="${requested#pg}"
fi

pg_accel_require_supported_pg "$pg"
if pg_accel_skip_if_preview_without_pgrx "$pg"; then
    exit 0
fi

pg_config="$(pg_accel_source_pg_config_for_required_pg "$pg")"
pg_version="$("$pg_config" --version | awk '{print $2}')"
pg_major="${pg_version%%.*}"
bindir="$("$pg_config" --bindir)"
sharedir="$("$pg_config" --sharedir)"
pkglibdir="$("$pg_config" --pkglibdir)"
extdir="$sharedir/extension"

mkdir -p "$extdir" "$pkglibdir"

EXT_DIRS=()
LIB_DIRS=()

add_ext_dir() {
    if [ -d "$1" ]; then
        EXT_DIRS+=("$1")
    fi
    return 0
}

add_lib_dir() {
    if [ -d "$1" ]; then
        LIB_DIRS+=("$1")
    fi
    return 0
}

add_env_dirs() {
    local value="$1" kind="$2" old_ifs dir
    [ -n "$value" ] || return 0
    old_ifs="$IFS"
    IFS=':'
    for dir in $value; do
        if [ "$kind" = "ext" ]; then
            add_ext_dir "$dir"
        else
            add_lib_dir "$dir"
        fi
    done
    IFS="$old_ifs"
}

collect_packaged_dirs() {
    EXT_DIRS=()
    LIB_DIRS=()

    add_env_dirs "${PG_ACCEL_PG_EXTENSION_SHARE_DIRS:-}" ext
    add_env_dirs "${PG_ACCEL_PG_EXTENSION_LIB_DIRS:-}" lib

    add_ext_dir "/opt/homebrew/share/postgresql@${pg_major}/extension"
    add_ext_dir "/usr/local/share/postgresql@${pg_major}/extension"
    add_ext_dir "/usr/share/postgresql/${pg_major}/extension"
    add_ext_dir "/usr/pgsql-${pg_major}/share/extension"

    add_lib_dir "/opt/homebrew/lib/postgresql@${pg_major}"
    add_lib_dir "/usr/local/lib/postgresql@${pg_major}"
    add_lib_dir "/usr/lib/postgresql/${pg_major}/lib"
    add_lib_dir "/usr/pgsql-${pg_major}/lib"

    if command -v brew >/dev/null 2>&1; then
        local brew_prefix formula_prefix
        brew_prefix="$(brew --prefix 2>/dev/null || true)"
        if [ -n "$brew_prefix" ]; then
            add_ext_dir "$brew_prefix/share/postgresql@${pg_major}/extension"
            add_lib_dir "$brew_prefix/lib/postgresql@${pg_major}"
        fi
        for formula in postgis "postgresql@${pg_major}"; do
            formula_prefix="$(brew --prefix "$formula" 2>/dev/null || true)"
            if [ -n "$formula_prefix" ]; then
                add_ext_dir "$formula_prefix/share/postgresql@${pg_major}/extension"
                add_lib_dir "$formula_prefix/lib/postgresql@${pg_major}"
            fi
        done
    fi
}

candidate_has_ext_file() {
    local pattern="$1" dir src
    for dir in "${EXT_DIRS[@]}"; do
        for src in "$dir"/$pattern; do
            [ -e "$src" ] && return 0
        done
    done
    return 1
}

maybe_install_postgis_package() {
    if candidate_has_ext_file "postgis.control"; then
        return 0
    fi
    if [ "$(uname -s)" = "Darwin" ] && command -v brew >/dev/null 2>&1; then
        if ! brew list --versions postgis >/dev/null 2>&1; then
            echo "Installing PostGIS package with Homebrew..."
            brew install postgis
        fi
    fi
}

copy_ext_patterns() {
    local pattern dir src target copied=0
    for dir in "${EXT_DIRS[@]}"; do
        for pattern in "$@"; do
            for src in "$dir"/$pattern; do
                [ -e "$src" ] || continue
                target="$extdir/$(basename "$src")"
                rm -f "$target"
                install -m 0644 "$src" "$target"
                copied=1
            done
        done
    done
    return "$copied"
}

copy_lib_patterns() {
    local pattern dir src target copied=0
    for dir in "${LIB_DIRS[@]}"; do
        for pattern in "$@"; do
            for src in "$dir"/$pattern; do
                [ -e "$src" ] || continue
                target="$pkglibdir/$(basename "$src")"
                rm -f "$target"
                install -m 0755 "$src" "$target"
                copied=1
            done
        done
    done
    return "$copied"
}

have_module() {
    local module="$1"
    [ -f "$pkglibdir/${module}.so" ] || [ -f "$pkglibdir/${module}.dylib" ]
}

have_h3() {
    [ -f "$extdir/h3.control" ] && have_module h3
}

have_postgis() {
    [ -f "$extdir/postgis.control" ] &&
        [ -f "$extdir/postgis_raster.control" ] &&
        have_module postgis-3 &&
        have_module postgis_raster-3
}

install_packaged_postgis() {
    copy_ext_patterns \
        "postgis*.control" "postgis*.sql" \
        "address_standardizer*.control" "address_standardizer*.sql" || true
    copy_lib_patterns \
        "postgis*.so" "postgis*.dylib" \
        "address_standardizer*.so" "address_standardizer*.dylib" || true
}

install_packaged_h3() {
    copy_ext_patterns "h3*.control" "h3*.sql" || true
    copy_lib_patterns "h3*.so" "h3*.dylib" || true
}

build_h3_pg_from_source() {
    local repo ref src build jobs
    repo="${PG_ACCEL_H3_PG_REPO:-https://github.com/postgis/h3-pg.git}"
    ref="${PG_ACCEL_H3_PG_REF:-v4.2.3}"
    src="$PG_ACCEL_REPO_ROOT/.pgaccel/build/h3-pg-src"
    build="$PG_ACCEL_REPO_ROOT/.pgaccel/build/h3-pg-pg${pg_major}"
    jobs="${PG_ACCEL_EXT_MAKE_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"

    if [ ! -d "$src/.git" ]; then
        mkdir -p "$(dirname "$src")"
        git clone --depth 1 --branch "$ref" "$repo" "$src"
    else
        git -C "$src" fetch --depth 1 origin "$ref" >/dev/null 2>&1 || true
        git -C "$src" checkout --detach "$ref" >/dev/null
    fi

    PATH="$bindir:$PATH" cmake -S "$src" -B "$build" \
        -DCMAKE_BUILD_TYPE=Release \
        -DPOSTGRESQL_VERSION="$pg_major" \
        -DBUILD_TESTING=OFF
    cmake --build "$build" --parallel "$jobs"
    cmake --install "$build" --component h3-pg
}

collect_packaged_dirs
maybe_install_postgis_package
collect_packaged_dirs

if ! have_postgis; then
    install_packaged_postgis
fi

if ! have_postgis; then
    cat >&2 <<EOF
error: could not install PostGIS into $("$pg_config" --prefix)

Install same-major PostGIS package artifacts, then rerun:
  macOS/Homebrew: brew install postgis
  Debian/Ubuntu: install postgresql-${pg_major}-postgis-3

You can also point PG_ACCEL_PG_EXTENSION_SHARE_DIRS and
PG_ACCEL_PG_EXTENSION_LIB_DIRS at directories containing postgis.control and
postgis-3.{so,dylib}.
EOF
    exit 1
fi

if ! have_h3; then
    install_packaged_h3
fi

if ! have_h3; then
    echo "Building h3-pg ${PG_ACCEL_H3_PG_REF:-v4.2.3} for PostgreSQL ${pg_version}..."
    build_h3_pg_from_source
fi

if ! have_h3; then
    echo "error: h3-pg install did not produce h3.control and h3 module in $("$pg_config" --prefix)" >&2
    exit 1
fi

echo "PostgreSQL ${pg_version} test extensions installed:"
echo "  h3:             $extdir/h3.control"
echo "  postgis:        $extdir/postgis.control"
echo "  postgis_raster: $extdir/postgis_raster.control"
