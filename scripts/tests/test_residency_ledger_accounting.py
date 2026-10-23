"""Executable arithmetic/protocol tests; synthetic receipts are not GPU proof."""

from __future__ import annotations

import copy
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from scripts import residency_ledger_accounting as accounting


ROOT = Path(__file__).resolve().parents[2]
VALIDATOR = ROOT / "scripts/residency_ledger_accounting.py"
RUNNER = ROOT / "scripts/residency_ledger_integration.sh"


def owner(database: str, phase: str, values: tuple[int, int, int, int]) -> dict:
    return {
        "kind": "owner", "phase": phase, "database_name": database,
        "backend_pid": 101 if database == "a" else 202,
        **dict(zip(accounting.COMPONENTS, values, strict=True)),
    }


def receipts() -> list[dict]:
    result = [{"kind": "baseline", "db_a": "a", "db_b": "b", "live_bytes": 7}]
    # Observations are explicit fixture values, not computed by the validator.
    phases = (
        ("ready", (100, 20, 10, 5), (80, 0, 0, 0), 222),
        ("trigger_refreshed", (110, 20, 10, 5), (80, 0, 0, 0), 232),
        ("refreshed", (110, 0, 10, 5), (80, 0, 0, 0), 212),
        # Numeric allocation can replace old pools with smaller total capacity.
        ("numeric_a", (150, 30, 8, 4), (80, 0, 0, 0), 279),
        ("numeric_b", (150, 30, 8, 4), (130, 20, 6, 3), 358),
        ("b_exited", (150, 30, 8, 4), None, 199),
        ("slot_reused", (150, 30, 8, 4), None, 199),
        ("all_exited", None, None, 7),
    )
    for phase, a_values, b_values, observed in phases:
        if phase in ("b_exited", "all_exited"):
            database, pid, mode = ("b", 202, "fatal") if phase == "b_exited" else ("a", 101, "normal")
            result.append({
                "kind": "exit", "phase": phase, "database_name": database,
                "backend_pid": pid, "mode": mode, "remaining_backends": 0,
            })
        for database, values in (("a", a_values), ("b", b_values)):
            if values is not None:
                result.append(owner(database, phase, values))
        result.append({"kind": "checkpoint", "phase": phase, "observer_pid": 303, "live_bytes": observed})
    return result


def audit(records: list[dict], *, complete: bool = True) -> accounting.LedgerAudit:
    result = accounting.LedgerAudit()
    for record in records:
        result.consume(record)
    result.finish(complete=complete)
    return result


def find(records: list[dict], kind: str, phase: str, database: str | None = None) -> dict:
    return next(record for record in records if record["kind"] == kind
                and record.get("phase") == phase
                and (database is None or record.get("database_name") == database))


class LedgerAccountingTests(unittest.TestCase):
    def test_exact_lifecycle_includes_shrink_reuse_two_owners_and_zero_owners(self) -> None:
        result = audit(receipts())
        self.assertEqual(result.index, len(accounting.PHASES))
        self.assertEqual(result.equations[0], "ready: 7 + 135 + 80 = 222")
        self.assertEqual(result.equations[-1], "all_exited: 7 = 7")
        self.assertEqual(accounting.ownership(7, []), 7)

    def test_expected_is_computed_before_observer_receipt_exists(self) -> None:
        result = accounting.LedgerAudit()
        for record in receipts()[:3]:
            result.consume(record)
        self.assertEqual(result.expected(), 222)

    def test_one_byte_mismatch_and_omitted_integer_charge_fail(self) -> None:
        for observed in (221, 223, 207):  # 207 is the obsolete relation-only sum.
            with self.subTest(observed=observed):
                records = receipts()
                find(records, "checkpoint", "ready")["live_bytes"] = observed
                with self.assertRaisesRegex(ValueError, "independent owner sum 222"):
                    audit(records)
        records = receipts()
        integer_owner = find(records, "owner", "ready", "a")
        integer_owner.update(workspace_bytes=0, output_bytes=0)
        with self.assertRaisesRegex(ValueError, "retain workspace and output"):
            audit(records)

    def test_numeric_double_counting_and_wrong_exit_release_fail(self) -> None:
        for phase, observed in (("numeric_a", 404), ("numeric_b", 550),
                                ("b_exited", 358), ("b_exited", 7),
                                ("slot_reused", 200), ("all_exited", 8)):
            with self.subTest(phase=phase, observed=observed):
                records = receipts()
                find(records, "checkpoint", phase)["live_bytes"] = observed
                with self.assertRaisesRegex(ValueError, "independent owner sum"):
                    audit(records)

    def test_unrelated_owner_cannot_change_even_if_global_still_matches(self) -> None:
        for phase, database in (("trigger_refreshed", "b"), ("refreshed", "b"),
                                ("numeric_a", "b"), ("numeric_b", "a"),
                                ("b_exited", "a"), ("slot_reused", "a")):
            with self.subTest(phase=phase):
                records = receipts()
                find(records, "owner", phase, database)["raw_bytes"] += 1
                find(records, "checkpoint", phase)["live_bytes"] += 1
                with self.assertRaisesRegex(ValueError, "unrelated backend"):
                    audit(records)

    def test_numbers_are_exact_beyond_float_precision(self) -> None:
        records = receipts()
        offset = 1 << 53
        for record in records:
            if "live_bytes" in record:
                record["live_bytes"] += offset
        self.assertEqual(audit(records).baseline, offset + 7)

    def test_observer_must_be_independent_of_both_live_owners(self) -> None:
        for pid in (0, 101, 202, 1 << 31):
            with self.subTest(pid=pid):
                records = receipts()
                find(records, "checkpoint", "ready")["observer_pid"] = pid
                with self.assertRaises(ValueError):
                    audit(records)

    def test_invalid_integer_types_missing_fields_and_overflow_fail(self) -> None:
        for value in (-1, True, 1.0, "10", None, accounting.MAX_BYTES + 1):
            with self.subTest(value=value):
                records = receipts()
                records[1]["raw_bytes"] = value
                with self.assertRaises(ValueError):
                    audit(records)
        records = receipts()
        del records[1]["output_bytes"]
        with self.assertRaisesRegex(ValueError, "receipt fields"):
            audit(records)
        records = receipts()
        records[0]["live_bytes"] = accounting.MAX_BYTES
        with self.assertRaisesRegex(ValueError, "sum exceeds"):
            audit(records)
        with self.assertRaisesRegex(ValueError, "sum exceeds"):
            accounting.exact_sum([accounting.MAX_BYTES, 1])

    def test_duplicate_owner_changed_pid_and_wrong_database_fail(self) -> None:
        records = receipts()
        records.insert(2, copy.deepcopy(records[1]))
        with self.assertRaisesRegex(ValueError, "duplicate"):
            audit(records)
        for database, pid in (("a", 303), ("b", 303)):
            records = receipts()
            find(records, "owner", "trigger_refreshed", database)["backend_pid"] = pid
            with self.assertRaisesRegex(ValueError, "identity changed"):
                audit(records)
        records = receipts()
        records[2]["backend_pid"] = 101
        with self.assertRaisesRegex(ValueError, "share a backend PID"):
            audit(records)
        records = receipts()
        records[1]["database_name"] = "unknown"
        with self.assertRaisesRegex(ValueError, "unexpected"):
            audit(records)

    def test_missing_out_of_order_and_extra_lifecycle_records_fail(self) -> None:
        records = receipts()
        del records[2]
        with self.assertRaisesRegex(ValueError, "missing complete owner"):
            audit(records)
        records = receipts()
        records[1]["phase"] = "numeric_a"
        with self.assertRaisesRegex(ValueError, "out-of-order"):
            audit(records)
        with self.assertRaisesRegex(ValueError, "missing lifecycle"):
            audit(receipts()[:4])
        records = receipts()
        records.append(copy.deepcopy(records[-1]))
        with self.assertRaisesRegex(ValueError, "after final"):
            audit(records)
        records = receipts()
        records[1]["unexpected"] = 1
        with self.assertRaisesRegex(ValueError, "receipt fields"):
            audit(records)

    def test_exit_requires_exact_identity_mode_absence_and_single_receipt(self) -> None:
        for key, value in (("database_name", "a"), ("backend_pid", 303),
                           ("mode", "normal"), ("remaining_backends", 1)):
            with self.subTest(key=key):
                records = receipts()
                find(records, "exit", "b_exited")[key] = value
                with self.assertRaises(ValueError):
                    audit(records)
        records = receipts()
        exit_record = find(records, "exit", "b_exited")
        records.insert(records.index(exit_record), copy.deepcopy(exit_record))
        with self.assertRaisesRegex(ValueError, "duplicate backend exit"):
            audit(records)
        records = [record for record in receipts() if record["kind"] != "exit"]
        with self.assertRaisesRegex(ValueError, "missing or unexpected exit"):
            audit(records)

    def test_cli_rejects_duplicate_json_fields_and_incomplete_final_receipts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "receipts.jsonl"
            payloads = (
                "\n".join(json.dumps(record) for record in receipts()),
                "\n".join(json.dumps(record) for record in receipts()[:4]),
                '{"kind":"baseline","db_a":"a","db_b":"b","live_bytes":7,"live_bytes":7}',
                "",
            )
            for index, payload in enumerate(payloads):
                with self.subTest(index=index):
                    path.write_text(payload, encoding="utf-8")
                    result = subprocess.run(
                        [sys.executable, str(VALIDATOR), "validate", str(path), "--complete"],
                        cwd=ROOT, text=True, capture_output=True, check=False,
                    )
                    self.assertEqual(result.returncode == 0, index == 0, result.stderr)
                    if index != 0:
                        self.assertEqual(result.stdout, "")


class OwnedClientShutdownTests(unittest.TestCase):
    """Execute the real shell helper on owned children, without a SQL mock."""

    def run_helper(self, body: str) -> subprocess.CompletedProcess[str]:
        source = RUNNER.read_text(encoding="utf-8")
        start = source.index("wait_client() {\n")
        end = source.index("\n}\n", start) + 3
        with tempfile.TemporaryDirectory() as directory:
            return subprocess.run(
                ["bash", "-c", "set -euo pipefail\n" + source[start:end] + "\n" + body,
                 "owned-client-test", sys.executable, directory],
                cwd=ROOT, text=True, capture_output=True, check=False, timeout=10,
            )

    def test_graceful_and_nonzero_child_exit_clear_only_owned_pid(self) -> None:
        for status in (0, 7):
            with self.subTest(status=status):
                result = self.run_helper(f'''
client_b="untouched"
"$1" -c 'raise SystemExit({status})' &
client_a=$!
if wait_client client_a; then result=0; else result=$?; fi
test "$result" -eq {status}
test -z "$client_a"
test "$client_b" = untouched
''')
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_timeout_kills_reaps_and_clears_an_owned_term_ignoring_child(self) -> None:
        result = self.run_helper('''
"$1" -c 'import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); print("ready", flush=True); time.sleep(30)' >"$2/ready" &
client_a=$!
owned_pid=$client_a
trap 'kill -KILL "$owned_pid" 2>/dev/null || true; wait "$owned_pid" 2>/dev/null || true' EXIT
deadline=$((SECONDS + 5))
while [ ! -s "$2/ready" ]; do
    test "$SECONDS" -lt "$deadline"
    sleep 0.01
done
if wait_client client_a 0 0; then result=0; else result=$?; fi
test "$result" -eq 124
test -z "$client_a"
! kill -0 "$owned_pid" 2>/dev/null
trap - EXIT
''')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("did not exit; terminating", result.stderr)

    def test_unowned_variable_is_rejected_and_empty_owned_slot_is_safe(self) -> None:
        result = self.run_helper('''
foreign_pid=$$
if wait_client foreign_pid; then exit 9; fi
test "$foreign_pid" -eq $$
client_a=""
wait_client client_a
test -z "$client_a"
''')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("unowned client variable", result.stderr)


if __name__ == "__main__":
    unittest.main()
