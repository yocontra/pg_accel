"""Hermetic synthetic evidence tests; these fixtures are NOT performance evidence."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("matrix", Path(__file__).with_name("check-matrix.py"))
M = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(M)


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def dump(path, value):
    write(path, json.dumps(value))


def seal(path, files, base):
    write(path, "".join(f"{M.sha(file)}  {file.relative_to(base)}\n" for file in files))


def defaults():
    return {"settings":list({**M.DEFAULTS,"pg_accel.cost_multiplier":"1"}.items())}


def plan(lane, actual=False):
    oid = {"2":21,"4":23,"8":20}[lane[7]]
    filtered = lane.endswith("bool_filter")
    aggregate = "m0:column(42.2:type=16) -> value.count" if filtered else "m0:count_star -> value.count"
    predicate = "sql_mask(input=42.3:type=16)" if filtered else "none"
    result = f"""Custom Scan (GpuAccelAgg)
  GPU Resident Pipeline: true
  GPU Resident Operator Class: resident_groupagg
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: hash_count
  GPU Physical Kernel Mode: parallel_hash
  GPU Descriptor Group Keys: k0:fact(42.1:type={oid}) type={oid} collation=0 encoding=hash
  GPU Descriptor Aggregates: {aggregate}
  GPU Descriptor Filter: fact=none; measures=[m0={predicate}]; dimensions=[none]
  GPU Descriptor Output: slot0:group[k0] source_type={oid} result_type={oid} typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.count source_type={16 if filtered else 0} result_type=20 typmod=-1 collation=0 nullable=false
"""
    if actual:
        result += "  GPU Descriptor Artifact Policy: ephemeral_fused reason=zero_retained_bytes\n  GPU Dispatched Physical Kernel Mode: parallel_hash\n  GPU Physical Kernel Mode Verified: true\n  GPU Lifecycle Calls Verified: true\n  GPU Kernel Dispatched: true\n"
    return result


def pair(index=0, accel=10):
    return {"accel_ms":accel,"parallel_ms":20,"accel_first":index % 2 == 0,"cache_state":"warm","cache_purge":"not_requested"}


def summary(pairs):
    m = M.metrics(pairs)
    return {"n":len(pairs),"speedup_median_vs_parallel":m["speedup"],"accel_cv_pct":m["accel_cv_pct"],"parallel_cv_pct":m["parallel_cv_pct"]}


def workload(lane, rows):
    raw = lane in M.RAW
    mode = "parallel_dense_count" if lane in M.DENSE else "parallel_hash"
    pairs = [pair(i) for i in range(10)]
    dep = {"relid":42,"generation":1,"global_generation":1,"relfilenode":42,"row_count":rows,"raw_bytes":rows*5,"derived_bytes":0}
    capture = {"phase":"lifecycle","iteration":pair(accel=100),"refresh_us":1000,"refreshed_relations":1,"refreshed_rows":rows,"dependencies_before":[dep],"dependencies_after":[dep],"queries_accelerated":1,"gpu_kernel_executions":1,"stock_exec_count":0,"output_rows_consumed":16389,"artifact_policy":"ephemeral_fused","artifact":{"builds":1,"hits":0,"rebuilds":0,"artifact_bytes_observed":0,"construction_bytes":0,"raw_load_us":0}}
    steady = []
    for i,p in enumerate(pairs):
        c = copy.deepcopy(capture)
        c.update(phase="steady_state",pair_index=i,iteration=p,refresh_us=0,refreshed_relations=0,refreshed_rows=0)
        steady.append(c)
    w = {"name":lane,"rows":rows,"iterations":pairs,"warmup_iterations":[pair(i) for i in range(5)],"plan_selected":True,"gpu_kernel_dispatched":True,"dispatch_counter_captured":True,"gpu_kernel_execution_delta":10,"pg_accel_stock_exec_delta":0,"accel_output_rows_consumed":163890,"physical_kernel_mode_verified":True,"physical_kernel_mode":mode,"physical_kernel_mode_counts":{"serial_generic":0,"parallel_hash":10 if mode=="parallel_hash" else 0,"parallel_dense_count":10 if mode=="parallel_dense_count" else 0},"baseline_plan_snippet":"Finalize HashAggregate\n  -> Gather\n","correctness_diff_artifact":f"correctness_diffs/{lane}-{rows}.json","artifact_lifecycle_probe":capture,"artifact_steady_state_captures":steady,"warm_summary":summary(pairs),"combined_warm_summary":summary([capture["iteration"]]+pairs)}
    if raw:
        w.update(plan_snippet=plan(lane),raw_integer_count_native_kernel_deltas=[0]*15,
                 raw_integer_count_execution_proof={"explain_analyze":plan(lane,True),"gpu_kernel_execution_delta":1,"stock_exec_delta":0,"settings":defaults()},
                 raw_integer_count_settings=[{"phase":phase,"index":i,"cache_state":"warm","settings":defaults()} for phase,n in (("warmup",5),("measured",10),("lifecycle",1)) for i in range(n)])
    else:
        w["typed_h3_settings" if lane in M.H3 else "raw_integer_count_settings"]=[{"phase":phase,"index":i,"cache_state":"warm","settings":defaults()} for phase,n in (("warmup",5),("measured",10),("lifecycle",1)) for i in range(n)]
        if lane in M.DENSE:
            w["plan_snippet"]="Custom Scan (GpuAccelAgg)\n  GPU Descriptor Aggregates: m0:count_star -> value.count\n  GPU Descriptor Specialization: dense_count_plain"
        else:
            oid=23 if "int4" in lane else 16
            w["plan_snippet"]=f"Custom Scan (GpuAccelAgg)\n  GPU Descriptor Group Keys: k0:h3_cell_to_parent(42.1:type=90001, resolution=0) type=90001 collation=0 encoding=hash\n  GPU Descriptor Aggregates: m0:column(42.2:type={oid}) -> value.count\n  GPU Descriptor Filter: fact=none; measures=[m0=none]; dimensions=[none]"
    return w


class MatrixTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name).resolve()
        self.root = self.repo / ".codex/scratch/raw-integer-count-20261026"
        self.build = self.root / "build18-synthetic"
        manifest_files = {
            "source.sha256":[self.repo/n for n in M.SOURCE_ANCHORS],
            "module.sha256":[self.repo/n for n in M.PG18_MODULE_PATHS],
            "bench.sha256":[self.repo/"target/release/pg_accel_bench"],
            "final-harness.sha256":[self.root/n for n in ("build-final.sh","build-production.sh")],
        }
        for name, files in manifest_files.items():
            for file in files:
                write(file,"synthetic non-production identity")
            seal(self.build/name,files,self.repo)
        write(self.root/"run-benchmark.sh","synthetic wrapper")
        write(self.build/"final-terminal.log","terminal=0\n")
        self.runs=[]
        for lane in M.LANES:
            run = self.root / f"performance18-{lane}"
            self.runs.append(run)
            for name in ("bench.log","device.log","extensions.log","status.log","stop.log"):
                write(run/name,"synthetic test receipt\n")
            write(run/"lane.txt",lane)
            write(run/"build-dir.txt",str(self.build))
            write(run/"terminal.log","terminal=0\n")
            seal(run/"harness.sha256",[self.root/"run-benchmark.sh",self.repo/"target/release/pg_accel_bench"],self.repo)
            for name,manifest in (("source-after.log",self.build/"source.sha256"),("module-after.log",self.build/"module.sha256"),("harness-after.log",run/"harness.sha256")):
                write(run/name,"".join(line[66:]+": OK\n" for line in manifest.read_text().splitlines()))
            report={"crashes":[],"hardware":{"cpu":"synthetic"},"artifact_dir":str(run/"artifacts"),"methodology":{"iterations":10,"warmup":5,"row_scales":list(M.SCALES),"timing_mode":"raw-wallclock","cache_mode":"warm","harness_profile":"release"},"gucs":{"settings":list({"shared_buffers":"16GB","work_mem":"512MB","max_parallel_workers_per_gather":"8","max_parallel_workers":"12"}.items())},"workloads":[]}
            for rows in M.SCALES:
                w=workload(lane,rows)
                report["workloads"].append(w)
                count="count(value) FILTER (WHERE qualifies)" if lane.endswith("bool_filter") else "count(*)"
                query=f"SELECT k, {count} AS n FROM bench_raw_integer_count GROUP BY k"
                diff={"workload":lane,"rows":rows,"status":"pass","accel_rows":65 if lane in M.DENSE else 16389,"baseline_rows":65 if lane in M.DENSE else 16389,"accel_minus_baseline_count":0,"baseline_minus_accel_count":0,"accel_minus_baseline_samples":[],"baseline_minus_accel_samples":[],"accel_query_sql":query,"baseline_query_sql":query}
                dump(run/"artifacts"/w["correctness_diff_artifact"],diff)
                if lane in M.RAW:
                    dump(run/"artifacts/fixture_manifests"/f"{lane}-{rows}.json",{"workload":lane,"rows":rows,"non_null_groups":16388,"null_group":True,"mandatory_scales":list(M.SCALES),"warmups":5,"iterations":10,"seed":42,"setup_sql":["synthetic"],"query_sql":query})
            dump(run/"report.json",report)
            self.reseal(run)

    def reseal(self,run):
        seal(run/"evidence.sha256",sorted(p for p in run.rglob("*") if p.is_file() and p.name!="evidence.sha256"),run)

    def check(self,runs=None):
        return M.check(self.build,self.runs if runs is None else runs,self.repo)

    def mutate(self,fn):
        run=self.runs[0]
        report=M.load(run/"report.json")
        fn(report)
        dump(run/"report.json",report)
        self.reseal(run)

    def test_complete_and_refresh_loss_retained(self):
        result=self.check()
        self.assertEqual(result["errors"],[])
        self.assertEqual(len(result["cells"]),22)
        self.assertEqual(result["cells"][0]["lifecycle_speedup"],0.2)

    def test_missing_lane(self):
        self.assertEqual(self.check(self.runs[1:])["status"],"fail")

    def test_duplicate_lane(self):
        self.assertEqual(self.check(self.runs+[self.runs[0]])["status"],"fail")

    def test_mutations(self):
        original=(self.runs[0]/"report.json").read_text()
        mutations=[lambda r:r["workloads"].pop(),
                   lambda r:r["workloads"].__setitem__(1,copy.deepcopy(r["workloads"][0])),
                   lambda r:r["workloads"][0].update(name="raw_int8_count_star"),
                   lambda r:r["workloads"][0].update(rows=1000000),
                   lambda r:r.update(crashes=[{"error":"failed"}]),
                   lambda r:r["workloads"][0].pop("raw_integer_count_execution_proof"),
                   lambda r:r["workloads"][0]["raw_integer_count_execution_proof"].update(explain_analyze=plan(M.RAW[0],True).replace("ephemeral_fused","cached_reusable")),
                   lambda r:r["workloads"][0]["raw_integer_count_settings"][0]["settings"]["settings"].append(["pg_accel.cost_multiplier","0.1"]),
                   lambda r:r["workloads"][0]["iterations"][0].update(accel_ms=float("nan")),
                   lambda r:r["workloads"][0]["artifact_lifecycle_probe"].update(artifact_policy="cached_reusable")]
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                write(self.runs[0]/"report.json",original)
                self.mutate(mutation)
                self.assertEqual(self.check()["status"],"fail")

    def test_tamper_unsealed_report(self):
        write(self.runs[0]/"report.json","{}")
        self.assertEqual(self.check()["status"],"fail")

    def test_missing_report_seal_entry(self):
        run=self.runs[0]
        path=run/"evidence.sha256"
        write(path,"\n".join(line for line in path.read_text().splitlines() if not line.endswith("report.json"))+"\n")
        self.assertEqual(self.check()["status"],"fail")

    def test_build_link_substitution(self):
        write(self.runs[0]/"build-dir.txt",str(self.root/"build18-other"))
        self.reseal(self.runs[0])
        self.assertEqual(self.check()["status"],"fail")

    def test_failed_terminal(self):
        write(self.runs[0]/"terminal.log","terminal=1\n")
        self.reseal(self.runs[0])
        self.assertEqual(self.check()["status"],"fail")

    def test_below_floor_samples_not_summary_claim(self):
        def lose(report):
            w=report["workloads"][0]
            for index,p in enumerate(w["iterations"]):
                p["accel_ms"]=21
                w["artifact_steady_state_captures"][index]["iteration"]=copy.deepcopy(p)
            w["warm_summary"]=summary(w["iterations"])
            w["combined_warm_summary"]=summary([w["artifact_lifecycle_probe"]["iteration"]]+w["iterations"])
        self.mutate(lose)
        result=self.check()
        self.assertTrue(any("below 1.15" in error for error in result["errors"]))
        self.assertEqual(len(result["cells"]),22)

    def test_variance_flag_and_all_samples_retained(self):
        def jitter(report):
            w=report["workloads"][0]
            w["iterations"][0]["accel_ms"]=40
            w["artifact_steady_state_captures"][0]["iteration"]=copy.deepcopy(w["iterations"][0])
            w["warm_summary"]=summary(w["iterations"])
            w["combined_warm_summary"]=summary([w["artifact_lifecycle_probe"]["iteration"]]+w["iterations"])
        self.mutate(jitter)
        result=self.check()
        self.assertEqual(result["status"],"pass")
        self.assertTrue(result["cells"][0]["variance_flag"])

    def test_omitted_artifact_seal_entry(self):
        path=self.runs[0]/"evidence.sha256"
        write(path,"\n".join(line for line in path.read_text().splitlines() if "correctness_diffs" not in line)+"\n")
        self.assertEqual(self.check()["status"],"fail")

    def test_actual_lifecycle_verification_is_required_on_same_node(self):
        original=(self.runs[0]/"report.json").read_text()
        actual=plan(M.RAW[0],True)
        for invalid in (
            actual.replace("GPU Lifecycle Calls Verified: true", "GPU Lifecycle Calls Verified: false"),
            actual.replace("  GPU Lifecycle Calls Verified: true\n", ""),
            actual + "  GPU Lifecycle Calls Verified: false\n",
            actual.replace("  GPU Lifecycle Calls Verified: true\n", "") + "  -> Seq Scan on other\n    GPU Lifecycle Calls Verified: true\n",
        ):
            with self.subTest(actual=invalid):
                write(self.runs[0]/"report.json",original)
                self.mutate(lambda r: r["workloads"][0]["raw_integer_count_execution_proof"].update(explain_analyze=invalid))
                self.assertEqual(self.check()["status"],"fail")

    def test_module_same_basenames_from_wrong_prefix_are_rejected(self):
        files=[self.repo/"wrong-version"/n for n in ("pg_accel.dylib","postgres","pg_config")]
        for file in files:
            write(file,"synthetic substituted binary")
        manifest=self.build/"module.sha256"
        seal(manifest,files,self.repo)
        # Even self-consistent replacement receipts and run seals cannot turn
        # another installation into the qualified PostgreSQL 18.4 installation.
        for run in self.runs:
            write(run/"module-after.log","".join(line[66:]+": OK\n" for line in manifest.read_text().splitlines()))
            self.reseal(run)
        with self.assertRaisesRegex(ValueError,"wrong qualified PG18.4"):
            self.check()


if __name__ == "__main__":
    unittest.main()
