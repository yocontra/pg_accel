#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/raw-integer-count-20261026"
major=${1:?major required}
stage=${2:?fresh stage required}
case "$major" in 18) version=18.4;; 19) version=19beta1;; *) exit 2;; esac
case "$stage" in ''|*[!a-zA-Z0-9_-]*) exit 2;; esac
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version"
build_dir="$task_root/build$major-$stage"
test ! -e "$build_dir"
test -z "${PGOPTIONS:-}"
mkdir "$build_dir"
{
 printf '%s\n' Cargo.toml Cargo.lock pg_accel/Cargo.toml pg_accel/build.rs pg_accel_bench/Cargo.toml pgaccel-kernels/CMakeLists.txt .acpp-version
 rg --files pg_accel/src pg_accel_bench/src pgaccel-kernels/src pgaccel-kernels/include pgaccel-kernels/test
} | LC_ALL=C sort -u | while IFS= read -r source_path; do
 shasum -a256 "$source_path"
done > "$build_dir/source.sha256"
shasum -a256 "$task_root/build-production.sh" > "$build_dir/harness.sha256"
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PG_CONFIG="$prefix/bin/pg_config"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
ulimit -n 4096
"$PG_CONFIG" --version > "$build_dir/pg-version.log"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 30m cargo pgrx install --package pg_accel --pg-config "$PG_CONFIG" --no-default-features --features "pg$major" --release > "$build_dir/install.log" 2>&1
shasum -a256 -c "$build_dir/source.sha256" > "$build_dir/source-after.log"
shasum -a256 -c "$build_dir/harness.sha256" > "$build_dir/harness-after.log"
shasum -a256 "$prefix/lib/pg_accel.dylib" "$prefix/bin/postgres" "$PG_CONFIG" > "$build_dir/module.sha256"
printf 'terminal=0\n' > "$build_dir/terminal.log"
