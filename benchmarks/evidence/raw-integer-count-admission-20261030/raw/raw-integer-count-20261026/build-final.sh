#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/raw-integer-count-20261026"
major=${1:?major required}
stage=${2:?fresh stage required}
case "$major" in 18|19) ;; *) exit 2;; esac
case "$stage" in ''|*[!a-zA-Z0-9_-]*) exit 2;; esac
bash "$task_root/build-production.sh" "$major" "$stage"
build_dir="$task_root/build$major-$stage"
test "$(<"$build_dir/terminal.log")" = terminal=0
ulimit -n 4096
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 15m cargo build --locked --release -p pg_accel_bench > "$build_dir/bench-build.log" 2>&1
shasum -a256 -c "$build_dir/source.sha256" > "$build_dir/final-source-after.log"
shasum -a256 -c "$build_dir/module.sha256" > "$build_dir/final-module-after.log"
shasum -a256 "$task_root/build-final.sh" "$task_root/build-production.sh" > "$build_dir/final-harness.sha256"
shasum -a256 target/release/pg_accel_bench > "$build_dir/bench.sha256"
printf 'terminal=0\n' > "$build_dir/final-terminal.log"
