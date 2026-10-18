#!/usr/bin/env bash
set -euo pipefail
root=/Users/contra/Projects/pg_accel
owned="$root/.codex/scratch/projection-replay-20261017"
cd "$root"
export PATH="$owned/no-git:$PATH"
shasum -a 256 patches/adaptivecpp/sscp-replay-controls.patch scripts/tests/fixtures/acpp_hcf_replay.cpp scripts/tests/test_acpp_hcf_replay.py > "$owned/logs/final-inputs.sha256"
cmake --build "$owned/build-make" --target acpp-clang --clean-first --parallel 4
cp "$owned/build-make/src/compiler/libacpp-clang.so" "$owned/prefix/lib/libacpp-clang.so"
export SDKROOT="$(xcrun --show-sdk-path)"
export ACPP_HCF_REPLAY_PREFIX="$owned/prefix"
export ACPP_HCF_REPLAY_ORIGINAL_PREFIX="$root/.pgaccel/acpp/metal"
export ACPP_HCF_REPLAY_LLVM=/opt/homebrew/opt/llvm@20
export ACPP_HCF_REPLAY_ARTIFACTS="$owned/final-tests"
python3 -m unittest scripts.tests.test_acpp_hcf_replay -v > "$owned/final-tests.log" 2>&1
shasum -a 256 -c "$owned/logs/final-inputs.sha256" > "$owned/logs/final-inputs.after-check.txt"
shasum -a 256 -c "$owned/logs/installed-prefix.before.sha256" > "$owned/logs/installed-prefix.final-check.txt"
shasum -a 256 -c "$owned/logs/installed-source.before.sha256" > "$owned/logs/installed-source.final-check.txt"
test "$(readlink .pgaccel/acpp/current)" = "$(cat "$owned/logs/current.before.txt")"
ruff check scripts/tests/test_acpp_hcf_replay.py > "$owned/logs/final-ruff.log"
