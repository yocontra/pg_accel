# ABI inventory focused test receipt

Evidence kind: **transcribed tool output**, not an originally captured logfile.
The four tests were run once; this receipt was assembled afterward from the
retained tool responses. No test was rerun to create this receipt.

Transcription recorded: 2026-10-31 01:24:39 UTC. This is the receipt creation
time, **not** the test execution timestamp, which was not captured here.

Working directory: `/Users/contra/Projects/pg_accel`.

Exact test command:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts.tests.test_cpu_cheat_audit.AbiInventoryTests.test_checked_in_manifest_has_literal_integrity_anchor scripts.tests.test_cpu_cheat_audit.AbiInventoryTests.test_mutated_immutable_manifest_is_rejected scripts.tests.test_cpu_cheat_audit.ProductionWitnessTests.test_complete_real_abi_baseline_and_violation_floor scripts.tests.test_cpu_cheat_audit.ProductionWitnessTests.test_count_work_inventory_does_not_grant_metadata_proof -v
```

Result: **4 tests passed**, unittest-reported elapsed time **438.000 seconds**,
process terminal exit code **0**. Original execution session: `55813`.
The initial tool chunk `f1ae7d` contained the first two passing test lines;
final tool chunk `f9c833` contained the remaining two passing lines, unittest
summary, and process exit code 0. Intermediate polls contained no test output.
The companion `audit-inventory-tests.transcribed.txt` concatenates those test
output portions without claiming to be a raw redirected stream.

Frozen source identities used for the reviewed change and verified unchanged
again when this receipt was created:

```text
7c728a782e72942068b82fc85a9a43e85cd481d55635a2e084af8b5f89215895  scripts/cpu_cheat_audit.py
3536d6c3d3a467ee868a178ac1101a4addcba3c68080620382a4f4b7720c76d8  scripts/tests/test_cpu_cheat_audit.py
e8383a0ac16ebbb2756e9275f55151abcbba5a97f744db551ce70ed241044105  scripts/cpu_cheat_abi_manifest.txt
```

Scope: the checked-in 134-signature inventory and its literal integrity anchor,
manifest tamper rejection, actual source/header/compiler inventory agreement,
the existing 82-hostile-production-name rejection floor, and an explicit
regression that `pgaccel_grouped_agg_count_work_requirements_v1` is inventoried
with its exact signature but remains unproved with an audit finding and is not
a lifecycle exemption. This is **not** a passing overall CPU-cheat release audit,
dynamic module export attestation, or GPU runtime test.
