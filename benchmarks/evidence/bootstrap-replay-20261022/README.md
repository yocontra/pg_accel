# Replay-patch bootstrap qualification

The production setup dispatcher now applies the committed SSCP replay patch
immediately after coverage. Both replay targets must match complete pinned,
coverage-only or combined file bytes before mutation; partial and drifted states
reject. Repeated application retains the other coverage-target checks. The source
pin, patch-digest cache identity and analyzer acceptance are unchanged.

Nine integration tests pass with no skips, using the actual patch functions and
dispatcher on temporary local pinned-source copies. They cover fresh/repeated
application, coverage-only upgrade, each partial target/hunk, in/out-of-hunk drift,
missing patch/target/pinned object and preservation of another coverage check.
Bash syntax and Ruff pass. Two independent reviews approved; a second reviewer
also reran all nine tests and the syntax check successfully.

The original dispatcher executes successfully but leaves exact coverage-only
bytes, failing the combined-byte oracle (`old-dispatcher-negative.log`, exit 1).
The retained script and negative runner reproduce that control. Missing local
fixtures also fail rather than skip (`missing-fixture-negative.log`, exit 1).
An initial implementation's coverage-only upgrade failed on Bash 3 empty-array
nounset expansion; its raw failure and minimal correction are retained. The
exact-byte oracle was not weakened.

## Reproduction and provenance

Run from the repository root with a local AdaptiveCpp repository containing pin
`456ae6910720810f5fe59f160e6707d46bb8e5f0`:

```sh
ACPP_SETUP_TEST_SOURCE=/path/to/local/AdaptiveCpp PYTHONDONTWRITEBYTECODE=1 \
  python3 -m unittest scripts/tests/test_setup_acpp_replay.py -v
bash -n scripts/setup_acpp.sh
ruff check scripts/tests/test_setup_acpp_replay.py
ACPP_SETUP_TEST_SOURCE=/path/to/local/AdaptiveCpp PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=. \
  python3 benchmarks/evidence/bootstrap-replay-20261022/old-dispatcher-negative.py
ACPP_SETUP_TEST_SOURCE=/nonexistent/pgaccel-bootstrap-fixture \
  PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts/tests/test_setup_acpp_replay.py -v
```

The last two commands must exit 1. Local execution used Python 3.15.0; sanitized
fixture subprocesses resolve `/bin/bash` 3.2.57 through `os.defpath`. The tests
read pinned Git blobs, use local shared clones under temporary owned directories,
and never invoke top-level setup. No network, compiler, GPU, install or current
toolchain symlink operation occurs. `fixture-blobs.sha256` records the ten pinned
input blobs; `inputs.sha256` records the unchanged pin and five committed patches.
`source.sha256` binds the two implementation files; the commit carries their full
contents. Verify these two manifests from the repository root, and `SHA256SUMS`
from this evidence directory.

Scope is **local generic-backend source patch composition only**. This is not an
installed-toolchain build, Metal/CUDA qualification, device proof, CI pass, or
closure of any release-audit finding. The overarching projection plan remains
open. Full source checkouts, caches, objects and compiler binaries are excluded.
