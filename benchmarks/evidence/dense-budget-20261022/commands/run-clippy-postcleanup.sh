#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/hosted-budget-20261018"
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PG_CONFIG="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4/bin/pg_config"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
shasum -a 256 -c "$task_root/task1-postlint-final9.sha256" > "$task_root/postcleanup-clippy-source-before.log"
set +e
cargo clippy --workspace --no-default-features --features 'pg18 pg_test' --all-targets -- -D warnings > "$task_root/postcleanup-clippy-pg18-pgtest.log" 2>&1
result=$?
set -e
shasum -a 256 -c "$task_root/task1-postlint-final9.sha256" > "$task_root/postcleanup-clippy-source-after.log"
printf 'Postcleanup workspace PG18 pg_test all-target Clippy terminal=%s\n' "$result"
exit "$result"
