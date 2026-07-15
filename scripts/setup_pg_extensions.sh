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

pg_accel_require_pgrx_support "$pg"

pg_config="$(pg_accel_source_pg_config_for_required_pg "$pg")"
pg_version="$("$pg_config" --version | awk '{print $2}')"
pg_major="$(pg_accel_pg_major_from_version "$pg_version")"
bindir="$("$pg_config" --bindir)"
prefix="$(dirname "$bindir")"
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
    ((${#EXT_DIRS[@]} > 0)) || return 1
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
    ((${#EXT_DIRS[@]} > 0)) || return 1
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
    ((${#LIB_DIRS[@]} > 0)) || return 1
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

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

build_postgis_from_source() {
    local version url expected_sha archive src build jobs actual_sha tmp
    local path_value pkg_config_value dependency dependency_prefix directory
    version="${PG_ACCEL_POSTGIS_VERSION:-3.6.4}"
    url="${PG_ACCEL_POSTGIS_URL:-https://download.osgeo.org/postgis/source/postgis-${version}.tar.gz}"
    expected_sha="${PG_ACCEL_POSTGIS_SHA256:-ed8dc6679f1e06f7b113592b04cde2a7e00f1b1e681294c8ca2204058990cec6}"
    archive="$PG_ACCEL_REPO_ROOT/.pgaccel/distfiles/postgis-${version}.tar.gz"
    src="$PG_ACCEL_REPO_ROOT/.pgaccel/build/postgis-${version}-src"
    build="$PG_ACCEL_REPO_ROOT/.pgaccel/build/postgis-${version}-pg${pg_version}"
    jobs="${PG_ACCEL_EXT_MAKE_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"

    mkdir -p "$(dirname "$archive")" "$(dirname "$src")"
    if [ ! -f "$archive" ]; then
        tmp="${archive}.tmp.$$"
        echo "Downloading PostGIS ${version} source..."
        if ! curl -fL --retry 3 --retry-delay 1 "$url" -o "$tmp"; then
            rm -f "$tmp"
            return 1
        fi
        mv "$tmp" "$archive"
    fi

    actual_sha="$(sha256_file "$archive")"
    if [ "$actual_sha" != "$expected_sha" ]; then
        echo "error: PostGIS ${version} source checksum mismatch" >&2
        echo "  expected: $expected_sha" >&2
        echo "  actual:   $actual_sha" >&2
        echo "  archive:  $archive" >&2
        return 1
    fi

    if [ ! -f "$src/.pgaccel-source-${expected_sha}" ]; then
        rm -rf "$src"
        mkdir -p "$src"
        tar -xzf "$archive" --strip-components=1 -C "$src"
        touch "$src/.pgaccel-source-${expected_sha}"
    fi
    mkdir -p "$build"

    path_value="$PATH"
    pkg_config_value="${PKG_CONFIG_PATH:-}"
    if command -v brew >/dev/null 2>&1; then
        for dependency in geos gdal proj libxml2 json-c pcre2 gettext; do
            dependency_prefix="$(brew --prefix "$dependency" 2>/dev/null || true)"
            [ -n "$dependency_prefix" ] || continue
            [ ! -d "$dependency_prefix/bin" ] || path_value="$dependency_prefix/bin:$path_value"
            for directory in lib/pkgconfig share/pkgconfig; do
                [ ! -d "$dependency_prefix/$directory" ] || \
                    pkg_config_value="$dependency_prefix/$directory${pkg_config_value:+:$pkg_config_value}"
            done
        done
    fi

    echo "Building PostGIS ${version} for PostgreSQL ${pg_version}..."
    (
        cd "$build"
        export PATH="$bindir:$path_value"
        export PKG_CONFIG_PATH="$pkg_config_value"
        "$src/configure" \
            --with-pgconfig="$pg_config" \
            --without-topology \
            --without-address-standardizer \
            --without-protobuf \
            --without-sfcgal
        make -j "$jobs"
        make install
    )
}

collect_packaged_dirs
maybe_install_postgis_package
collect_packaged_dirs

if ! have_postgis; then
    install_packaged_postgis
fi

if ! have_postgis; then
    build_postgis_from_source
fi

if ! have_postgis; then
    cat >&2 <<EOF
error: could not install PostGIS into $prefix

Install same-major PostGIS package artifacts, then rerun:
  macOS/Homebrew: brew install postgis
  Debian/Ubuntu: install postgresql-${pg_major}-postgis-3

You can also point PG_ACCEL_PG_EXTENSION_SHARE_DIRS and
PG_ACCEL_PG_EXTENSION_LIB_DIRS at directories containing postgis.control and
postgis-3.{so,dylib}, or override PG_ACCEL_POSTGIS_URL and
PG_ACCEL_POSTGIS_SHA256 for a different source archive.
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
    echo "error: h3-pg install did not produce h3.control and h3 module in $prefix" >&2
    exit 1
fi

echo "PostgreSQL ${pg_version} test extensions installed:"
echo "  h3:             $extdir/h3.control"
echo "  postgis:        $extdir/postgis.control"
echo "  postgis_raster: $extdir/postgis_raster.control"
