#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task="$PWD/.codex/scratch/hash-count-where-rust-20261030"
run="$task/qualification2"
test ! -e "$run"
mkdir "$run"
stage=preflight
finish() {
 result=$?
 trap - EXIT
 set +e
 shasum -a256 -c "$task/cost-validate3/source.sha256" > "$run/source-after.log" || result=1
 shasum -a256 -c "$run/harness.sha256" > "$run/harness-after.log" || result=1
 printf 'stage=%s\nterminal=%s\n' "$stage" "$result" > "$run/terminal.log"
 exit "$result"
}
trap finish EXIT
shasum -a256 "$0" "$task/run-live2.sh" "$task/check-live.py" "$task/raw.expected" "$task/h3.expected" "$task/planner.expected" "$task/pgrx/config.toml" > "$run/harness.sha256"
shasum -a256 -c "$task/cost-validate3/source.sha256" > "$run/source-before.log"
for major in 18 19; do
 for family in raw h3 planner; do
  stage="pg$major-$family"
  shasum -a256 -c "$run/harness.sha256" > "$run/$stage-harness-before.log"
  case "$family" in
   raw) filter=raw_count_tests;;
   h3) filter=h3_count_tests;;
   planner) filter=generic_groupagg::tests::pg_pg_raw_count_;;
  esac
  bash "$task/run-live2.sh" "$major" "$family-qualification2" "$filter"
 done
done
stage=complete
