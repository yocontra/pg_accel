#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task="$PWD/.codex/scratch/hash-count-where-rust-20261030"
run="$task/qualification3"
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
shasum -a256 "$0" "$task/run-live3.sh" "$task/check-live2.py" "$task/raw.expected" "$task/h3.expected" "$task/planner.expected" "$task/pgrx/config.toml" "$task/pgtests18-raw-qualification2/live.log" "$task/pgtests18-raw-qualification2/terminal.log" "$task/pgtests18-raw-qualification2/pg-test-module.sha256" > "$run/harness.sha256"
shasum -a256 "$task/pgtests18-raw-qualification2/harness.sha256" "$task/pgtests18-raw-qualification2/protected.sha256" >> "$run/harness.sha256"
shasum -a256 -c "$task/cost-validate3/source.sha256" > "$run/source-before.log"
# Retain the original terminal=1 checker failure. Revalidate, never rerun, its
# successful nine-test lib suite with the reviewed runner-section checker.
shasum -a256 -c "$task/pgtests18-raw-qualification2/pg-test-module.sha256" > "$run/retained-raw-module-before.log"
shasum -a256 -c "$task/pgtests18-raw-qualification2/source.sha256" > "$run/retained-raw-source-before.log"
shasum -a256 -c "$task/pgtests18-raw-qualification2/harness.sha256" > "$run/retained-raw-harness-before.log"
shasum -a256 -c "$task/pgtests18-raw-qualification2/protected.sha256" > "$run/retained-raw-protected-before.log"
/usr/bin/python3 -I "$task/check-live2.py" "$task/pgtests18-raw-qualification2/live.log" "$task/raw.expected" > "$run/retained-raw9-check.log"
for major in 18 19; do
 for family in raw h3 planner; do
  if test "$major-$family" = 18-raw; then continue; fi
  stage="pg$major-$family"
  shasum -a256 -c "$run/harness.sha256" > "$run/$stage-harness-before.log"
  case "$family" in
   raw) filter=raw_count_tests;;
   h3) filter=h3_count_tests;;
   planner) filter=generic_groupagg::tests::pg_pg_raw_count_;;
  esac
  bash "$task/run-live3.sh" "$major" "$family-qualification3" "$filter"
 done
done
stage=complete
