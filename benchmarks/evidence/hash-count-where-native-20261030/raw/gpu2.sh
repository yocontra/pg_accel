#!/usr/bin/env bash
set -euo pipefail
repo=/Users/contra/Projects/pg_accel
task="$repo/.codex/scratch/hash-count-where-native-20261030"
build="$task/build-contract4"
run="$task/gpu2"
timeout=/opt/homebrew/bin/gtimeout
test ! -e "$run"
test "$(tail -1 "$build/terminal.log")" = terminal=0
cd "$repo"
shasum -a256 -c "$build/source.sha256" > /dev/null
mkdir "$run"
cp "$build/source.sha256" "$run/source.sha256"
shasum -a256 "$0" > "$run/harness.sha256"
shasum -a256 "$build/native/test_grouped_agg_hash" "$build/native/test_h3" "$build/native/libpgaccel_kernels_shared.dylib" > "$run/binaries.sha256"
stage=preflight
finish() {
  result=$?
  trap - EXIT
  set +e
  shasum -a256 -c "$run/source.sha256" > "$run/source-after.log" || result=1
  shasum -a256 -c "$run/binaries.sha256" > "$run/binaries-after.log" || result=1
  shasum -a256 -c "$run/harness.sha256" > "$run/harness-after.log" || result=1
  printf 'stage=%s\nterminal=%s\n' "$stage" "$result" > "$run/terminal.log"
  exit "$result"
}
trap finish EXIT
df -h "$repo" > "$run/disk.log"
vm_stat > "$run/memory.log"
export ACPP_APPDB_DIR="$run/jit-cache"
stage=hash
"$timeout" --signal=TERM --kill-after=30s 20m "$build/native/test_grouped_agg_hash" > "$run/hash.log" 2>&1
stage=h3
"$timeout" --signal=TERM --kill-after=30s 10m "$build/native/test_h3" > "$run/h3.log" 2>&1
stage=complete
