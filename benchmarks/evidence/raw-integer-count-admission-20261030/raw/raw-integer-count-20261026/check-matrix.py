#!/usr/bin/env python3
"""Offline, fail-closed raw COUNT qualification. No database or subprocess calls."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import statistics
import sys

SCALES = (1_000_017, 2_000_017)
RAW = tuple(f"raw_int{k}_count_{m}" for k in (2, 4, 8) for m in ("star", "bool_filter"))
DENSE = tuple(f"dense_int{k}_count_control" for k in (2, 4, 8))
H3 = ("h3_parent_count_int4_res0", "h3_parent_count_bool_res0")
LANES = RAW + DENSE + H3
DEFAULTS = {"pg_accel.enabled": "on", "pg_accel.gpu_enabled": "on", "pg_accel.min_batch_size": "65536"}
SOURCE_ANCHORS = ("Cargo.toml", "Cargo.lock", "pg_accel/src/engine/executor/agg/descriptor.rs",
                  "pg_accel/src/engine/ffi/custom_scan/private_data.rs",
                  "pg_accel_bench/src/workloads/raw_integer_count.rs", "pg_accel_bench/src/report.rs",
                  "pg_accel_bench/src/runner.rs", "pgaccel-kernels/src/grouped_agg.cpp")
PG18_PREFIX = Path(".codex/scratch/uniform-budget-live-20261012/postgres/18.4")
PG18_MODULE_PATHS = (PG18_PREFIX / "lib/pg_accel.dylib", PG18_PREFIX / "bin/postgres", PG18_PREFIX / "bin/pg_config")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def load(path):
    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result, f"duplicate JSON key {key}")
            result[key] = value
        return result
    def reject(value):
        raise ValueError(f"nonfinite JSON constant {value}")
    return json.loads(path.read_text(), object_pairs_hook=pairs, parse_constant=reject)


def positive(value):
    return type(value) in (int, float) and math.isfinite(value) and value > 0


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def seal(path, base, confined=False):
    require(path.resolve().is_relative_to(base.resolve()), "seal outside root")
    entries = {}
    for line in path.read_text().splitlines():
        match = re.fullmatch(r"([0-9a-f]{64}) [ *](.+)", line)
        require(match is not None, f"malformed seal {path}")
        digest, name = match.groups()
        target = (base / name).resolve()
        require(target.is_relative_to(base.resolve()), "seal path escape")
        require(target not in entries, "duplicate seal target")
        require(target.is_file() and sha(target) == digest, f"hash mismatch {target}")
        entries[target] = digest
    require(entries, f"empty seal {path}")
    return entries


def settings(capture):
    pairs = capture["settings"]
    result = dict(pairs)
    require(len(result) == len(pairs), "duplicate setting")
    return result


def defaults(capture):
    values = settings(capture)
    require(all(values.get(k) == v for k, v in DEFAULTS.items()), "nondefault measured settings")
    require(float(values["pg_accel.cost_multiplier"]) == 1.0, "nondefault cost multiplier")


def node(plan):
    lines = plan.splitlines()
    starts = [i for i, line in enumerate(lines) if re.match(r"\s*(?:->\s*)?Custom Scan \(GpuAccel", line)]
    require(len(starts) == 1, "expected exactly one pg_accel node")
    index = starts[0]
    require("Custom Scan (GpuAccelAgg)" in lines[index], "wrong custom scan")
    indent = lambda line: len(line) - len(line.lstrip(" ->"))
    depth = indent(lines[index])
    children = []
    for line in lines[index + 1:]:
        if not line.strip():
            continue
        if indent(line) <= depth:
            break
        children.append(line)
    require(children, "empty descriptor")
    direct = min(map(indent, children))
    properties = {}
    for line in children:
        if indent(line) == direct and ":" in line:
            key, value = line.strip().split(":", 1)
            require(key.lower() not in properties, "duplicate node property")
            properties[key.lower()] = value.strip()
    return properties


def raw_plan(plan, lane, actual):
    p = node(plan)
    key_type = {"2": 21, "4": 23, "8": 20}[lane[7]]
    filtered = lane.endswith("bool_filter")
    match = re.fullmatch(fr"k0:fact\(([1-9][0-9]*)\.1:type={key_type}\) type={key_type} collation=0 encoding=hash", p["gpu descriptor group keys"])
    require(match is not None, "wrong raw key identity")
    relation = match[1]
    expected = {
        "gpu resident pipeline": "true", "gpu resident operator class": "resident_groupagg",
        "gpu descriptor strategy": "descriptor_grouped_aggregate", "gpu descriptor specialization": "hash_count",
        "gpu physical kernel mode": "parallel_hash",
        "gpu descriptor aggregates": f"m0:column({relation}.2:type=16) -> value.count" if filtered else "m0:count_star -> value.count",
        "gpu descriptor filter": f"fact=none; measures=[m0=sql_mask(input={relation}.3:type=16)]; dimensions=[none]" if filtered else "fact=none; measures=[m0=none]; dimensions=[none]",
        "gpu descriptor output": f"slot0:group[k0] source_type={key_type} result_type={key_type} typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.count source_type={16 if filtered else 0} result_type=20 typmod=-1 collation=0 nullable=false",
    }
    if actual:
        expected.update({"gpu dispatched physical kernel mode": "parallel_hash", "gpu physical kernel mode verified": "true", "gpu kernel dispatched": "true", "gpu lifecycle calls verified": "true"})
        require(p["gpu descriptor artifact policy"].split()[0] == "ephemeral_fused", "wrong actual artifact policy")
    require(all(p.get(k) == v for k, v in expected.items()), "raw descriptor/physical mode mismatch")
    return relation


def samples(pairs, count):
    require(len(pairs) == count, "wrong retained sample count")
    for pair in pairs:
        require(pair["cache_state"] == "warm" and pair["cache_purge"] == "not_requested", "not warm raw sample")
        require(type(pair["accel_first"]) is bool, "missing crossover order")
        require(positive(pair["accel_ms"]) and positive(pair["parallel_ms"]), "invalid timing")


def metrics(pairs):
    a, p = ([pair[key] for pair in pairs] for key in ("accel_ms", "parallel_ms"))
    return {"speedup": statistics.median(p) / statistics.median(a),
            "accel_cv_pct": statistics.stdev(a) / statistics.mean(a) * 100,
            "parallel_cv_pct": statistics.stdev(p) / statistics.mean(p) * 100}


def lifecycle(w, raw):
    probe = w["artifact_lifecycle_probe"]
    captures = w["artifact_steady_state_captures"]
    require(probe["phase"] == "lifecycle" and probe.get("pair_index") is None, "invalid lifecycle phase")
    require(positive(probe["refresh_us"]) and probe["refreshed_relations"] == 1 and probe["refreshed_rows"] == w["rows"], "missing refresh costs")
    samples([probe["iteration"]], 1)
    require(len(captures) == 10, "missing steady lifecycle evidence")
    require(sorted(c["pair_index"] for c in captures) == list(range(10)), "duplicate/missing steady pair")
    for capture in [probe] + captures:
        require(not capture.get("error"), "lifecycle error")
        require(capture["stock_exec_count"] == 0 and positive(capture["gpu_kernel_executions"]) and positive(capture["queries_accelerated"]) and positive(capture["output_rows_consumed"]), "lifecycle dispatch/output missing")
        for deps in (capture["dependencies_before"], capture["dependencies_after"]):
            require(len(deps) == 1 and deps[0]["row_count"] == w["rows"] and positive(deps[0]["raw_bytes"]), "missing dependency identity")
            require(all(positive(deps[0][k]) for k in ("relid", "generation", "global_generation", "relfilenode")), "invalid dependency stamp")
        artifact = capture["artifact"]
        if raw:
            require(capture.get("artifact_policy") == "ephemeral_fused", "wrong raw lifecycle policy")
            require(artifact["builds"] == 1 and all(artifact[k] == 0 for k in ("hits", "rebuilds", "artifact_bytes_observed", "construction_bytes")), "raw artifact is not zero-buffer ephemeral")
        if capture is not probe:
            require(capture["phase"] == "steady_state" and capture["iteration"] == w["iterations"][capture["pair_index"]], "steady sample substitution")
            require(capture["dependencies_before"] == capture["dependencies_after"], "changed steady dependency")
            require(all(capture[k] == 0 for k in ("refresh_us", "refreshed_relations", "refreshed_rows")) and artifact["raw_load_us"] == 0, "hidden steady refresh")
    return probe


def cell(w, artifact_root, sealed):
    lane, rows = w["name"], w["rows"]
    require(lane in LANES and rows in SCALES, "substituted lane/scale")
    samples(w["iterations"], 10)
    samples(w["warmup_iterations"], 5)
    require(sum(p["accel_first"] for p in w["iterations"]) == 5, "unbalanced pairs")
    raw = lane in RAW
    require(w["plan_selected"] is True and w["gpu_kernel_dispatched"] is True and w["dispatch_counter_captured"] is True, "missing selection/dispatch")
    require(positive(w["gpu_kernel_execution_delta"]) and w["pg_accel_stock_exec_delta"] == 0 and positive(w["accel_output_rows_consumed"]), "missing actual counters/output")
    require(w["physical_kernel_mode_verified"] is True and w["physical_kernel_mode_counts"]["serial_generic"] == 0, "unverified physical mode")
    require("GpuAccel" not in w["baseline_plan_snippet"] and bool(w["baseline_plan_snippet"].strip()), "baseline not native")
    diff_path = (artifact_root / w["correctness_diff_artifact"]).resolve()
    require(diff_path in sealed and diff_path.is_relative_to(artifact_root.resolve()), "unsealed parity artifact")
    diff = load(diff_path)
    require((diff["workload"], diff["rows"]) == (lane, rows) and diff["status"] == "pass" and not diff.get("error"), "wrong/failed parity report")
    require(diff["accel_minus_baseline_count"] == diff["baseline_minus_accel_count"] == 0 and diff["accel_rows"] == diff["baseline_rows"] and positive(diff["accel_rows"]), "full multiset parity failed")
    require(not diff["accel_minus_baseline_samples"] and not diff["baseline_minus_accel_samples"], "parity mismatch samples")
    probe = lifecycle(w, raw)
    captures = w["typed_h3_settings" if lane in H3 else "raw_integer_count_settings"]
    require(len(captures) == 16, "missing default captures")
    expected = {(phase, i, "warm") for phase, n in (("warmup",5),("measured",10),("lifecycle",1)) for i in range(n)}
    require({(c["phase"],c["index"],c["cache_state"]) for c in captures} == expected, "duplicate/missing default capture")
    for capture in captures:
        defaults(capture["settings"])
    if raw:
        require(diff["accel_rows"] == 16389, "wrong sparse cardinality")
        require(w["physical_kernel_mode"] == "parallel_hash" and positive(w["physical_kernel_mode_counts"]["parallel_hash"]), "wrong raw physical mode")
        require(w["raw_integer_count_native_kernel_deltas"] == [0] * 15, "native kernel delta missing/nonzero")
        proof = w["raw_integer_count_execution_proof"]
        defaults(proof["settings"])
        require(positive(proof["gpu_kernel_execution_delta"]) and proof["stock_exec_delta"] == 0, "off-clock dispatch proof missing")
        require(raw_plan(w["plan_snippet"],lane,False) == raw_plan(proof["explain_analyze"],lane,True), "static/actual relation substitution")
        fixture_path = artifact_root / "fixture_manifests" / f"{lane}-{rows}.json"
        require(fixture_path.resolve() in sealed, "unsealed fixture manifest")
        fixture = load(fixture_path)
        require((fixture["workload"],fixture["rows"],fixture["non_null_groups"],fixture["null_group"]) == (lane,rows,16388,True), "wrong predeclared fixture")
        require(fixture["mandatory_scales"] == list(SCALES) and fixture["warmups"] == 5 and fixture["iterations"] == 10 and fixture["seed"] == 42 and fixture["setup_sql"], "missing pre-timing SQL/seed")
        require(fixture["query_sql"] == diff["accel_query_sql"] == diff["baseline_query_sql"], "query substitution")
        count = "count(value) FILTER (WHERE qualifies)" if lane.endswith("bool_filter") else "count(*)"
        require(fixture["query_sql"] == f"SELECT k, {count} AS n FROM bench_raw_integer_count GROUP BY k", "noncanonical raw SQL")
    elif lane in DENSE:
        require(w["physical_kernel_mode"] == "parallel_dense_count" and positive(w["physical_kernel_mode_counts"]["parallel_dense_count"]), "dense control lost dense execution")
        require(diff["accel_rows"] == 65, "wrong dense control cardinality")
        p = node(w["plan_snippet"])
        require(p["gpu descriptor aggregates"] == "m0:count_star -> value.count" and p["gpu descriptor specialization"] == "dense_count_plain", "wrong dense control descriptor")
    else:
        require(w["physical_kernel_mode"] == "parallel_hash" and positive(w["physical_kernel_mode_counts"]["parallel_hash"]), "H3 control lost hash execution")
        p = node(w["plan_snippet"])
        key = re.fullmatch(r"k0:h3_cell_to_parent\(([1-9][0-9]*)\.1:type=([1-9][0-9]*), resolution=0\) type=\2 collation=0 encoding=hash",p["gpu descriptor group keys"])
        require(key is not None, "wrong H3 resolution/key control")
        oid = 23 if "int4" in lane else 16
        require(p["gpu descriptor aggregates"] == f"m0:column({key[1]}.2:type={oid}) -> value.count" and p["gpu descriptor filter"] == "fact=none; measures=[m0=none]; dimensions=[none]", "wrong H3 COUNT control")
    measured = metrics(w["iterations"])
    combined = metrics([probe["iteration"]] + w["iterations"])
    for field, count, calculated in (("warm_summary",10,measured),("combined_warm_summary",11,combined)):
        summary = w[field]
        require(summary["n"] == count, "summary count mismatch")
        for saved, key in (("speedup_median_vs_parallel","speedup"),("accel_cv_pct","accel_cv_pct"),("parallel_cv_pct","parallel_cv_pct")):
            require(type(summary[saved]) in (int,float) and math.isclose(summary[saved],calculated[key],rel_tol=1e-7,abs_tol=1e-7), "summary/sample mismatch")
    return {"lane":lane,"rows":rows,"role":"mandatory_raw" if raw else "control", **measured,
            "refresh_us":probe["refresh_us"], "lifecycle_accel_ms":probe["iteration"]["accel_ms"],
            "lifecycle_speedup":probe["iteration"]["parallel_ms"]/probe["iteration"]["accel_ms"],
            "combined_speedup":combined["speedup"],"combined_accel_cv_pct":combined["accel_cv_pct"],
            "variance_flag": max(measured["accel_cv_pct"],measured["parallel_cv_pct"]) > 15,
            "warm_floor_met":measured["speedup"] >= 1.15 if raw else None,
            "control_below_parity":not raw and measured["speedup"] < 1.0,
            "released_h3_floor_met":measured["speedup"] >= 1.15 if lane in H3 else None}


def check(build, runs, repo):
    task_root = repo / ".codex/scratch/raw-integer-count-20261026"
    require(build.resolve().parent == task_root.resolve() and build.name.startswith("build18-"), "noncanonical build root")
    require((build / "final-terminal.log").resolve().is_relative_to(repo.resolve()), "build terminal path escape")
    require((build / "final-terminal.log").read_text().strip() == "terminal=0", "build failed")
    build_seals = {name:seal(build / name, repo) for name in ("source.sha256","module.sha256","bench.sha256","final-harness.sha256")}
    require(all((repo / name).resolve() in build_seals["source.sha256"] for name in SOURCE_ANCHORS), "source seal missing qualification inputs")
    require(set(build_seals["bench.sha256"]) == {(repo / "target/release/pg_accel_bench").resolve()}, "wrong harness binary seal")
    require(set(build_seals["module.sha256"]) == {(repo / path).resolve() for path in PG18_MODULE_PATHS}, "wrong qualified PG18.4 module/PostgreSQL seal")
    require(set(build_seals["final-harness.sha256"]) == {(task_root / n).resolve() for n in ("build-final.sh","build-production.sh")}, "wrong build wrapper seal")
    cells, seen_lanes, errors = [], set(), []
    for run in runs:
        try:
            require(run.resolve().parent == task_root.resolve() and run.name.startswith("performance18-"), "noncanonical run root")
            sealed = seal(run / "evidence.sha256",run,True)
            for name in ("report.json","lane.txt","build-dir.txt","terminal.log","harness.sha256","source-after.log","module-after.log","harness-after.log","status.log","stop.log","device.log","extensions.log","bench.log"):
                require((run / name).resolve() in sealed, f"missing sealed {name}")
            require((run / "build-dir.txt").read_text().strip() == str(build.resolve()), "wrong build linkage")
            require((run / "terminal.log").read_text().strip() == "terminal=0", "run failed")
            require(not (run / "pgdata/postmaster.pid").exists(), "postmaster still present")
            run_harness = seal(run / "harness.sha256",repo)
            require(set(run_harness) == {(task_root / "run-benchmark.sh").resolve(),(repo / "target/release/pg_accel_bench").resolve()}, "wrong run wrapper/binary seal")
            require(all(path.resolve() in sealed for path in (run / "artifacts").rglob("*") if path.is_file()), "omitted artifact seal entry")
            for manifest, receipt in (("source.sha256","source-after.log"),("module.sha256","module-after.log")):
                expected = {line[66:] + ": OK" for line in (build / manifest).read_text().splitlines()}
                require(set((run / receipt).read_text().splitlines()) == expected, "incomplete after-run verification")
            expected = {line[66:] + ": OK" for line in (run / "harness.sha256").read_text().splitlines()}
            require(set((run / "harness-after.log").read_text().splitlines()) == expected, "incomplete harness verification")
            lane = (run / "lane.txt").read_text().strip()
            require(lane in LANES and lane not in seen_lanes, "duplicate/substituted lane run")
            seen_lanes.add(lane)
            report = load(run / "report.json")
            require(report["crashes"] == [], "report contains crashes")
            method = report["methodology"]
            require(method["iterations"] == 10 and method["warmup"] == 5 and method["timing_mode"] == "raw-wallclock" and method["cache_mode"] == "warm" and method["harness_profile"] == "release" and method["row_scales"] == list(SCALES), "wrong methodology")
            gucs = settings(report["gucs"])
            require(all(gucs.get(k) == v for k,v in {"shared_buffers":"16GB","work_mem":"512MB","max_parallel_workers_per_gather":"8","max_parallel_workers":"12"}.items()), "not observed realistic PG settings")
            require(report["hardware"], "missing hardware")
            artifact_root = run / "artifacts"
            require(Path(report["artifact_dir"]).resolve() == artifact_root.resolve(), "artifact root substitution")
            workloads = report["workloads"]
            require(len(workloads) == 2 and {(w["name"],w["rows"]) for w in workloads} == {(lane,n) for n in SCALES}, "missing/duplicate/substituted cells")
            for w in workloads:
                cells.append(cell(w,artifact_root,sealed))
        except (ValueError,KeyError,TypeError,OSError,IndexError) as error:
            errors.append(f"{run.name}: {error}")
    require_seen = {(lane,n) for lane in LANES for n in SCALES}
    actual = {(c["lane"],c["rows"]) for c in cells}
    if actual != require_seen or len(cells) != 22:
        errors.append(f"incomplete matrix: missing={sorted(require_seen-actual)}; count={len(cells)}")
    errors.extend(f"{c['lane']}@{c['rows']}: warm speedup {c['speedup']:.6f} below 1.15" for c in cells if c["warm_floor_met"] is False)
    errors.extend(f"{c['lane']}@{c['rows']}: released H3 control below its 1.15 floor" for c in cells if c["released_h3_floor_met"] is False)
    errors.extend(f"{c['lane']}@{c['rows']}: dense control below parity" for c in cells if c["lane"] in DENSE and c["control_below_parity"])
    return {"status":"fail" if errors else "pass", "errors":errors, "cells":cells,
            "scope":"warm-only; controls and refresh-inclusive losses retained; no cold-cache qualification",
            "build_dir":str(build.resolve()), "checker_sha256":sha(Path(__file__))}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo",type=Path,default=Path(__file__).resolve().parents[3])
    parser.add_argument("--build-dir",type=Path,required=True)
    parser.add_argument("runs",type=Path,nargs="+")
    args = parser.parse_args()
    try:
        result = check(args.build_dir.resolve(),args.runs,args.repo.resolve())
    except (ValueError,KeyError,TypeError,OSError,IndexError) as error:
        result = {"status":"fail","errors":[str(error)],"cells":[]}
    print(json.dumps(result,indent=2,allow_nan=False))
    return int(result["status"] != "pass")


if __name__ == "__main__":
    sys.exit(main())
