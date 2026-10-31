"""Synthetic only: solver and fail-closed receipt contracts, not timing evidence."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC=importlib.util.spec_from_file_location("calibration",Path(__file__).with_name("dense-count-calibration.py"))
C=importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(C)


def write(path,value):
    path.write_text(json.dumps(value))
    return path


def call(n,g):
    facts=dict.fromkeys(C.FACTS,0)
    facts.update(rows=n,group_capacity=g+1,row_counter_updates=n,global_counter_clear_entries=g+1,
                 serial_reset_group_visits=g+1,serial_merge_group_visits=g+1,
                 serial_partial_counter_reads=g+1,serial_finalize_group_visits=g+1,
                 persistent_counter_lanes=1,key_columns=1,nullable_key_columns=1)
    return {"index":0,"flags":7,"status":"success","facts":facts,"structure":{
        "rows":n,"group_capacity":g+1,"key_type":23,"count_type":0,"nullable_value":False,
        "sql_mask":False,"mode":"parallel_dense_count","workspace_layout_id":"synthetic"}}


def sample(us,index=0,profile=None,limits=None):
    return {"backend_pid":42,"sequence":index,"settings":profile or {},"device_limits":limits or {},
            "actual_plan":"Custom Scan (GpuAccelAgg)\n  GPU Dispatched Physical Kernel Mode: parallel_dense_count\n  GPU Physical Kernel Mode Verified: true\n  GPU Planned Lifecycle Calls: 1\n  GPU Lifecycle Calls Verified: true",
            "before":{stage:{"calls":index,"elapsed_us":index*us} for stage in C.STAGES},
            "after":{stage:{"calls":index+1,"elapsed_us":(index+1)*us} for stage in C.STAGES}}


class SolverTests(unittest.TestCase):
    def design(self):
        return [[float(n+g+6),float(11*(g+1)),1.] for n in C.NS for g in C.GROUPS["train"]]

    def test_exact_positive_fit(self):
        x=self.design()
        result=C.nnls(x,[3*r[0]+2*r[1]+5 for r in x])
        self.assertAlmostEqual(result["a_us_per_cost"],3,places=8)
        self.assertAlmostEqual(result["b_us_per_serial_work"],2,places=8)
        self.assertAlmostEqual(result["serial_cost_per_work"],2/3,places=8)
        self.assertEqual(len(result["candidates"]),8)

    def test_zero_b_is_accepted_without_floor(self):
        x=self.design()
        result=C.nnls(x,[3*r[0]+5 for r in x])
        self.assertEqual(result["b_us_per_serial_work"],0)
        self.assertEqual(result["serial_cost_per_work"],0)

    def test_full_rank_required_even_when_subset_would_fit(self):
        with self.assertRaisesRegex(ValueError,"rank-deficient"):
            C.nnls([[float(i),2.*i,1.] for i in range(1,13)],[float(i) for i in range(1,13)])

    def test_zero_a_global_optimum_rejected(self):
        with self.assertRaisesRegex(ValueError,"a=0"):
            C.nnls(self.design(),[5.]*12)

    def test_nonfinite_and_incomplete_rejected(self):
        x=self.design()
        for y in ([1.]*11,[float("nan")]*12):
            with self.assertRaises(ValueError):
                C.nnls(x,y)

    def test_per_call_static_multiplicands_not_summed(self):
        first=call(100,64)
        first["flags"]=3
        first["facts"]["serial_finalize_group_visits"]=0
        final=call(0,64)
        final["index"]=1
        final["flags"]=4
        for key in ("global_counter_clear_entries","serial_reset_group_visits","serial_merge_group_visits","serial_partial_counter_reads"):
            final["facts"][key]=0
        work=C.trace_work([first,final],100)
        self.assertEqual(work["S"],65*11)
        self.assertEqual(work["L"],2)

    def test_impossible_star_facts_rejected_without_topology_thresholds(self):
        for changes in (
            {"row_counter_updates":0,"global_counter_clear_entries":0,"persistent_counter_lanes":0,"serial_reset_group_visits":1,"serial_merge_group_visits":1,"serial_finalize_group_visits":1,"value_null_checks":123,"sql_mask_checks":456},
            {"row_counter_updates":0},{"persistent_counter_lanes":0},{"value_null_checks":1},
            {"sql_mask_checks":1},{"serial_reset_group_visits":1},
            {"serial_merge_group_visits":1},{"serial_finalize_group_visits":1},
            {"global_counter_clear_entries":0},{"serial_partial_counter_reads":0},
            {"local_counter_init_entries":3,"partial_counter_writes":65},
        ):
            c=call(500017,64)
            c["facts"].update(changes)
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                C.trace_work([c],500017)

    def test_control_phase_cannot_invent_accumulate_work(self):
        first=call(100,64)
        first["flags"]=3
        first["facts"]["serial_finalize_group_visits"]=0
        final=call(0,64)
        final["index"]=1
        final["flags"]=4
        final["facts"]["serial_reset_group_visits"]=0
        final["facts"]["serial_merge_group_visits"]=0
        with self.assertRaisesRegex(ValueError,"control-only"):
            C.trace_work([first,final],100)


class ReceiptTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name).resolve()
        def entry(name):
            path=self.root/name
            path.write_text("synthetic evidence, not an actual review or binary")
            return {"path":name,"sha256":C.digest(path)}
        def document(name,value):
            return self.document(name,value)
        source=entry("source.txt")
        self.profile=dict.fromkeys(C.REQUIRED_SETTINGS,"default-observed")
        self.profile.update({"pg_accel.cost_multiplier":"1","pg_accel.min_batch_size":"65536","pg_accel.resident_memory_budget_mb":"-1",**{k:"on" for k in ("pg_accel.enabled","pg_accel.gpu_enabled","pg_accel.auto_load","pg_accel.execution_profiling")}})
        limit_values={"gpu_op_cost_reduce":"1","gpu_op_cost_filter":"1","gpu_hash_agg_max_groups":"100000"}
        stats=self.root/"stats.rs"
        stats.write_text('fn pg_accel_device_limits() { vec!['+','.join('("'+key+'".into(), value)' for key in limit_values)+']; TableIterator::new(rows) }')
        stats_entry={"path":"stats.rs","sha256":C.digest(stats)}
        self.limits={"values":limit_values,"sources":dict.fromkeys(limit_values,"synthetic")}
        self.fixtures={cell_id:{"key_type":23,"count_type":0,"seed":42,"setup_sql":[f"synthetic {cell_id}"],"query_sql":"SELECT k, count(*) FROM fixture GROUP BY k"} for role in C.GROUPS for cell_id in C.ids(role)}
        identities={"timed_binary":entry("binary.txt"),"companion_binary":entry("companion.txt"),
                    "observed_settings":document("settings.json",{"kind":"observed_settings","settings":self.profile}),
                    "device_limits":document("limits.json",{"kind":"device_limits",**self.limits,"inventory_source":stats_entry}),
                    "fixture_manifest":document("fixtures.json",{"kind":"fixture_manifest","cells":self.fixtures})}
        for role in ("timed_source","companion_source","harness"):
            identities[role]=document(role+".json",{"kind":role,"base_commit":C.CE24,"files":[source,stats_entry]})
        for variant in ("timed","companion"):
            identities[variant+"_build"]=document(variant+"-build.json",{"kind":variant+"_build","terminal":0,"variant":"ce24_uninstrumented" if variant=="timed" else "ce24_workfacts_companion","native_exports":[f"synthetic_export_{i}" for i in range(133)]+([] if variant=="timed" else [C.WORK_EXPORT]),"bindings":{k:identities[k]["sha256"] for k in (variant+"_binary",variant+"_source","harness")}})
        eq=document("equivalence.json",{"kind":"work_equivalence","bindings":{k:identities[k]["sha256"] for k in ("timed_binary","companion_binary","timed_source","companion_source","harness")},"analysis":entry("equivalence.txt")})
        self.m={"schema_version":1,"coefficients":{"reduce":1.,"filter":1.,"launch":5.},
                "training_ids":C.ids("train"),"holdout_ids":C.ids("holdout"),"existing_validation_ids":C.ids("existing_validation"),
                "identities":identities,
                "work_equivalence":{"artifact":eq,"reviews":[entry("review1.txt"),entry("review2.txt")]}}
        self.manifest=write(self.root/"manifest.json",self.m)
        self.paths=[]
        for n in C.NS:
            for g in C.GROUPS["train"]:
                self.paths.append(self.make_receipt(n,g,"train"))
        self.complete_run()

    def complete_run(self):
        cells={}
        for path in self.paths:
            data=C.read(path)
            cells[f"n{data['N']}-g{data['nominal_groups']}"]={"path":path.name,"sha256":C.digest(path)}
        write(self.root/"completion.json",{"kind":"training_run_complete","terminal":0,"manifest_sha256":C.digest(self.manifest),"cells":cells})

    def document(self,name,value):
        path=write(self.root/name,value)
        return {"path":name,"sha256":C.digest(path)}

    def reseal_acquisition(self,data,fit_sha=None):
        cell_id=f"n{data['N']}-g{data['nominal_groups']}"
        acquisition={"kind":"calibration_acquisition","terminal":0,"bindings":{k:v["sha256"] for k,v in self.m["identities"].items()},"manifest_sha256":C.digest(self.manifest),"role":data["role"],"cell_id":cell_id,"backend_pid":42,"started_at":data["acquisition_started_at"],"completed_at":data["acquisition_started_at"],"heldout_fit_sha256":fit_sha}
        acquisition.update({field+"_sha256":C.object_digest(data[field]) for field in ("trace_before","trace_after","warmups","samples","parity")})
        acquisition.update({"raw_trace_"+phase+"_sha256":data["raw_trace_"+phase]["sha256"] for phase in ("before","after")})
        data["acquisition_receipt"]=self.document(data["role"]+"-"+cell_id+"-acquisition.json",acquisition)

    def make_receipt(self,n,g,role,fit_sha=None):
        trace=[call(n,g)]
        raw={"call_index":0,"flags":7,"rows":n,"group_capacity":g+1,"status":0,"detail":0,"facts_status":0,"workspace_status":0,"uncertain_rows":0,"dim_count":0,"native_returned":True,"completed":True,"native_mode":2,"grouping_mode":0,"key_count":1,"measure_count":1,"count_op":4,"partition_present":False,"output_present":True,"emitted_groups":g+1,"keys":[{"tag":2,"source":0,"null_code":g}],"count_nullable":False,"where_mask_present":False,"measure_mask_present":False,"facts":{**trace[0]["facts"],"abi_version":1,"size_bytes":144,"reserved0":0,"reserved1":0}}
        layout={"workspace_bytes":4096,"workspace_alignment":64,"workspace_space":0,"scratch_bytes":0,"scratch_alignment":1,"scratch_space":0}
        raw.update(layout)
        trace[0]["raw_call"]=raw
        trace[0]["structure"]["workspace_layout_id"]=C.object_digest(layout)
        work=C.trace_work(trace,n)
        us=3*(work["P"]+5)+2*work["S"]+5
        data={"schema_version":1,"role":role,"N":n,"nominal_groups":g,"variant":"ce24_dense",
              "manifest_sha256":C.digest(self.manifest),"timed_binary_sha256":self.m["identities"]["timed_binary"]["sha256"],
              "acquisition_started_at":"2000-01-01T00:00:00Z" if role=="train" else "2099-01-01T00:00:00Z","backend_pid":42,"fixture":self.fixtures[f"n{n}-g{g}"],
              "trace_before":trace,"trace_after":copy.deepcopy(trace),"warmups":[sample(us,i,self.profile,self.limits) for i in range(5)],"samples":[sample(us,i,self.profile,self.limits) for i in range(5,15)],
              "parity":{"accel_minus_native":0,"native_minus_accel":0,"native_kernel_delta":0,"output_groups":g+1}}
        for phase in ("before","after"):
            data["raw_trace_"+phase]=self.document(f"{role}-n{n}-g{g}-raw-{phase}.json",{"schema":"dense-count-companion-trace-v1","untimed":True,"armed":True,"valid":True,"overflow":False,"expected_calls":1,"attempted_calls":1,"calls":[raw]})
        self.reseal_acquisition(data,fit_sha)
        return write(self.root/f"{role}-n{n}-g{g}.json",data)

    def test_complete_fit_and_separate_validation_no_refit(self):
        result=C.fit(self.root,self.manifest,self.paths)
        self.assertAlmostEqual(result["solution"]["serial_cost_per_work"],2/3,places=8)
        frozen=write(self.root/"fit.json",result)
        validation=[self.make_receipt(n,g,role,C.digest(frozen)) for role in ("holdout","existing_validation") for n in C.NS for g in C.GROUPS[role]]
        with patch.object(C,"nnls",side_effect=AssertionError("validation must never refit")):
            checked=C.validate(self.root,self.manifest,frozen,C.digest(frozen),validation)
        self.assertEqual(len(checked["cells"]),12)

    def test_missing_duplicate_and_holdout_training_fail(self):
        holdout=self.make_receipt(C.NS[0],256,"holdout")
        for paths in (self.paths[:-1],self.paths[:-1]+[self.paths[0]],self.paths[:-1]+[holdout]):
            with self.assertRaises(ValueError):
                C.fit(self.root,self.manifest,paths)

    def test_receipt_mutations_fail(self):
        original=C.read(self.paths[0])
        mutations=[lambda d:d["trace_after"][0]["facts"].update(group_capacity=99),
                   lambda d:d["samples"].pop(),lambda d:d["warmups"].pop(),
                   lambda d:d["samples"][0]["after"]["kernel_execution"].update(calls=52),
                   lambda d:d["samples"][0]["after"]["kernel_execution"].update(elapsed_us=0),
                   lambda d:d["samples"][0]["settings"].update({"pg_accel.cost_multiplier":"0.1"}),
                   lambda d:d["samples"][0].update(actual_plan=sample(1)["actual_plan"].replace("parallel_dense_count","parallel_hash")),
                   lambda d:d.update(timed_binary_sha256="bad"),lambda d:d["parity"].update(native_minus_accel=1)]
        for mutation in mutations:
            bad=copy.deepcopy(original)
            mutation(bad)
            self.reseal_acquisition(bad)
            write(self.paths[0],bad)
            self.complete_run()
            with self.assertRaises(ValueError):
                C.fit(self.root,self.manifest,self.paths)

    def test_hash_tamper_and_path_escape(self):
        (self.root/"source.txt").write_text("changed")
        with self.assertRaisesRegex(ValueError,"hash mismatch"):
            C.fit(self.root,self.manifest,self.paths)
        with self.assertRaisesRegex(ValueError,"escaping"):
            C.evidence({"path":"../outside","sha256":"bad"},self.root)

    def test_holdout_must_follow_frozen_fit_and_fit_hash(self):
        frozen=write(self.root/"fit.json",C.fit(self.root,self.manifest,self.paths))
        paths=[self.make_receipt(n,g,role,C.digest(frozen)) for role in ("holdout","existing_validation") for n in C.NS for g in C.GROUPS[role]]
        with self.assertRaisesRegex(ValueError,"substituted fit"):
            C.validate(self.root,self.manifest,frozen,"bad",paths)
        first=C.read(paths[0])
        first["acquisition_started_at"]="2000-01-01T00:00:00Z"
        self.reseal_acquisition(first,C.digest(frozen))
        write(paths[0],first)
        with self.assertRaisesRegex(ValueError,"before fit freeze"):
            C.validate(self.root,self.manifest,frozen,C.digest(frozen),paths)

    def test_replayed_and_overlapping_intervals_rejected_when_resealed(self):
        original=C.read(self.paths[0])
        for target,source in (("warmup",0),("measured",0),("measured",4)):
            bad=copy.deepcopy(original)
            if target=="warmup":
                bad["warmups"][1]["before"]=copy.deepcopy(bad["warmups"][source]["before"])
                bad["warmups"][1]["after"]=copy.deepcopy(bad["warmups"][source]["after"])
            else:
                bad["samples"][0]["before"]=copy.deepcopy(bad["warmups"][source]["before"])
                bad["samples"][0]["after"]=copy.deepcopy(bad["warmups"][source]["after"])
            self.reseal_acquisition(bad)
            write(self.paths[0],bad)
            self.complete_run()
            with self.assertRaisesRegex(ValueError,"overlapping/replayed"):
                C.fit(self.root,self.manifest,self.paths)

    def test_full_profile_budget_and_limits_cannot_drift(self):
        original=C.read(self.paths[0])
        for field,key,value in (("settings","pg_accel.resident_memory_budget_mb","1024"),("settings","max_parallel_workers_per_gather","0"),("settings","work_mem","4MB"),("settings","pg_accel.kernel_timeout_ms","10s"),("limits","gpu_hash_agg_max_groups","1")):
            bad=copy.deepcopy(original)
            if field=="settings":
                bad["samples"][0]["settings"][key]=value
            else:
                bad["samples"][0]["device_limits"]["values"][key]=value
            self.reseal_acquisition(bad)
            write(self.paths[0],bad)
            self.complete_run()
            with self.assertRaisesRegex(ValueError,"profile/budget/device limits"):
                C.fit(self.root,self.manifest,self.paths)

    def test_structured_acquisition_and_identity_roles_required(self):
        original=C.read(self.paths[0])
        for mutation in (lambda a:a.update(terminal=1),lambda a:a["bindings"].pop("companion_binary"),lambda a:a["bindings"].update(timed_binary="other"),lambda a:a.update(samples_sha256="wrong")):
            bad=copy.deepcopy(original)
            acquisition=C.read(self.root/original["acquisition_receipt"]["path"])
            mutation(acquisition)
            bad["acquisition_receipt"]=self.document("bad-acquisition.json",acquisition)
            write(self.paths[0],bad)
            self.complete_run()
            with self.assertRaises(ValueError):
                C.fit(self.root,self.manifest,self.paths)
        bad=copy.deepcopy(original)
        bad["acquisition_receipt"]=self.m["work_equivalence"]["reviews"][0]
        write(self.paths[0],bad)
        self.complete_run()
        with self.assertRaises(ValueError):
            C.fit(self.root,self.manifest,self.paths)
        missing=copy.deepcopy(self.m)
        missing["identities"].pop("companion_source")
        write(self.manifest,missing)
        with self.assertRaisesRegex(ValueError,"identity role"):
            C.fit(self.root,self.manifest,self.paths)

    def test_missing_device_inventory_entry_rejected(self):
        limits=C.read(self.root/"limits.json")
        limits["values"].pop("gpu_hash_agg_max_groups")
        self.m["identities"]["device_limits"]=self.document("limits.json",limits)
        write(self.manifest,self.m)
        with self.assertRaisesRegex(ValueError,"incomplete device-limit inventory"):
            C.fit(self.root,self.manifest,self.paths)

    def test_holdout_timestamp_cannot_replace_frozen_fit_binding(self):
        frozen=write(self.root/"fit.json",C.fit(self.root,self.manifest,self.paths))
        paths=[self.make_receipt(n,g,role) for role in ("holdout","existing_validation") for n in C.NS for g in C.GROUPS[role]]
        with self.assertRaisesRegex(ValueError,"did not bind frozen fit"):
            C.validate(self.root,self.manifest,frozen,C.digest(frozen),paths)

    def test_training_completion_and_binding_precede_single_freeze(self):
        original=C.read(self.paths[0])
        for future_start,fit_binding in ((False,None),(True,None),(False,"not-training")):
            bad=copy.deepcopy(original)
            if future_start:
                bad["acquisition_started_at"]="2099-01-01T00:00:00Z"
            self.reseal_acquisition(bad,fit_binding)
            acquisition=C.read(self.root/bad["acquisition_receipt"]["path"])
            if fit_binding is None:
                acquisition["completed_at"]="2099-01-01T00:00:00Z"
            bad["acquisition_receipt"]=self.document("future-acquisition.json",acquisition)
            write(self.paths[0],bad)
            self.complete_run()
            with self.assertRaisesRegex(ValueError,"incomplete at fit freeze|held-out fit binding"):
                C.fit(self.root,self.manifest,self.paths)
        write(self.paths[0],original)
        self.reseal_acquisition(original)
        write(self.paths[0],original)
        self.complete_run()
        fixed=C.instant("2026-10-26T00:00:00Z")
        with patch.object(C,"datetime") as clock:
            clock.now.return_value=fixed
            clock.fromisoformat.side_effect=__import__("datetime").datetime.fromisoformat
            result=C.fit(self.root,self.manifest,self.paths)
            clock.now.assert_called_once_with(C.timezone.utc)
        self.assertEqual(result["frozen_at"],fixed.isoformat())

    def test_raw_drain_mutations_rejected_even_when_resealed(self):
        original=C.read(self.paths[0])
        mutations=[lambda r:r.update(overflow=True),lambda r:r.update(attempted_calls=2),
                   lambda r:r["calls"][0].update(status=1),
                   lambda r:r["calls"][0].update(group_capacity=99),
                   lambda r:r["calls"][0]["facts"].update(row_counter_updates=0),
                   lambda r:r["calls"][0].update(workspace_bytes=999),
                   lambda r:r["calls"][0]["keys"][0].update(tag=3)]
        raw_original=C.read(self.root/original["raw_trace_before"]["path"])
        for mutation in mutations:
            bad=copy.deepcopy(original)
            raw=copy.deepcopy(raw_original)
            mutation(raw)
            for phase in ("before","after"):
                bad["raw_trace_"+phase]=self.document("bad-raw-"+phase+".json",raw)
                bad["trace_"+phase][0]["raw_call"]=copy.deepcopy(raw["calls"][0])
            self.reseal_acquisition(bad)
            write(self.paths[0],bad)
            self.complete_run()
            with self.assertRaises(ValueError):
                C.fit(self.root,self.manifest,self.paths)
        bad=copy.deepcopy(original)
        bad["trace_before"][0]["raw_call"].pop("count_op")
        bad["trace_after"]=copy.deepcopy(bad["trace_before"])
        self.reseal_acquisition(bad)
        write(self.paths[0],bad)
        self.complete_run()
        with self.assertRaisesRegex(ValueError,"structural fields"):
            C.fit(self.root,self.manifest,self.paths)

    def test_final_completion_is_required_before_receipts_or_solver(self):
        completion=self.root/"completion.json"
        original=C.read(completion)
        completion.unlink()
        mutations=[lambda d:d.update(terminal=1),lambda d:d.update(manifest_sha256="stale"),
                   lambda d:d["cells"].pop(C.ids("train")[0]),
                   lambda d:d["cells"].update(extra=next(iter(d["cells"].values()))),
                   lambda d:d["cells"][C.ids("train")[0]].update(sha256="changed"),
                   lambda d:d["cells"][C.ids("train")[0]].update(path=self.paths[1].name)]
        with patch.object(C,"receipt",side_effect=AssertionError("must gate receipts")),patch.object(C,"nnls",side_effect=AssertionError("must not fit")):
            with self.assertRaisesRegex(ValueError,"missing final run completion"):
                C.fit(self.root,self.manifest,self.paths)
            for mutation in mutations:
                bad=copy.deepcopy(original)
                mutation(bad)
                write(completion,bad)
                with self.assertRaises(ValueError):
                    C.fit(self.root,self.manifest,self.paths)
        write(completion,original)
        self.paths[0].write_text(self.paths[0].read_text()+"\n")
        with self.assertRaisesRegex(ValueError,"hash mismatch"):
            C.fit(self.root,self.manifest,self.paths)

    def test_wrong_lineage_and_export_inventory_rejected(self):
        for role,field,value in (("timed_source","base_commit","current-main"),
                                 ("companion_source","base_commit","current-main"),
                                 ("timed_build","variant","ce24_workfacts_companion"),
                                 ("companion_build","native_exports",["fake"]*134)):
            manifest=copy.deepcopy(self.m)
            document=C.read(self.root/manifest["identities"][role]["path"])
            document[field]=value
            manifest["identities"][role]=self.document("mutated-"+role+".json",document)
            write(self.manifest,manifest)
            with self.subTest(role=role),self.assertRaises(ValueError):
                C.manifest(self.manifest,self.root)


if __name__=="__main__":
    unittest.main()
