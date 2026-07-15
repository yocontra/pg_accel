#!/usr/bin/env bash
# Build and locate repo-local PostgreSQL source installs for pg_accel.

set -euo pipefail

PG_ACCEL_REPO_ROOT="${PG_ACCEL_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
PG_ACCEL_PG_ROOT="${PG_ACCEL_PG_ROOT:-$PG_ACCEL_REPO_ROOT/.pgaccel/postgres}"
PG_ACCEL_PG_MAKE_JOBS="${PG_ACCEL_PG_MAKE_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"
PG_ACCEL_PG18_VERSION="${PG_ACCEL_PG18_VERSION:-18.4}"
PG_ACCEL_PG19_VERSION="${PG_ACCEL_PG19_VERSION:-19beta1}"
PG_ACCEL_PG_DOWNLOAD_BASE="${PG_ACCEL_PG_DOWNLOAD_BASE:-https://ftp.postgresql.org/pub/source}"
PG_ACCEL_PG_CONFIGURE_FLAGS="${PG_ACCEL_PG_CONFIGURE_FLAGS:---without-icu}"

pg_accel_pg_major_from_version() {
    local version="${1#pg}"
    if [[ "$version" =~ ^([0-9]+) ]]; then
        printf '%s\n' "${BASH_REMATCH[1]}"
        return 0
    fi
    echo "error: invalid PostgreSQL version: $1" >&2
    return 1
}

pg_accel_pg_version_for_pg() {
    local pg="${1#pg}"
    if [[ "$pg" == *.* ]]; then
        printf '%s\n' "$pg"
        return 0
    fi

    local var="PG_ACCEL_PG${pg}_VERSION"
    local value="${!var:-}"
    if [ -n "$value" ]; then
        printf '%s\n' "$value"
        return 0
    fi

    echo "error: no PostgreSQL source version configured for major $pg; set $var, for example ${var}=18.4" >&2
    return 1
}

pg_accel_pg_prefix_for_version() {
    local version="$1"
    printf '%s/install/%s\n' "$PG_ACCEL_PG_ROOT" "$version"
}

pg_accel_pg_prefix_for_pg() {
    local version
    version="$(pg_accel_pg_version_for_pg "$1")"
    pg_accel_pg_prefix_for_version "$version"
}

pg_accel_source_pg_config_for_pg() {
    local prefix
    prefix="$(pg_accel_pg_prefix_for_pg "$1")"
    printf '%s/bin/pg_config\n' "$prefix"
}

pg_accel_pg_install_is_usable() {
    local pg_config="$1"
    local platform="${2:-$(uname -s)}"
    local cppflags token sysroot="" expect_sysroot=0

    [ -x "$pg_config" ] || return 1
    [ "$platform" = "Darwin" ] || return 0

    cppflags="$("$pg_config" --cppflags 2>/dev/null)" || return 1
    for token in $cppflags; do
        if [ "$expect_sysroot" -eq 1 ]; then
            sysroot="$token"
            break
        fi
        case "$token" in
            -isysroot)
                expect_sysroot=1
                ;;
            -isysroot*)
                sysroot="${token#-isysroot}"
                break
                ;;
        esac
    done

    if [ -n "$sysroot" ] && [ ! -d "$sysroot" ]; then
        echo "PostgreSQL pg_config references missing macOS SDK: $sysroot" >&2
        return 1
    fi
    return 0
}

pg_accel_pg_tarball_for_version() {
    local version="$1"
    printf '%s/distfiles/postgresql-%s.tar.bz2\n' "$PG_ACCEL_PG_ROOT" "$version"
}

pg_accel_pg_source_dir_for_version() {
    local version="$1"
    printf '%s/src/postgresql-%s\n' "$PG_ACCEL_PG_ROOT" "$version"
}

pg_accel_download_pg() {
    local version="$1"
    local tarball url
    tarball="$(pg_accel_pg_tarball_for_version "$version")"
    url="$PG_ACCEL_PG_DOWNLOAD_BASE/v${version}/postgresql-${version}.tar.bz2"
    mkdir -p "$(dirname "$tarball")"
    if [ -f "$tarball" ]; then
        echo "PostgreSQL $version tarball already present: $tarball"
        return 0
    fi
    echo "Downloading PostgreSQL $version from $url"
    curl -fL "$url" -o "$tarball"
}

pg_accel_unpack_pg() {
    local version="$1"
    local tarball source_dir
    tarball="$(pg_accel_pg_tarball_for_version "$version")"
    source_dir="$(pg_accel_pg_source_dir_for_version "$version")"
    if [ -f "$source_dir/configure" ]; then
        echo "PostgreSQL $version source already unpacked: $source_dir"
        return 0
    fi
    mkdir -p "$(dirname "$source_dir")"
    tar -xjf "$tarball" -C "$(dirname "$source_dir")"
}

pg_accel_build_pg_version() {
    local version="$1"
    local source_dir build_dir prefix pg_config stale_install=0
    source_dir="$(pg_accel_pg_source_dir_for_version "$version")"
    build_dir="$PG_ACCEL_PG_ROOT/build/$version"
    prefix="$(pg_accel_pg_prefix_for_version "$version")"
    pg_config="$prefix/bin/pg_config"

    pg_accel_download_pg "$version"
    pg_accel_unpack_pg "$version"

    if pg_accel_pg_install_is_usable "$pg_config"; then
        echo "PostgreSQL $version already installed: $prefix"
        return 0
    fi
    if [ -x "$pg_config" ]; then
        stale_install=1
        echo "PostgreSQL $version install is stale; rebuilding in place: $prefix"
    fi

    mkdir -p "$build_dir" "$prefix"
    (
        cd "$build_dir"
        if [ "$stale_install" -eq 1 ] && [ -f Makefile ]; then
            make clean
        fi
        "$source_dir/configure" \
            --prefix="$prefix" \
            --enable-debug \
            --enable-cassert \
            $PG_ACCEL_PG_CONFIGURE_FLAGS
        make -j"$PG_ACCEL_PG_MAKE_JOBS"
        make install
    )
}

pg_accel_build_pg() {
    local version
    version="$(pg_accel_pg_version_for_pg "$1")"
    pg_accel_build_pg_version "$version"
}

pg_accel_print_env_for_pg() {
    local pg="$1"
    local pg_config prefix
    pg_config="$(pg_accel_source_pg_config_for_pg "$pg")"
    prefix="$(dirname "$(dirname "$pg_config")")"
    cat <<EOF
export PG_CONFIG="$pg_config"
export PATH="$prefix/bin:\$PATH"
EOF
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    cmd="${1:-}"
    case "$cmd" in
        build)
            pg_accel_build_pg "${2:?usage: $0 build <pg-major-or-version>}"
            ;;
        pg-config)
            pg_config="$(pg_accel_source_pg_config_for_pg "${2:?usage: $0 pg-config <pg-major-or-version>}")"
            [ -x "$pg_config" ] || {
                echo "error: $pg_config does not exist; run: $0 build ${2#pg}" >&2
                exit 1
            }
            printf '%s\n' "$pg_config"
            ;;
        prefix)
            pg_accel_pg_prefix_for_pg "${2:?usage: $0 prefix <pg-major-or-version>}"
            ;;
        env)
            pg_accel_print_env_for_pg "${2:?usage: $0 env <pg-major-or-version>}"
            ;;
        *)
            echo "usage: $0 {build|pg-config|prefix|env} <pg-major-or-version>" >&2
            exit 2
            ;;
    esac
fi
