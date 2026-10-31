#!/usr/bin/env python3
"""Review-gated validation only against an immutable reviewed training fit.

Uses persistent psql backends and retains every 5+10 interval. Companion calls
are untimed; old timed code is unchanged. Current observations are separate and
never enter the fit. The shell wrapper owns all server lifetimes/deadlines.
"""
import argparse
from datetime import datetime, timezone
import importlib.util
import json
from pathlib import Path
import subprocess
import shutil
import time


ROOT = Path(__file__).resolve().parent


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), ROOT / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


V = load("dense-count-calibration")
A = load("qualify-companion")


def now():
    return datetime.now(timezone.utc).isoformat()


def write(path, value):
    with path.open("x") as stream:
        json.dump(value, stream, sort_keys=True, separators=(",", ":"), allow_nan=False)
        stream.write("\n")
    return reference(path)


def reference(path):
    path = path.resolve()
    V.need(path.is_relative_to(ROOT), "evidence outside task root")
    return {"path": str(path.relative_to(ROOT)), "sha256": V.digest(path)}


def fixture(n, groups):
    # Original 16388 generator remains exact for later regression validation;
    # all other frozen capacities use one signed, injective sparse INT4 map.
    key = "g * 200003 - 1638844582" if groups == 16388 else f"(g - {groups // 2}) * 32749 + 42"
    return {"key_type": 23, "count_type": 0, "seed": 42, "setup_sql": [
        "CREATE TEMP TABLE bench_raw_integer_count(k int4,value boolean,qualifies boolean)",
        f"INSERT INTO bench_raw_integer_count SELECT CASE WHEN i%97=0 THEN NULL::int4 ELSE ({key})::int4 END, CASE WHEN i%7=0 THEN NULL::boolean ELSE i%2=0 END, CASE WHEN g=0 THEN false WHEN i%11=0 THEN NULL::boolean ELSE i%3<>0 END FROM (SELECT i,CASE WHEN i>{groups} AND i%5=0 THEN 0 ELSE (i-1)%{groups} END g FROM generate_series(1::bigint,{n}) s(i)) fixture",
        "ANALYZE bench_raw_integer_count"],
        "query_sql": "SELECT k,count(*) AS n FROM bench_raw_integer_count GROUP BY k"}


def inventory(source, seal):
    entries = []
    for line in seal.read_text().splitlines():
        digest, relative = line.split("  ", 1)
        path = source / relative
        V.need(V.digest(path) == digest, "source seal drift: " + relative)
        entries.append(reference(path))
    return entries


def prepare(args):
    run = args.run_dir.resolve()
    V.need(run == ROOT / "dense-count-validation1" and not run.exists(), "fresh validation directory required")
    training = ROOT / "dense-count-training3"
    fit_path = training / "frozen-fit.json"
    fit_hash = "27a4219e358771bf72d7b399b20bba10b8e6bf54f8e4c82e704c49464c20b84a"
    V.need(V.digest(fit_path) == fit_hash, "reviewed fit changed")
    V.manifest(training / "manifest.json", ROOT)
    run.mkdir()
    write(run / "validation-binding.json", {
        "kind": "frozen_fit_validation_acquisition",
        "manifest": reference(training / "manifest.json"), "fit": reference(fit_path),
        "harness": [reference(Path(__file__)), reference(ROOT / "run-dense-count-validation.sh")],
        "cell_ids": V.ids("holdout") + V.ids("existing_validation")})
    print("VALIDATION_PREPARED_NO_RESPONSES")


class Backend:
    def __init__(self, psql, connection, log):
        self.log = log.open("x")
        self.error = log.with_suffix(".stderr").open("x")
        self.process = subprocess.Popen([str(psql), connection, "-X", "-qAt", "-v", "ON_ERROR_STOP=1"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.error, text=True, bufsize=1)
        self.sequence = 0

    def execute(self, sql):
        self.sequence += 1
        marker = f"__DENSE_TRAINING_END_{self.sequence}__"
        self.log.write(sql + ";\n")
        self.log.flush()
        self.process.stdin.write(sql + ";\nSELECT '" + marker + "';\n")
        self.process.stdin.flush()
        lines = []
        while True:
            line = self.process.stdout.readline()
            if not line:
                raise RuntimeError("psql exited before complete response")
            self.log.write(line)
            self.log.flush()
            if line.rstrip("\n") == marker:
                return "\n".join(lines)
            lines.append(line.rstrip("\n"))

    def json(self, sql):
        return json.loads(self.execute(sql))

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.terminate()
                try:
                    self.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=5)
        self.log.close()
        self.error.close()


def snapshot(backend):
    return backend.json("SELECT jsonb_object_agg(stage,jsonb_build_object('calls',calls,'elapsed_us',elapsed_us)) FROM pg_accel_descriptor_stage_stats()")


def profile(backend):
    names = ",".join("'" + name + "'" for name in sorted(V.REQUIRED_SETTINGS))
    settings = backend.json("SELECT jsonb_object_agg(name,setting) FROM pg_settings WHERE name LIKE 'pg_accel.%' OR name IN (" + names + ")")
    limits = backend.json("SELECT jsonb_build_object('values',jsonb_object_agg(name,value),'sources',jsonb_object_agg(name,source)) FROM pg_accel_device_limits()")
    return settings, limits


def initialize(backend, fixture, m):
    backend.execute("SET statement_timeout='5min'; SET temp_file_limit='2GB'; SET pg_accel.enabled=on; SET pg_accel.gpu_enabled=on; SET pg_accel.auto_load=on; SET pg_accel.execution_profiling=on")
    settings, limits = profile(backend)
    V.need(settings == m["_settings"] and limits == m["_limits"], "actual profile/device differs from frozen manifest")
    for sql in fixture["setup_sql"]:
        backend.execute(sql)
    backend.execute("SELECT pg_accel_pin('bench_raw_integer_count'::regclass,ARRAY['k'])")
    return settings, limits


def counters(backend):
    return backend.json("SELECT jsonb_build_object('gpu',pg_accel_kernel_executions(),'stock',(SELECT stock_exec_count FROM pg_accel_stats()))")


def parity(backend, sql, groups):
    backend.execute("SET pg_accel.enabled=off")
    before = counters(backend)
    backend.execute("CREATE TEMP TABLE expected AS " + sql)
    after = counters(backend)
    V.need(after == before, "native oracle used extension")
    backend.execute("SET pg_accel.enabled=on")
    accel_before = counters(backend)
    backend.execute("CREATE TEMP TABLE actual AS " + sql)
    accel_after = counters(backend)
    V.need(accel_after["stock"] == accel_before["stock"], "stock fallback in parity")
    # Compare already materialized outputs natively. Otherwise the old planner
    # may auto-load the oracle's aggregate subqueries while they are in use.
    backend.execute("SET pg_accel.enabled=off")
    compare_before = counters(backend)
    result = backend.json("SELECT jsonb_build_object('accel_minus_native',(SELECT count(*) FROM (TABLE actual EXCEPT ALL TABLE expected) q),'native_minus_accel',(SELECT count(*) FROM (TABLE expected EXCEPT ALL TABLE actual) q),'output_groups',(SELECT count(*) FROM actual),'native_kernel_delta',0)")
    V.need(counters(backend) == compare_before, "parity comparison used extension")
    V.need(result["accel_minus_native"] == result["native_minus_accel"] == 0 and result["output_groups"] == groups + 1, "full parity failed")
    backend.execute("DROP TABLE actual")
    backend.execute("SET pg_accel.enabled=on")
    return result


def analyze(backend, sql):
    return backend.execute("EXPLAIN (ANALYZE,VERBOSE,COSTS OFF,TIMING OFF) " + sql)


def planned_calls(plan):
    import re
    found = re.findall(r"GPU Planned Lifecycle Calls: ([0-9]+)", plan)
    V.need(len(found) == 1 and 1 <= int(found[0]) <= 4096, "missing/ambiguous actual lifecycle")
    return int(found[0])


def trace(backend, sql, calls, run, cell_id, phase, groups):
    backend.execute(f"SELECT pg_accel_count_calibration_arm({calls})")
    plan = analyze(backend, sql)
    raw = backend.json("SELECT pg_accel_count_calibration_drain()")
    raw_ref = write(run / (cell_id + "-raw-" + phase + ".json"), raw)
    canonical = A.canonical(raw, V)
    V.raw_trace_binding(raw_ref, canonical, ROOT, groups + 1)
    write(run / (cell_id + "-companion-" + phase + "-plan.json"), {"plan": plan})
    return canonical, raw_ref


def samples(backend, sql, m, calls, require_dense, run, cell_id, role):
    pid = backend.json("SELECT pg_backend_pid()")
    result = []
    for index in range(15):
        settings, limits = profile(backend)
        V.need(settings == m["_settings"] and limits == m["_limits"], "profile drift")
        native_before = counters(backend)
        before = snapshot(backend)
        started = time.perf_counter_ns()
        plan = analyze(backend, sql)
        elapsed = time.perf_counter_ns() - started
        after = snapshot(backend)
        native_after = counters(backend)
        sample = {"sequence": index, "backend_pid": pid, "settings": settings,
            "device_limits": limits, "before": before, "after": after,
            "actual_plan": plan, "diagnostic_wall_ns": elapsed,
            "native_before": native_before, "native_after": native_after}
        # Retain each complete interval before any assertion can reject it.
        write(run / f"{cell_id}-{role}-interval-{index:02d}.json", sample)
        V.need(native_after["stock"] == native_before["stock"], "stock fallback in sample")
        if require_dense:
            V.need(native_after["gpu"] > native_before["gpu"], "dense sample did not dispatch")
            V.sample_us(sample, pid, calls, m["_settings"], m["_limits"])
        result.append(sample)
    return pid, result


def acquire(args):
    run = args.run_dir.resolve()
    V.need(run == ROOT / "dense-count-validation1", "fixed training root required")
    manifest_path = ROOT / "dense-count-training3/manifest.json"
    m = V.manifest(manifest_path, ROOT)
    mhash = V.digest(manifest_path)
    binding = V.read(run / "validation-binding.json")
    for ref in [binding["manifest"], binding["fit"], *binding["harness"]]:
        V.need(V.digest(ROOT / ref["path"]) == ref["sha256"], "validation identity changed")
    fit_sha = binding["fit"]["sha256"]
    V.need(fit_sha == "27a4219e358771bf72d7b399b20bba10b8e6bf54f8e4c82e704c49464c20b84a", "unreviewed fit")
    current_build = ROOT / "build18-workfacts"
    V.need((current_build / "final-terminal.log").read_text().strip() == "terminal=0", "current build not qualified")
    # Endpoint strings are fixed by the reviewed wrapper, never read from JSON.
    endpoints = {
        "timed": (ROOT / "control-baseline-build2/postgres18/bin/psql", "host=" + str(ROOT.parent / "counttrainold") + " port=33918 dbname=postgres connect_timeout=10"),
        "companion": (ROOT / "calibration-companion-build1/postgres18/bin/psql", "host=" + str(ROOT.parent / "counttraindiag") + " port=33919 dbname=postgres connect_timeout=10"),
        "current": (ROOT.parent / "uniform-budget-live-20261012/postgres/18.4/bin/psql", "host=" + str(ROOT.parent / "counttrainnew") + " port=33920 dbname=postgres connect_timeout=10"),
    }
    for n in V.NS:
        for g in V.GROUPS["holdout"] + V.GROUPS["existing_validation"]:
            role = "holdout" if g in V.GROUPS["holdout"] else "existing_validation"
            cell_id = f"n{n}-g{g}"
            fixture_data = m["_fixtures"][cell_id]
            started = now()
            backends = {}
            try:
                for backend_role, (psql, connection) in endpoints.items():
                    backend = Backend(psql, connection, run / (cell_id + "-" + backend_role + ".log"))
                    backends[backend_role] = backend
                    initialize(backend, fixture_data, m)
                query = fixture_data["query_sql"]
                parities = {role: parity(backend, query, g) for role, backend in backends.items()}
                observed = analyze(backends["companion"], query)
                calls = planned_calls(observed)
                before, before_ref = trace(backends["companion"], query, calls, run, cell_id, "before", g)
                V.trace_work(before, n)
                pid, old_samples = samples(backends["timed"], query, m, calls, True, run, cell_id, "timed")
                current_pid, current_samples = samples(backends["current"], query, m, calls, False, run, cell_id, "current")
                after, after_ref = trace(backends["companion"], query, calls, run, cell_id, "after", g)
                V.need(before == after, "companion work drift around timing")
                cell = {"schema_version": 1, "role": role, "N": n, "nominal_groups": g,
                    "variant": "ce24_dense", "manifest_sha256": mhash,
                    "timed_binary_sha256": m["identities"]["timed_binary"]["sha256"],
                    "acquisition_started_at": started, "backend_pid": pid,
                    "trace_before": before, "trace_after": after,
                    "raw_trace_before": before_ref, "raw_trace_after": after_ref,
                    "warmups": old_samples[:5], "samples": old_samples[5:],
                    "parity": parities["timed"], "fixture": fixture_data}
                acquisition = {"kind": "calibration_acquisition", "terminal": 0,
                    "bindings": {role: ref["sha256"] for role, ref in m["identities"].items()},
                    "manifest_sha256": mhash, "role": role, "cell_id": cell_id,
                    "backend_pid": pid, "started_at": started, "completed_at": now(),
                    "heldout_fit_sha256": fit_sha,
                    "validation_binding": reference(run / "validation-binding.json"),
                    **{field + "_sha256": V.object_digest(cell[field]) for field in
                       ("trace_before", "trace_after", "warmups", "samples", "parity")},
                    "raw_trace_before_sha256": before_ref["sha256"], "raw_trace_after_sha256": after_ref["sha256"]}
                cell["acquisition_receipt"] = write(run / (cell_id + "-acquisition.json"), acquisition)
                path = run / (cell_id + ".json")
                write(path, cell)
                V.receipt(path, ROOT, m, mhash, role)
                write(run / (cell_id + "-current.json"), {"kind": "current_default_observation_not_fit_input",
                    "current_build": reference(current_build / "module.sha256"), "fixture": fixture_data,
                    "backend_pid": current_pid, "warmups": current_samples[:5], "samples": current_samples[5:],
                    "parity": parities["current"], "companion_parity": parities["companion"]})
            finally:
                for backend in backends.values():
                    backend.close()
    V.need(V.digest(manifest_path) == mhash, "manifest changed during acquisition")
    print("VALIDATION_12_CELLS_COMPLETE_NO_REFIT")


def complete(args):
    run = args.run_dir.resolve()
    V.need(run == ROOT / "dense-count-validation1", "fixed completion root required")
    V.need((run / "terminal.log").read_text().strip() == "terminal=0", "wrapper did not finish successfully")
    manifest_path = ROOT / "dense-count-training3/manifest.json"
    m = V.manifest(manifest_path, ROOT)
    mhash = V.digest(manifest_path)
    cells = {}
    for cell_id in V.ids("holdout") + V.ids("existing_validation"):
        role = "holdout" if cell_id in V.ids("holdout") else "existing_validation"
        path = run / (cell_id + ".json")
        V.receipt(path, ROOT, m, mhash, role)
        cells[cell_id] = reference(path)
    for role in ("timed", "companion", "current"):
        server = run / "servers" / role
        V.need(not (server / "pgdata/postmaster.pid").exists(), "server still owns PID file")
    # Exclusive creation after all checks: never overwrite a completion receipt.
    write(run / "completion.json", {"kind": "validation_run_complete", "terminal": 0,
        "manifest_sha256": mhash, "cells": cells,
        "validation_binding": reference(run / "validation-binding.json")})


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("prepare", "acquire", "complete"))
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--reviews", type=Path, nargs="*", default=[])
    args = parser.parse_args()
    if args.command == "prepare":
        prepare(args)
    elif args.command == "acquire":
        acquire(args)
    else:
        complete(args)


if __name__ == "__main__":
    main()

