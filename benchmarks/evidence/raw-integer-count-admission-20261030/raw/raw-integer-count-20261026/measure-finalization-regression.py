#!/usr/bin/env python3
"""Two-cell historical-before/current-after diagnostic; not a paired benchmark."""
import importlib.util
import json
from pathlib import Path
import re
import statistics

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("sealed_validation", ROOT / "acquire-dense-count-validation.py")
A = importlib.util.module_from_spec(spec)
spec.loader.exec_module(A)
V = A.V
RUN = ROOT / "finalization-regression1"
BUILD = ROOT / "build18-hash-finalization1"


def main():
    V.need(RUN.is_dir() and not (RUN / "results.json").exists(), "fresh owned diagnostic required")
    m = V.manifest(ROOT / "dense-count-training3/manifest.json", ROOT)
    result = {"kind": "historical_before_after_diagnostic_not_contemporaneous_or_headline",
              "new_module_seal": A.reference(BUILD / "module.sha256"),
              "new_source_seal": A.reference(BUILD / "source.sha256"), "cells": []}
    prefix = ROOT.parent / "uniform-budget-live-20261012/postgres/18.4"
    for n in (1000017, 2000017):
        cell_id = f"n{n}-g4096"
        fixture = m["_fixtures"][cell_id]
        backend = A.Backend(prefix / "bin/psql",
            "host=" + str(ROOT.parent / "countsort18") + " port=33921 dbname=postgres connect_timeout=10",
            RUN / (cell_id + ".log"))
        try:
            A.initialize(backend, fixture, m)
            parity = A.parity(backend, fixture["query_sql"], 4096)
            pid, samples = A.samples(backend, fixture["query_sql"], m, 1, False, RUN, cell_id, "after")
            old_path = ROOT / "dense-count-validation1" / (cell_id + "-current.json")
            old = V.read(old_path)
            V.need(old["fixture"] == fixture, "historical fixture changed")
            for sample in old["warmups"] + old["samples"] + samples:
                plan = sample["actual_plan"]
                V.need(re.findall(r"GPU Dispatched Physical Kernel Mode: (.*)", plan) == ["parallel_hash"], "raw mode changed")
                V.need("GPU Physical Kernel Mode Verified: true" in plan, "unverified raw mode")
                V.need(sample["native_after"]["gpu"] > sample["native_before"]["gpu"] and
                       sample["native_after"]["stock"] == sample["native_before"]["stock"], "dispatch/fallback mismatch")
            # Preserve full plans and intervals; diagnostic wall includes EXPLAIN.
            result["cells"].append({"id": cell_id, "fixture": fixture, "backend_pid": pid,
                "parity": parity, "warmups": samples[:5], "samples": samples[5:],
                "historical_receipt": A.reference(old_path),
                "historical_module_seal": A.reference(ROOT / "build18-workfacts/module.sha256"),
                "historical_source_seal": A.reference(ROOT / "build18-workfacts/source.sha256"),
                "old_diagnostic_wall_median_ns": statistics.median(s["diagnostic_wall_ns"] for s in old["samples"]),
                "new_diagnostic_wall_median_ns": statistics.median(s["diagnostic_wall_ns"] for s in samples[5:])})
        finally:
            backend.close()
    A.write(RUN / "results.json", result)
    print("TWO_CELL_RAW_FINALIZATION_DIAGNOSTIC_COMPLETE")


if __name__ == "__main__":
    main()
