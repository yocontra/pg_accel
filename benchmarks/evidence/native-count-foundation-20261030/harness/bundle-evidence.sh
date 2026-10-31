#!/usr/bin/env bash
set -euo pipefail
repo=/Users/contra/Projects/pg_accel
task_root="$repo/.codex/scratch/native-count-foundation-validation-20261030"
evidence="$repo/benchmarks/evidence/native-count-foundation-20261030"
for run in qualification1 dense-materialization1; do
 test "$(cat "$task_root/$run/terminal.log")" = $'stage=complete\nterminal=0'
 test ! -e "$evidence/$run"
 mkdir -p "$evidence/$run"
 for file in "$task_root/$run/"*.log "$task_root/$run/"*.sha256; do
  test -f "$file" && cp "$file" "$evidence/$run/"
 done
 for major in 18 19; do
  test ! -e "$task_root/$run/pg$major/pgdata/postmaster.pid"
  mkdir "$evidence/$run/pg$major"
  for file in "$task_root/$run/pg$major/"*.log "$task_root/$run/pg$major/"*.sha256; do
   test -f "$file" && cp "$file" "$evidence/$run/pg$major/"
  done
 done
done
mkdir "$evidence/harness"
for file in qualify.sh dense-smoke.sql dense-materialization.sh dense-materialization.sql bundle-evidence.sh; do
 cp "$task_root/$file" "$evidence/harness/"
done
