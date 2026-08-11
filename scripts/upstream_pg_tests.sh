#!/usr/bin/env bash
# Run PostgreSQL's own regression and isolation schedules with and without
# pg_accel preloaded. Every run uses PostgreSQL's temporary-install harness;
# shared_preload_libraries therefore places the candidate hooks in every test
# session without installing extension SQL objects into the regression DB.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$repo_root/scripts/pg_versions.sh"

usage() {
    echo "usage: $0 <pg-major> <new-artifact-directory>" >&2
}

pg="${1:-}"
artifact_dir="${2:-}"
if [ -z "$pg" ] || [ -z "$artifact_dir" ] || [ "$#" -ne 2 ]; then
    usage
    exit 2
fi
pg="${pg#pg}"
pg_accel_require_supported_pg "$pg"

case "$artifact_dir" in
    /*) ;;
    *) artifact_dir="$PWD/$artifact_dir" ;;
esac
if [ -e "$artifact_dir" ]; then
    echo "error: artifact directory already exists: $artifact_dir" >&2
    exit 2
fi

scripts_pg_source="$repo_root/scripts/pg_source.sh"
"$scripts_pg_source" build "$pg"
version="$(pg_accel_pg_version_for_pg "$pg")"
build_dir="$PG_ACCEL_PG_ROOT/build/$version"
source_dir="$(pg_accel_pg_source_dir_for_version "$version")"
tarball="$(pg_accel_pg_tarball_for_version "$version")"
pg_config="$($scripts_pg_source pg-config "$pg")"
pkglibdir="$($pg_config --pkglibdir)"

module_file=""
for candidate in \
    "$pkglibdir/pg_accel.so" \
    "$pkglibdir/pg_accel.dylib" \
    "$pkglibdir/pg_accel.bundle"; do
    if [ -f "$candidate" ]; then
        module_file="$candidate"
        break
    fi
done
if [ -z "$module_file" ]; then
    echo "error: pg_accel is not installed for PostgreSQL $pg in $pkglibdir" >&2
    echo "       run: just install-pg-accel $pg" >&2
    exit 1
fi
case "$pkglibdir" in
    *"'"*|*$'\n'*)
        echo "error: PostgreSQL library path cannot be represented safely in temp config" >&2
        exit 1
        ;;
esac

hash_file() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        echo "error: neither shasum nor sha256sum is available" >&2
        return 1
    fi
}

provenance_tmp="$(mktemp -d "${TMPDIR:-/tmp}/pgaccel-upstream-provenance.XXXXXX")"
cleanup() {
    rm -rf -- "$provenance_tmp"
}
trap cleanup EXIT

git -C "$repo_root" diff --binary HEAD > "$provenance_tmp/git-diff.patch"
git -C "$repo_root" status --porcelain=v1 --untracked-files=all \
    > "$provenance_tmp/git-status.txt"
while IFS= read -r -d '' candidate_file; do
    if [ -f "$repo_root/$candidate_file" ]; then
        printf "%s  %s\n" "$(hash_file "$repo_root/$candidate_file")" "$candidate_file"
    fi
done < <(git -C "$repo_root" ls-files --cached --others --exclude-standard -z) \
    > "$provenance_tmp/source-files.sha256"

mkdir -p "$artifact_dir/config" "$artifact_dir/runs"

pristine_config="$artifact_dir/config/pristine.conf"
loaded_config="$artifact_dir/config/loaded.conf"
printf "%s\n" \
    "shared_preload_libraries = ''" \
    "log_min_messages = warning" \
    > "$pristine_config"
printf "%s\n" \
    "dynamic_library_path = '\$libdir:$pkglibdir'" \
    "shared_preload_libraries = 'pg_accel'" \
    "pg_accel.enabled = on" \
    "pg_accel.gpu_enabled = on" \
    "log_min_messages = warning" \
    > "$loaded_config"

git_head="$(git -C "$repo_root" rev-parse HEAD)"
git_tree="$(git -C "$repo_root" write-tree)"
git_dirty=false
if [ -s "$provenance_tmp/git-status.txt" ]; then
    git_dirty=true
fi
cp "$provenance_tmp/git-diff.patch" "$artifact_dir/git-diff.patch"
cp "$provenance_tmp/git-status.txt" "$artifact_dir/git-status.txt"
cp "$provenance_tmp/source-files.sha256" "$artifact_dir/source-files.sha256"
cat > "$artifact_dir/manifest.txt" <<EOF
schema_version=1
postgres_major=$pg
postgres_version=$version
postgres_tarball=$tarball
postgres_tarball_sha256=$(hash_file "$tarball")
postgres_source_dir=$source_dir
postgres_build_dir=$build_dir
postgres_configure=$($pg_config --configure)
pg_accel_git_head=$git_head
pg_accel_git_tree=$git_tree
pg_accel_git_dirty=$git_dirty
pg_accel_source_manifest_sha256=$(hash_file "$artifact_dir/source-files.sha256")
pg_accel_git_diff_sha256=$(hash_file "$artifact_dir/git-diff.patch")
pg_accel_module=$module_file
pg_accel_module_sha256=$(hash_file "$module_file")
loaded_session_contract=shared_preload_libraries loads pg_accel into the postmaster and every forked regression/isolation backend
suites=regression isolation
modes=pristine loaded
EOF

printf "mode\tsuite\tstatus\texit_code\n" > "$artifact_dir/results.tsv"
overall_status=0

copy_if_present() {
    source_path="$1"
    destination="$2"
    if [ -e "$source_path" ]; then
        cp -R "$source_path" "$destination"
    fi
}

run_suite() {
    mode="$1"
    suite="$2"
    suite_dir="$3"
    config="$4"
    run_dir="$artifact_dir/runs/$mode-$suite"
    mkdir -p "$run_dir"

    set +e
    make -C "$suite_dir" check TEMP_CONFIG="$config" > "$run_dir/command.log" 2>&1
    status=$?
    set -e

    copy_if_present "$suite_dir/regression.out" "$run_dir/regression.out"
    copy_if_present "$suite_dir/regression.diffs" "$run_dir/regression.diffs"
    copy_if_present "$suite_dir/results" "$run_dir/results"
    copy_if_present "$suite_dir/output_iso" "$run_dir/output_iso"
    copy_if_present "$suite_dir/log" "$run_dir/log"
    copy_if_present "$suite_dir/tmp_check/log" "$run_dir/tmp_check-log"
    copy_if_present "$suite_dir/tmp_check_iso/log" "$run_dir/tmp_check-iso-log"

    if [ "$status" -eq 0 ]; then
        label=pass
    else
        label=fail
        overall_status=1
    fi
    printf "%s\t%s\t%s\t%s\n" "$mode" "$suite" "$label" "$status" \
        >> "$artifact_dir/results.tsv"
}

regress_dir="$build_dir/src/test/regress"
isolation_dir="$build_dir/src/test/isolation"
for mode in pristine loaded; do
    if [ "$mode" = pristine ]; then
        config="$pristine_config"
    else
        config="$loaded_config"
    fi
    run_suite "$mode" regression "$regress_dir" "$config"
    run_suite "$mode" isolation "$isolation_dir" "$config"
done

(
    cd "$artifact_dir"
    find . -type f ! -name SHA256SUMS -print \
        | LC_ALL=C sort \
        | while IFS= read -r file; do
            if command -v shasum >/dev/null 2>&1; then
                shasum -a 256 "$file"
            else
                sha256sum "$file"
            fi
        done > SHA256SUMS
)

if [ "$overall_status" -ne 0 ]; then
    echo "PostgreSQL $pg upstream compatibility gate failed; see $artifact_dir/results.tsv" >&2
    exit 1
fi
echo "PostgreSQL $pg upstream compatibility gate passed; evidence: $artifact_dir"
