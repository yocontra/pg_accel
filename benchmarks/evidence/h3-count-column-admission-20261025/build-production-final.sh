#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/h3-count-column-20261022"
major=${1:?major required}
case "$major" in 18) version=18.4;; 19) version=19beta1;; *) exit 2;; esac
build_dir="$task_root/production-final-build$major"
test ! -e "$build_dir"
mkdir -p "$build_dir"
while read -r old_hash source_path; do
  shasum -a256 "$source_path"
done < "$task_root/production-build18/source.sha256" > "$build_dir/source.sha256"
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PG_CONFIG="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version/bin/pg_config"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
ulimit -n 4096
cargo pgrx install --package pg_accel --pg-config "$PG_CONFIG" --no-default-features --features "pg$major" --release > "$build_dir/install.log" 2>&1
shasum -a256 -c "$build_dir/source.sha256" > "$build_dir/source-after.log"
shasum -a256 "$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version/lib/pg_accel.dylib" > "$build_dir/module.sha256"
