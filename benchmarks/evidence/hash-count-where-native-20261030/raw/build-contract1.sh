#!/usr/bin/env bash
set -euo pipefail
repo=/Users/contra/Projects/pg_accel
run="$repo/.codex/scratch/hash-count-where-native-20261030/build-contract1"
timeout=/opt/homebrew/bin/gtimeout
test ! -e "$run"
mkdir "$run"
stage=preflight
files=(pgaccel-kernels/include/pgaccel_olap.h pgaccel-kernels/src/grouped_agg.cpp pgaccel-kernels/test/test_grouped_agg_hash.cpp scripts/cpu_cheat_abi_manifest.txt scripts/cpu_cheat_audit.py scripts/tests/test_cpu_cheat_audit.py)
cd "$repo"
shasum -a256 "${files[@]}" > "$run/source.sha256"
shasum -a256 "$0" > "$run/harness.sha256"
finish() {
  result=$?
  trap - EXIT
  set +e
  shasum -a256 -c "$run/source.sha256" > "$run/source-after.log" || result=1
  shasum -a256 -c "$run/harness.sha256" > "$run/harness-after.log" || result=1
  printf 'stage=%s\nterminal=%s\n' "$stage" "$result" > "$run/terminal.log"
  exit "$result"
}
trap finish EXIT
df -h "$repo" > "$run/disk.log"
export ACPP_TARGETS=generic
export ACPP_PREFIX="$repo/.pgaccel/acpp/current"
export CMAKE_BUILD_PARALLEL_LEVEL=4
stage=configure
"$timeout" --signal=TERM --kill-after=10s 3m cmake -S "$repo/pgaccel-kernels" -B "$run/native" -DAdaptiveCpp_DIR="$ACPP_PREFIX/lib/cmake/AdaptiveCpp" -DACPP_TARGETS=generic -DCMAKE_BUILD_TYPE=Release > "$run/configure.log" 2>&1
stage=build
"$timeout" --signal=TERM --kill-after=30s 20m cmake --build "$run/native" --target test_grouped_agg_hash --parallel 4 > "$run/build.log" 2>&1
shasum -a256 "$run/native/test_grouped_agg_hash" "$run/native/libpgaccel_kernels_shared.dylib" > "$run/binaries.sha256"
stage=contract
"$timeout" --signal=TERM --kill-after=10s 3m "$run/native/test_grouped_agg_hash" --contract-only > "$run/contract.log" 2>&1
stage=complete
