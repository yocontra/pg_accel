#!/usr/bin/env bash
set -euo pipefail
repo=/Users/contra/Projects/pg_accel
task="$repo/.codex/scratch/dense-planning-diagnostic-20261030"
prior="$repo/.codex/scratch/raw-integer-count-20261026/build18-dense-cost-final3"
test "$(cat "$prior/final-terminal.log")" = terminal=0
(cd "$task/final3-source" && shasum -a256 -c "$prior/source.sha256")
shasum -a256 -c "$prior/bench.sha256"
for file in Cargo.toml Cargo.lock pg_accel_bench/Cargo.toml; do
 cmp "$repo/$file" "$task/final3-source/$file"
done
diff -qr "$repo/pg_accel_bench/src" "$task/final3-source/pg_accel_bench/src"
printf 'retained_benchmark_source_and_binary=PASS (not rebuilt)\n'
