#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task="$PWD/.codex/scratch/hash-count-where-rust-20261030"
run="$task/where-population-qualification1"
test ! -e "$run"
mkdir "$run"
stage=preflight
finish() {
 primary=$?; trap - EXIT; set +e
 secondary=0
 shasum -a256 -c "$task/where-validate4/source.sha256" > "$run/source-after.log" || secondary=1
 shasum -a256 -c "$run/harness.sha256" > "$run/harness-after.log" || secondary=1
 result=$primary; if test "$result" = 0; then result=$secondary; fi
 printf 'stage=%s\nprimary=%s\nsecondary=%s\nterminal=%s\n' "$stage" "$primary" "$secondary" "$result" > "$run/terminal.log"
 exit "$result"
}
trap finish EXIT
shasum -a256 "$0" "$task/run-where-population-live1.sh" "$task/check-live2.py" "$task/raw.expected" "$task/h3-22.expected" "$task/pgrx/config.toml" "$task/where-validate4/terminal.log" "$task/where-validate4/source.sha256" "$task/where-validate4/harness.sha256" "$task/where-validate4/pg18.log" "$task/where-validate4/pg19.log" > "$run/harness.sha256"
rg -qx 'stage=complete' "$task/where-validate4/terminal.log"
rg -qx 'terminal=0' "$task/where-validate4/terminal.log"
shasum -a256 -c "$task/where-validate4/source.sha256" > "$run/source-before.log"
shasum -a256 -c "$task/where-validate4/harness.sha256" > "$run/validation-harness-before.log"
for major in 18 19; do
 for family in raw h3; do
  stage="pg$major-$family"
  shasum -a256 -c "$run/harness.sha256" > "$run/$stage-harness-before.log"
  case "$family" in
   raw) filter=raw_count_tests;;
   h3) filter=h3_count_tests;;
  esac
  bash "$task/run-where-population-live1.sh" "$major" "$family-where-population1" "$filter"
 done
done
stage=complete
