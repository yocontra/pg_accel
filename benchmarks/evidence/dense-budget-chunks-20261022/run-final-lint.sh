#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/dense-budget-chunks-20261022"
lint_stage=${1:?lint stage required}
manifest="$task_root/integration-${lint_stage}/source.sha256"
test ! -e "$task_root/$lint_stage-clippy.log"
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PG_CONFIG="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4/bin/pg_config"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
shasum -a 256 -c "$manifest" > "$task_root/$lint_stage-lint-source-before.log"
source_paths=()
while read -r digest source_path; do source_paths+=("$source_path"); done < "$manifest"
rustfmt --edition 2024 --check "${source_paths[@]}" > "$task_root/$lint_stage-rustfmt.log" 2>&1
set +e
cargo clippy --package pg_accel --all-targets --no-default-features --features pg18,pg_test -- -D warnings > "$task_root/$lint_stage-clippy.log" 2>&1
lint_result=$?
set -e
shasum -a 256 -c "$manifest" > "$task_root/$lint_stage-lint-source-after.log"
printf '%s rustfmt=0 feature-enabled all-target Clippy=%s\n' "$lint_stage" "$lint_result"
exit "$lint_result"
