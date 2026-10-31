#!/usr/bin/env python3
"""Untimed, non-calibration raw-drain adapter; never fits or starts processes."""
import argparse
import importlib.util
import json
from pathlib import Path


def load_validator(path):
    spec = importlib.util.spec_from_file_location("dense_count_calibration", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def canonical(raw, validator):
    result = []
    for call in raw["calls"]:
        layout = {key: call[key] for key in (
            "workspace_bytes", "workspace_alignment", "workspace_space",
            "scratch_bytes", "scratch_alignment", "scratch_space")}
        result.append({
            "index": call["call_index"], "flags": call["flags"],
            "status": "success" if call["completed"] and call["status"] == 0 else "failed",
            "facts": {key: call["facts"][key] for key in validator.FACTS},
            "structure": {
                "rows": call["rows"], "group_capacity": call["group_capacity"],
                # SQL identities come from the sealed qualification fixture,
                # not a reinterpretation of the native physical tags.
                "key_type": 23, "count_type": 0,
                "nullable_value": call["count_nullable"],
                "sql_mask": call["where_mask_present"] or call["measure_mask_present"],
                "mode": "parallel_dense_count" if call["native_mode"] == 2 else "unexpected",
                "workspace_layout_id": validator.object_digest(layout),
            },
            "raw_call": call,
        })
    return result


def write_new(path, value):
    with path.open("x") as stream:
        json.dump(value, stream, sort_keys=True, separators=(",", ":"), allow_nan=False)
        stream.write("\n")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("run_dir", type=Path)
    args = parser.parse_args()
    task_root = Path(__file__).resolve().parent
    run = args.run_dir.resolve()
    if run.parent != task_root or not run.name.startswith("companion-qualification"):
        raise ValueError("qualification directory outside exact task root")
    validator = load_validator(task_root / "dense-count-calibration.py")
    raw_path = run / "raw-traces.json"
    traces = validator.read(raw_path)
    qualification = validator.read(run / "qualification.json")
    validator.need(qualification["kind"] == "untimed-companion-qualification"
                   and qualification["N"] == 1250031
                   and qualification["nominal_groups"] == 257
                   and qualification["key_type"] == 23 and qualification["count_type"] == 0,
                   "qualification fixture identity changed")
    validator.need(set(traces) == {"warm", "before", "after", "overflow", "missing", "unexecuted"},
                   "unexpected trace inventory")
    canonical_traces = {}
    references = {}
    for name, raw in traces.items():
        path = run / ("raw-" + name + ".json")
        write_new(path, raw)
        ref = {"path": str(path.relative_to(task_root)), "sha256": validator.digest(path)}
        references[name] = ref
        calls = canonical(raw, validator)
        if name in ("warm", "before", "after"):
            validator.raw_trace_binding(ref, calls, task_root, 258)
            validator.trace_work(calls, 1250031)
            canonical_traces[name] = calls
        else:
            try:
                validator.raw_trace_binding(ref, calls, task_root, 258)
            except ValueError:
                pass
            else:
                raise ValueError("invalid trace unexpectedly accepted: " + name)
    validator.need(canonical_traces["before"] == canonical_traces["after"], "before/after drift")
    for name in ("warm", "before", "after", "overflow", "missing"):
        parity = qualification["results"][name]
        validator.need(parity["gpu_delta"] > 0 and parity["output_groups"] == 258
                       and all(parity[key] == 0 for key in
                               ("stock_delta", "native_kernel_delta", "accel_minus_native", "native_minus_accel")),
                       "dispatch/full parity failed")
    for name in ("before-ledger", "after-ledger", "overflow-ledger", "missing-ledger"):
        ledger = qualification["results"][name]
        validator.need(all(type(value) is int and value >= 0 for value in ledger.values())
                       and ledger["ledger"] - ledger["workspace_idle"] - ledger["output_idle"]
                       == ledger["normalized"] == ledger["baseline"], "exact normalized ledger failed")
    validator.need(validator.REQUIRED_SETTINGS <= set(qualification["settings"]), "incomplete settings")
    for name, value in {"pg_accel.cost_multiplier": "1", "pg_accel.min_batch_size": "65536",
                        "pg_accel.resident_memory_budget_mb": "-1", "pg_accel.enabled": "on",
                        "pg_accel.gpu_enabled": "on", "pg_accel.auto_load": "on",
                        "pg_accel.execution_profiling": "on"}.items():
        validator.need(qualification["settings"][name] == value, "settings changed: " + name)
    summary = {
        "kind": "untimed-companion-qualification-adapter", "not_calibration_data": True,
        "raw_collection_sha256": validator.digest(raw_path), "raw_traces": references,
        "canonical_before": canonical_traces["before"], "canonical_after": canonical_traces["after"],
        "work": validator.trace_work(canonical_traces["before"], 1250031),
        "qualification_sha256": validator.digest(run / "qualification.json"),
        "validator_sha256": validator.digest(task_root / "dense-count-calibration.py"),
        "adapter_sha256": validator.digest(Path(__file__)),
        "invalid_traces_rejected": ["overflow", "missing", "unexecuted"],
        "result": "PASS",
    }
    write_new(run / "canonical-qualification.json", summary)
    print("UNTIMED_ADAPTER_PASS (not a calibration acquisition)")


if __name__ == "__main__":
    main()
