#!/usr/bin/env python3
"""Check idle owner receipts against independently observed shared-ledger bytes.

Pool fields are retained capacity for these bounded integration fixtures, not
a general SQL charge API. A mismatch is an error, never a calibration input.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


MAX_BYTES = (1 << 63) - 1
COMPONENTS = ("raw_bytes", "derived_bytes", "workspace_bytes", "output_bytes")
PHASES = (
    "ready", "trigger_refreshed", "refreshed", "numeric_a", "numeric_b",
    "b_exited", "slot_reused", "all_exited",
)


def integer(value: object, name: str, *, minimum: int = 0) -> int:
    if type(value) is not int or not minimum <= value <= MAX_BYTES:
        raise ValueError(f"{name}: expected integer in [{minimum}, {MAX_BYTES}]")
    return value


def exact_sum(values: list[int]) -> int:
    total = 0
    for value in values:
        total += integer(value, "byte component")
        if total > MAX_BYTES:
            raise ValueError("ledger byte sum exceeds signed bigint")
    return total


def fields(receipt: dict, names: set[str]) -> None:
    if set(receipt) != names:
        raise ValueError(f"unexpected receipt fields: expected {sorted(names)}")


def ownership(baseline: int, owners: list[dict]) -> int:
    """Compute an expectation only from baseline and owner-local components."""
    return exact_sum([baseline] + [owner[key] for owner in owners for key in COMPONENTS])


class LedgerAudit:
    def __init__(self) -> None:
        self.baseline: int | None = None
        self.databases: tuple[str, str] | None = None
        self.index = 0
        self.identities: dict[str, int] = {}
        self.previous: dict[str, dict] = {}
        self.pending: dict[str, dict] = {}
        self.exit_seen = False
        self.equations: list[str] = []

    def consume(self, receipt: dict) -> None:
        if not isinstance(receipt, dict):
            raise ValueError("receipt must be a JSON object")
        kind = receipt.get("kind")
        if self.baseline is None:
            fields(receipt, {"kind", "db_a", "db_b", "live_bytes"})
            if kind != "baseline":
                raise ValueError("first receipt must be the independent baseline")
            databases = (receipt["db_a"], receipt["db_b"])
            if any(not isinstance(db, str) or not db for db in databases):
                raise ValueError("database identities must be nonempty strings")
            if databases[0] == databases[1]:
                raise ValueError("two distinct database identities are required")
            self.databases = databases
            self.baseline = integer(receipt["live_bytes"], "baseline")
            return
        if self.index == len(PHASES):
            raise ValueError("receipt after final backend exit")
        phase = PHASES[self.index]
        if receipt.get("phase") != phase:
            raise ValueError(f"out-of-order receipt: expected phase {phase}")
        assert self.databases is not None
        db_a, db_b = self.databases
        if kind == "owner":
            fields(receipt, {"kind", "phase", "database_name", "backend_pid", *COMPONENTS})
            database = receipt["database_name"]
            allowed = () if phase == "all_exited" else (
                (db_a,) if phase in ("b_exited", "slot_reused") else self.databases
            )
            if database not in allowed or database in self.pending:
                raise ValueError("unexpected or duplicate database/PID/phase owner")
            pid = integer(receipt["backend_pid"], "backend PID", minimum=1)
            if pid > (1 << 31) - 1:
                raise ValueError("backend PID exceeds PostgreSQL integer")
            for key in COMPONENTS:
                integer(receipt[key], key)
            if database in self.identities:
                if self.identities[database] != pid:
                    raise ValueError("backend identity changed during owner lifetime")
            elif pid in self.identities.values():
                raise ValueError("different live owners share a backend PID")
            self.identities[database] = pid
            self.pending[database] = receipt
        elif kind == "exit":
            fields(receipt, {"kind", "phase", "database_name", "backend_pid", "mode", "remaining_backends"})
            if phase not in ("b_exited", "all_exited") or self.exit_seen:
                raise ValueError("unexpected or duplicate backend exit")
            database, mode = (db_b, "fatal") if phase == "b_exited" else (db_a, "normal")
            pid = integer(receipt["backend_pid"], "exited backend PID", minimum=1)
            if (receipt["database_name"], pid, receipt["mode"]) != (
                database, self.identities.get(database), mode
            ):
                raise ValueError("exit does not identify the expected backend and mode")
            if integer(receipt["remaining_backends"], "remaining backends") != 0:
                raise ValueError("exited backend remains in pg_stat_activity")
            self.exit_seen = True
        elif kind == "checkpoint":
            fields(receipt, {"kind", "phase", "observer_pid", "live_bytes"})
            expected = self.expected()
            observer = integer(receipt["observer_pid"], "observer PID", minimum=1)
            if observer > (1 << 31) - 1 or any(
                owner["backend_pid"] == observer for owner in self.pending.values()
            ):
                raise ValueError("observer must be a distinct PostgreSQL backend")
            actual = integer(receipt["live_bytes"], "observer global bytes")
            if actual != expected:
                raise ValueError(f"{phase}: global bytes {actual}, independent owner sum {expected}")
            parts = [str(self.baseline)] + [
                str(ownership(0, [self.pending[db]])) for db in self.databases if db in self.pending
            ]
            self.equations.append(f"{phase}: {' + '.join(parts)} = {actual}")
            self.previous = self.pending
            self.pending = {}
            self.exit_seen = False
            self.index += 1
        else:
            raise ValueError(f"unknown receipt kind: {kind!r}")

    def expected(self) -> int:
        if self.baseline is None or self.databases is None or self.index == len(PHASES):
            raise ValueError("no pending lifecycle checkpoint")
        phase = PHASES[self.index]
        db_a, db_b = self.databases
        required = set() if phase == "all_exited" else (
            {db_a} if phase in ("b_exited", "slot_reused") else set(self.databases)
        )
        if set(self.pending) != required:
            raise ValueError(f"{phase}: missing complete owner snapshots")
        if self.exit_seen != (phase in ("b_exited", "all_exited")):
            raise ValueError(f"{phase}: missing or unexpected exit receipt")
        for database, owner in self.pending.items():
            if exact_sum([owner["raw_bytes"], owner["derived_bytes"]]) == 0:
                raise ValueError("live fixture owner has no resident input bytes")
            needs_pools = (
                database == db_a and phase == "ready"
                or phase in ("numeric_b", "b_exited", "slot_reused")
                or database == db_a and phase == "numeric_a"
            )
            if needs_pools and (owner["workspace_bytes"] == 0 or owner["output_bytes"] == 0):
                raise ValueError("dispatched fixture must retain workspace and output buffers")
            unchanged = (
                database == db_b and phase in ("trigger_refreshed", "refreshed", "numeric_a")
                or database == db_a and phase in ("numeric_b", "b_exited", "slot_reused")
            )
            if unchanged and any(owner[key] != self.previous[database][key] for key in COMPONENTS):
                raise ValueError(f"{phase}: unrelated backend ownership changed")
        return ownership(self.baseline, list(self.pending.values()))

    def finish(self, *, complete: bool = False) -> None:
        if self.baseline is None or self.pending or self.exit_seen:
            raise ValueError("incomplete ledger receipt sequence")
        if complete and self.index != len(PHASES):
            raise ValueError("missing lifecycle checkpoints or final baseline return")


def unique_object(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON field: {key}")
        result[key] = value
    return result


def read_audit(path: Path) -> LedgerAudit:
    audit = LedgerAudit()
    with path.open(encoding="utf-8") as source:
        for number, line in enumerate(source, 1):
            try:
                audit.consume(json.loads(line, object_pairs_hook=unique_object))
            except (ValueError, TypeError) as error:
                raise ValueError(f"receipt line {number}: {error}") from error
    return audit


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("expected", "validate"))
    parser.add_argument("receipts", type=Path)
    parser.add_argument("--complete", action="store_true")
    args = parser.parse_args()
    try:
        audit = read_audit(args.receipts)
        if args.mode == "expected":
            if args.complete:
                raise ValueError("--complete requires validate mode")
            print(audit.expected())
        else:
            audit.finish(complete=args.complete)
            print("\n".join(audit.equations))
    except (ValueError, OSError) as error:
        print(f"ledger accounting: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
