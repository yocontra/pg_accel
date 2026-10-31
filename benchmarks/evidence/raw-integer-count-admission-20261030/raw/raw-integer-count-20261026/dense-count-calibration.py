#!/usr/bin/env python3
"""Offline receipt validation/NNLS; never starts processes or database sessions.

schema_version=1 (premeasurement schema, supersedes the unapproved draft).
Manifest: coefficients {reduce,filter,launch}, training_ids,holdout_ids,
existing_validation_ids, identities mapping exactly IDENTITY_ROLES to
{path,sha256}; work_equivalence {artifact:{path,sha256},reviews:[refs...]}.
timed_binary/companion_binary identify the actual extension module, not the
benchmark executable. Source/harness inventories: {kind:role,files:[refs...]};
both source inventories also require base_commit=CE24.
Build receipts: {kind:timed_build|companion_build,terminal:0,bindings:{module,
source,harness role names -> sha256},variant:ce24_uninstrumented or
ce24_workfacts_companion,native_exports:[all 133 or 134 export names]}.
Fixtures: {kind:fixture_manifest,cells:
{cell_id:{key_type:23,count_type:0,seed:42,setup_sql:[...],query_sql:...}}}.
Settings: {kind:observed_settings,settings:full observed string map}.
Limits: {kind:device_limits,values:string map,sources:string map,
inventory_source:ref to timed source inventory's stats.rs}; names are read
from its pg_accel_device_limits API, never copied dispatch thresholds.
Equivalence artifact: {kind:work_equivalence,bindings:{timed_binary,
companion_binary,timed_source,companion_source,harness -> sha256},analysis:ref}.
Cell: role,N,nominal_groups,variant ('ce24_dense' or 'current'), manifest_sha256,
timed_binary_sha256, acquisition_started_at (UTC ISO), backend_pid,
trace_before/trace_after lists of calls; warmups(5),samples(10),parity,fixture,
acquisition_receipt:ref,raw_trace_before/after:refs to original companion drains.
Calls: index,flags,status='success',structure,facts(16 u64),raw_call (entire
unchanged original call object). Acquisition binds raw_trace_before_sha256
and raw_trace_after_sha256 as well as canonical object hashes. Layout identity
is object_digest of the six raw workspace/scratch bytes/alignment/space fields.
Structure: rows,group_capacity,key_type,count_type,nullable_value,sql_mask,
mode,workspace_layout_id. Samples: before/after cumulative stage dictionaries
{stage:{calls,elapsed_us}}, backend_pid,sequence(0..14),settings,device_limits
{values,sources},actual_plan. All seven STAGES and chronological nonoverlap are
required across warmups AND measured samples; gaps are retained, not subtracted.
Acquisition JSON: kind=calibration_acquisition,terminal=0,bindings of ALL identity
roles to hashes,manifest_sha256,role,cell_id,backend_pid,started_at,completed_at,
trace_before_sha256,trace_after_sha256,warmups_sha256,samples_sha256,parity_sha256,
heldout_fit_sha256 (null for training). Digests use sorted compact UTF-8 JSON.
An acquisition timestamp alone is not proof of chronology: its reviewed
wrapper receipt must bind the held-out fit hash before acquisition. Hashes
establish consistency, not attestation against rewriting evidence plus seals;
human equivalence reviews remain manual authority, not machine-proved semantics.
All evidence paths are relative to explicitly supplied --root; SHA256 checked.
Fit also requires sibling completion.json emitted by the reviewed wrapper only
after cleanup/after-seals succeed: {kind:training_run_complete,terminal:0,
manifest_sha256,cells:{each training ID:{path,sha256}}}. The exact twelve input
receipts must match before receipt validation or fitting. Completion is sealed
into the fit; it is consistency evidence, not independent attestation.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import itertools
import json
import math
from pathlib import Path
import re
import statistics
import sys

NS = (500017,1000017,2000017)
GROUPS = {"train":(64,1024,8192,32768),"holdout":(256,4096,65536),"existing_validation":(16388,)}
FACTS = ("rows","group_capacity","row_counter_updates","global_counter_clear_entries",
         "local_counter_init_entries","local_reduction_additions","partial_counter_writes",
         "serial_reset_group_visits","serial_merge_group_visits","serial_partial_counter_reads",
         "serial_finalize_group_visits","persistent_counter_lanes","key_columns",
         "nullable_key_columns","value_null_checks","sql_mask_checks")
STATIC = ("group_capacity","persistent_counter_lanes","key_columns","nullable_key_columns")
RANK_TOL = 1e-10
RSS_TOL = 1e-12
CE24 = "ce24baf79bf2ebc9b7f2c1189aaaec66ac4be2c9"
WORK_EXPORT = "pgaccel_grouped_agg_count_work_requirements_v1"
IDENTITY_ROLES = {"timed_binary","companion_binary","timed_source","companion_source","harness",
                  "fixture_manifest","device_limits","observed_settings","timed_build","companion_build"}
STAGES = {"plan_build","artifact_ensure","derived_input_binding","workspace_allocation","output_allocation","kernel_execution","tuple_materialization"}
REQUIRED_SETTINGS = {"pg_accel."+name for name in ("enabled","gpu_enabled","min_batch_size","kernel_timeout_ms","max_workers_total","resident_memory_budget_mb","auto_load","cost_multiplier","log_level","assert_dispatch","parallel_fused_count","planner_profiling","execution_profiling","otel_log_max_mb","otel_log_max_rotations","fp64_enabled","soft_fp64_cost_multiplier")}
REQUIRED_SETTINGS |= {"shared_buffers","work_mem","effective_cache_size","max_parallel_workers_per_gather","max_worker_processes","max_parallel_workers","maintenance_work_mem","jit","jit_above_cost","random_page_cost","effective_io_concurrency","track_io_timing","parallel_leader_participation"}


def need(value,message):
    if not value:
        raise ValueError(message)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream,"sha256").hexdigest()


def object_digest(value):
    return hashlib.sha256(json.dumps(value,sort_keys=True,separators=(",",":"),allow_nan=False).encode()).hexdigest()


def read(path):
    def object_pairs(items):
        result={}
        for key,value in items:
            need(key not in result,"duplicate JSON key")
            result[key]=value
        return result
    def bad(value):
        raise ValueError(f"nonfinite JSON constant {value}")
    return json.loads(path.read_text(),object_pairs_hook=object_pairs,parse_constant=bad)


def finite(value,positive=False):
    return type(value) in (int,float) and math.isfinite(value) and (value>0 if positive else value>=0)


def evidence(item,root):
    name=Path(item["path"])
    need(not name.is_absolute(),"absolute evidence path")
    path=(root/name).resolve()
    need(path.is_relative_to(root.resolve()) and path.is_file(),"missing/escaping evidence path")
    need(digest(path)==item["sha256"],f"evidence hash mismatch: {name}")
    return path


def ids(role):
    return [f"n{n}-g{g}" for n,g in itertools.product(NS,GROUPS[role])]


def instant(value):
    result=datetime.fromisoformat(value.replace("Z","+00:00"))
    need(result.tzinfo is not None and result.utcoffset().total_seconds()==0,"timestamp must be UTC")
    return result


def manifest(path,root):
    need(path.resolve().is_relative_to(root.resolve()),"manifest outside root")
    data=read(path)
    need(data["schema_version"]==1,"manifest schema")
    for role,key in (("train","training_ids"),("holdout","holdout_ids"),("existing_validation","existing_validation_ids")):
        need(data[key]==ids(role),f"changed {role} cells/order")
    need(set(data["coefficients"])=={"reduce","filter","launch"},"coefficient fields")
    need(all(finite(v,True) for v in data["coefficients"].values()),"invalid cost units")
    need(data["coefficients"]["launch"]==5.0,"changed sealed launch cost requires review")
    identities=data["identities"]
    need(set(identities)==IDENTITY_ROLES,"missing/unknown identity role")
    need(len({item["path"] for item in identities.values()})==len(IDENTITY_ROLES),"identity roles alias same evidence")
    files={role:evidence(item,root) for role,item in identities.items()}
    hashes={role:item["sha256"] for role,item in identities.items()}
    for role in ("timed_source","companion_source","harness"):
        inventory=read(files[role])
        need(inventory["kind"]==role and inventory["files"],"wrong source/harness inventory kind")
        need(len({f["path"] for f in inventory["files"]})==len(inventory["files"]),"duplicate inventory input")
        for entry in inventory["files"]:
            evidence(entry,root)
        if role!="harness":
            need(inventory["base_commit"]==CE24,"source is not approved ce24 lineage")
    builds={}
    for variant in ("timed","companion"):
        build=read(files[variant+"_build"])
        expected={role:hashes[role] for role in (variant+"_binary",variant+"_source","harness")}
        need(build["kind"]==variant+"_build" and type(build["terminal"]) is int and build["terminal"]==0 and build["bindings"]==expected,"failed/substituted build receipt")
        need(build["variant"]==("ce24_uninstrumented" if variant=="timed" else "ce24_workfacts_companion"),"current/diagnostic module substituted for old timing")
        exports=build["native_exports"]
        need(len(exports)==len(set(exports))==(133 if variant=="timed" else 134),"wrong native export inventory")
        builds[variant]=build
    need(WORK_EXPORT not in builds["timed"]["native_exports"] and set(builds["companion"]["native_exports"])==set(builds["timed"]["native_exports"])|{WORK_EXPORT},"companion export delta is not additive query only")
    profile=read(files["observed_settings"])
    need(profile["kind"]=="observed_settings" and REQUIRED_SETTINGS<=set(profile["settings"]),"incomplete observed profile/budget")
    values=profile["settings"]
    need(values["pg_accel.cost_multiplier"]=="1" and values["pg_accel.min_batch_size"]=="65536" and values["pg_accel.resident_memory_budget_mb"]=="-1" and all(values[k]=="on" for k in ("pg_accel.enabled","pg_accel.gpu_enabled","pg_accel.auto_load","pg_accel.execution_profiling")),"nondefault admission/budget")
    limits=read(files["device_limits"])
    inventory_source=limits["inventory_source"]
    need(inventory_source in read(files["timed_source"])["files"],"device inventory not bound to timed source")
    source=evidence(inventory_source,root).read_text()
    need(source.count("fn pg_accel_device_limits()") == 1,"ambiguous device-limit API source")
    function=source.split("fn pg_accel_device_limits()",1)[1].split("TableIterator::new",1)[0]
    names=re.findall(r'"([a-z0-9_]+)"\.into\(\)',function)
    need(names and len(set(names))==len(names),"unsupported device inventory source syntax")
    need(limits["kind"]=="device_limits" and set(limits["values"])==set(limits["sources"])==set(names),"incomplete device-limit inventory")
    need(all(isinstance(v,str) and (v in ("true","false") or finite(float(v))) for v in limits["values"].values()) and all(isinstance(s,str) and s for s in limits["sources"].values()),"invalid device limits/source")
    need(float(limits["values"]["gpu_op_cost_reduce"])==data["coefficients"]["reduce"] and float(limits["values"]["gpu_op_cost_filter"])==data["coefficients"]["filter"],"price/observed limit mismatch")
    fixtures=read(files["fixture_manifest"])
    need(fixtures["kind"]=="fixture_manifest" and set(fixtures["cells"])==set(ids("train")+ids("holdout")+ids("existing_validation")),"incomplete fixture inventory")
    for cell_id,fixture in fixtures["cells"].items():
        need(fixture["key_type"]==23 and fixture["count_type"]==0 and fixture["seed"]==42 and fixture["setup_sql"] and fixture["query_sql"],"wrong fixture identity/SQL")
    equivalence=data["work_equivalence"]
    proof=read(evidence(equivalence["artifact"],root))
    need(proof["kind"]=="work_equivalence" and proof["bindings"]=={k:hashes[k] for k in ("timed_binary","companion_binary","timed_source","companion_source","harness")},"equivalence identity mismatch")
    evidence(proof["analysis"],root)
    need(len(equivalence["reviews"])>=2,"two independent work-equivalence reviews required")
    need(len({r["path"] for r in equivalence["reviews"]})==len(equivalence["reviews"]),"duplicate review artifact")
    for review in equivalence["reviews"]:
        evidence(review,root)
    data["_settings"]=values
    data["_limits"]={"values":limits["values"],"sources":limits["sources"]}
    data["_fixtures"]=fixtures["cells"]
    return data


def trace_work(calls,n):
    need(calls and [c["index"] for c in calls]==list(range(len(calls))),"trace incomplete/order")
    p=q=s=rows=0
    first_static=None
    reset=finalize=0
    for call in calls:
        facts=call["facts"]
        shape=call["structure"]
        flags=call["flags"]
        need(call["status"]=="success" and type(flags) is int and 1<=flags<=7,"unsuccessful/invalid phase")
        need(set(facts)==set(FACTS) and all(type(v) is int and 0<=v<=2**64-1 for v in facts.values()),"malformed native facts")
        need(shape["rows"]==facts["rows"] and shape["group_capacity"]==facts["group_capacity"],"descriptor/fact disagreement")
        need(shape["key_type"]==23 and shape["count_type"]==0 and shape["nullable_value"] is False and shape["sql_mask"] is False,"not INT4 COUNTstar calibration")
        need(shape["mode"]=="parallel_dense_count" and shape["workspace_layout_id"],"not observed dense layout")
        static=tuple(facts[k] for k in STATIC)
        need(first_static is None or static==first_static,"capacity/lane/key drift across calls")
        first_static=static
        need(facts["group_capacity"]>0 and facts["key_columns"]==1 and facts["nullable_key_columns"]==1,"wrong key metadata")
        if flags & 1:
            reset+=1
            need(call["index"]==0,"late/reset repeated")
        if flags & 4:
            finalize+=1
            need(call["index"]==len(calls)-1,"early finalize")
        capacity=facts["group_capacity"]
        need(facts["persistent_counter_lanes"]==1,"COUNTstar must have one persistent lane")
        need(facts["value_null_checks"]==facts["sql_mask_checks"]==0,"unfiltered COUNTstar has no nullable-value/mask checks")
        need(facts["serial_reset_group_visits"]==(capacity if flags&1 else 0),"RESET work mismatch")
        need(facts["serial_finalize_group_visits"]==(capacity if flags&4 else 0),"FINALIZE work mismatch")
        need(facts["serial_merge_group_visits"]==(capacity if flags&2 else 0),"ACCUMULATE work mismatch")
        if flags & 2:
            rows+=facts["rows"]
            need(facts["row_counter_updates"]==facts["rows"],"COUNTstar row updates must equal all input rows")
            clears=facts["global_counter_clear_entries"]
            need(clears>=capacity and clears%capacity==0 and facts["serial_partial_counter_reads"]==clears,"invalid clear/read population")
            local=facts["local_counter_init_entries"]
            writes=facts["partial_counter_writes"]
            if facts["rows"]==0:
                need(clears==capacity and local==writes==facts["local_reduction_additions"]==0,"empty accumulate work mismatch")
            elif local:
                need(writes==clears and local>=writes and local%writes==0 and facts["local_reduction_additions"]==local-writes,"inconsistent hierarchical facts")
            else:
                need(clears==capacity and writes==facts["local_reduction_additions"]==0,"inconsistent nonhierarchical facts")
        else:
            need(facts["rows"]==0,"control call has rows")
            need(all(facts[k]==0 for k in FACTS[2:7]+("serial_partial_counter_reads",)),"control-only call contains accumulate work")
        p+=sum(facts[k] for k in FACTS[2:7])
        q+=facts["value_null_checks"]+facts["sql_mask_checks"]
        s+=facts["serial_reset_group_visits"]*(1+facts["persistent_counter_lanes"])
        s+=facts["serial_merge_group_visits"]*(2+facts["persistent_counter_lanes"])+facts["serial_partial_counter_reads"]
        s+=facts["serial_finalize_group_visits"]*(1+3*facts["key_columns"]+facts["nullable_key_columns"])
    need(reset==finalize==1 and rows==n,"incomplete lifecycle/input population")
    return {"P":p,"Q":q,"S":s,"L":len(calls),"actual_capacity":first_static[0]}


def raw_trace_binding(reference,canonical,root,output_groups):
    raw=read(evidence(reference,root))
    need(raw["schema"]=="dense-count-companion-trace-v1" and raw["untimed"] is True and raw["armed"] is True and raw["valid"] is True and raw["overflow"] is False,"invalid raw companion envelope")
    need(raw["expected_calls"]==raw["attempted_calls"]==len(raw["calls"])==len(canonical),"raw/canonical call inventory mismatch")
    for index,(source,call) in enumerate(zip(raw["calls"],canonical)):
        need(source==call["raw_call"],"raw trace structural fields dropped/changed")
        need(source["call_index"]==call["index"]==index and source["flags"]==call["flags"],"phase/index adapter mismatch")
        need(all(source[k]==0 for k in ("status","detail","facts_status","workspace_status","uncertain_rows","dim_count")) and source["native_returned"] is True and source["completed"] is True,"raw companion call failed")
        need(source["native_mode"]==2 and source["grouping_mode"]==0 and source["key_count"]==source["measure_count"]==1 and source["count_op"]==4 and source["partition_present"] is False,"wrong raw count shape/mode")
        need(source["output_present"]==(index==len(canonical)-1),"raw output phase mismatch")
        if source["output_present"]:
            need(source["emitted_groups"]==output_groups,"raw output cardinality mismatch")
        facts=source["facts"]
        need(facts["abi_version"]==1 and facts["size_bytes"]==144 and facts["reserved0"]==facts["reserved1"]==0,"raw facts envelope mismatch")
        need({key:facts[key] for key in FACTS}==call["facts"],"native facts changed by adapter")
        shape=call["structure"]
        need(source["rows"]==shape["rows"] and source["group_capacity"]==shape["group_capacity"],"native capacity/rows changed by adapter")
        need(source["keys"][0]["tag"]==2 and source["keys"][0]["source"]==0 and source["keys"][0]["null_code"]!=-2147483648,"not nullable fact INT32 key representation")
        need(source["count_nullable"] is False and source["where_mask_present"] is False and source["measure_mask_present"] is False,"COUNTstar unexpectedly reads nullable value/mask")
        layout={key:source[key] for key in ("workspace_bytes","workspace_alignment","workspace_space","scratch_bytes","scratch_alignment","scratch_space")}
        need(shape["workspace_layout_id"]==object_digest(layout),"layout identity not derived from raw layout")


def sample_us(sample,backend,calls,profile,limits):
    need(sample["backend_pid"]==backend,"stage backend changed")
    values=sample["settings"]
    need(values==profile and sample["device_limits"]==limits,"observed profile/budget/device limits drift")
    need(values["pg_accel.cost_multiplier"]=="1" and values["pg_accel.min_batch_size"]=="65536" and all(values[k]=="on" for k in ("pg_accel.enabled","pg_accel.gpu_enabled","pg_accel.execution_profiling")),"nondefault admission/profiling")
    plan=sample["actual_plan"]
    need(plan.count("Custom Scan (GpuAccelAgg)")==1,"missing/ambiguous actual node")
    lines=plan.splitlines()
    start=next(i for i,line in enumerate(lines) if "Custom Scan (GpuAccelAgg)" in line)
    indent=lambda line:len(line)-len(line.lstrip(" ->"))
    depth=indent(lines[start])
    body=[]
    for line in lines[start+1:]:
        if not line.strip():
            continue
        if indent(line)<=depth:
            break
        body.append(line)
    need(body,"empty actual plan")
    direct=min(map(indent,body))
    properties={}
    for line in body:
        if indent(line)==direct and ":" in line:
            key,value=line.strip().split(":",1)
            need(key not in properties,"duplicate actual plan property")
            properties[key]=value.strip()
    need(properties.get("GPU Dispatched Physical Kernel Mode")=="parallel_dense_count" and properties.get("GPU Physical Kernel Mode Verified")=="true" and properties.get("GPU Lifecycle Calls Verified")=="true" and properties.get("GPU Planned Lifecycle Calls")==str(calls),"not actual dense lifecycle mode")
    before,after=sample["before"],sample["after"]
    need(set(before)==set(after)==STAGES,"stage inventory mismatch")
    deltas={}
    for stage in before:
        for key in ("calls","elapsed_us"):
            a,b=before[stage][key],after[stage][key]
            need(type(a) is int and type(b) is int and 0<=a<=b,"invalid/regressed stage counter")
        deltas[stage]={k:after[stage][k]-before[stage][k] for k in ("calls","elapsed_us")}
    kernel=deltas["kernel_execution"]
    need(kernel["calls"]==calls and kernel["elapsed_us"]>0,"stage/native lifecycle call mismatch")
    return kernel["elapsed_us"]


def receipt(path,root,m,mhash,role):
    need(path.resolve().is_relative_to(root.resolve()),"receipt outside root")
    data=read(path)
    need(data["schema_version"]==1 and data["role"]==role,"receipt role/schema")
    n,g=data["N"],data["nominal_groups"]
    need(n in NS and g in GROUPS[role],"undeclared cell")
    need(data["variant"]=="ce24_dense","training/validation requires old dense timing; current comparisons are separate")
    need(data["manifest_sha256"]==mhash and data["timed_binary_sha256"]==m["identities"]["timed_binary"]["sha256"],"receipt identity mismatch")
    acquisition=read(evidence(data["acquisition_receipt"],root))
    need(acquisition["kind"]=="calibration_acquisition" and type(acquisition["terminal"]) is int and acquisition["terminal"]==0,"failed/non-acquisition receipt")
    need(acquisition["bindings"]=={k:v["sha256"] for k,v in m["identities"].items()} and acquisition["manifest_sha256"]==mhash,"acquisition identities not bound to frozen manifest")
    need((acquisition["role"],acquisition["cell_id"],acquisition["backend_pid"])==(role,f"n{n}-g{g}",data["backend_pid"]),"acquisition cell/backend substitution")
    need(acquisition["started_at"]==data["acquisition_started_at"] and instant(acquisition["started_at"])<=instant(acquisition["completed_at"]),"acquisition clock/order mismatch")
    for field in ("trace_before","trace_after","warmups","samples","parity"):
        need(acquisition[field+"_sha256"]==object_digest(data[field]),"acquisition evidence digest mismatch")
    need(data["fixture"]==m["_fixtures"][f"n{n}-g{g}"],"fixture/SQL drift")
    parity=data["parity"]
    need(parity["accel_minus_native"]==parity["native_minus_accel"]==parity["native_kernel_delta"]==0 and parity["output_groups"]==g+1,"parity/cardinality failed")
    need(data["trace_before"]==data["trace_after"],"companion trace drift")
    for phase in ("before","after"):
        reference=data["raw_trace_"+phase]
        need(acquisition["raw_trace_"+phase+"_sha256"]==reference["sha256"],"raw drain not bound to acquisition")
        raw_trace_binding(reference,data["trace_"+phase],root,g+1)
    work=trace_work(data["trace_before"],n)
    need(len(data["warmups"])==5 and len(data["samples"])==10,"incomplete retained samples")
    backend=data["backend_pid"]
    need(type(backend) is int and backend>0,"invalid backend")
    all_samples=data["warmups"]+data["samples"]
    previous=None
    durations=[]
    for index,sample in enumerate(all_samples):
        need(sample["sequence"]==index,"sample sequence duplicate/gap")
        durations.append(sample_us(sample,backend,work["L"],m["_settings"],m["_limits"]))
        if previous is not None:
            need(all(sample["before"][stage][key]>=previous[stage][key] for stage in STAGES for key in ("calls","elapsed_us")),"overlapping/replayed/regressed cumulative interval")
        previous=sample["after"]
    warm,times=durations[:5],durations[5:]
    c=m["coefficients"]
    x=c["reduce"]*work["P"]+c["filter"]*work["Q"]+c["launch"]*work["L"]
    need(finite(x,True),"invalid work product")
    return {"id":f"n{n}-g{g}","role":role,"path":str(path.relative_to(root)),"sha256":digest(path),"acquisition_started_at":data["acquisition_started_at"],"acquisition_completed_at":acquisition["completed_at"],"acquisition_receipt":data["acquisition_receipt"],"heldout_fit_sha256":acquisition["heldout_fit_sha256"],"features":[x,work["S"],1.0],"work":work,"warmup_us":warm,"samples_us":times,"median_us":statistics.median(times),"cv_pct":statistics.stdev(times)/statistics.mean(times)*100}


def qr_solve(columns,y):
    """Unit-column two-pass MGS, fixed order; rank failure is not suppressed."""
    norms=[math.sqrt(sum(v*v for v in col)) for col in columns]
    need(all(finite(v,True) for v in norms),"zero/nonfinite design column")
    q=[]
    r=[[0.0]*len(columns) for _ in columns]
    for j,col in enumerate(columns):
        v=[x/norms[j] for x in col]
        for _ in range(2):
            for i,qi in enumerate(q):
                product=sum(a*b for a,b in zip(qi,v))
                r[i][j]+=product
                v=[a-product*b for a,b in zip(v,qi)]
        r[j][j]=math.sqrt(sum(a*a for a in v))
        need(r[j][j]>RANK_TOL,"rank-deficient complete/subset design")
        q.append([a/r[j][j] for a in v])
    z=[sum(a*b for a,b in zip(qi,y)) for qi in q]
    for i in reversed(range(len(z))):
        z[i]=(z[i]-sum(r[i][j]*z[j] for j in range(i+1,len(z))))/r[i][i]
    return [v/norm for v,norm in zip(z,norms)]


def nnls(design,y):
    need(len(design)==len(y)==12 and all(len(row)==3 for row in design),"fit requires full twelve-by-three design")
    need(all(finite(v) for row in design for v in row) and all(finite(v,True) for v in y),"nonfinite fit data")
    columns=[list(col) for col in zip(*design)]
    qr_solve(columns,y)  # Full rank before ANY active subset.
    candidates=[]
    for mask in range(8):
        active=[i for i in range(3) if mask & (1<<i)]
        solved=qr_solve([columns[i] for i in active],y) if active else []
        beta=[0.0]*3
        for i,v in zip(active,solved):
            beta[i]=v
        if not all(finite(v) for v in beta):
            candidates.append({"mask":mask,"eligible":False,"coefficients":beta})
            continue
        residuals=[actual-sum(a*b for a,b in zip(row,beta)) for row,actual in zip(design,y)]
        rss=sum(v*v for v in residuals)
        need(math.isfinite(rss),"RSS overflow")
        candidates.append({"mask":mask,"eligible":True,"coefficients":beta,"rss":rss,"residuals":residuals})
    best_rss=min(c["rss"] for c in candidates if c["eligible"])
    best=next(c for c in candidates if c["eligible"] and c["rss"]<=best_rss+RSS_TOL*max(1,best_rss))
    a,b,c=best["coefficients"]
    need(a>0,"global NNLS optimum has a=0")
    return {"rank":3,"rank_tolerance":RANK_TOL,"rss_tie_tolerance":RSS_TOL,"candidates":candidates,"selected_mask":best["mask"],"a_us_per_cost":a,"b_us_per_serial_work":b,"c_us":c,"serial_cost_per_work":b/a}


def fit(root,manifest_path,paths):
    frozen_at=datetime.now(timezone.utc)
    m=manifest(manifest_path,root)
    parents={p.resolve().parent for p in paths}
    need(len(parents)==1,"training receipts span multiple runs")
    run=parents.pop()
    need(run.is_relative_to(root.resolve()),"training run outside evidence root")
    completion_path=run/"completion.json"
    need(completion_path.is_file(),"missing final run completion")
    completion=read(completion_path)
    need(completion["kind"]=="training_run_complete" and type(completion["terminal"]) is int and completion["terminal"]==0,"failed final run completion")
    need(completion["manifest_sha256"]==digest(manifest_path),"stale completion manifest")
    sealed=completion["cells"]
    need(set(sealed)==set(ids("train")) and len(paths)==12,"completion training inventory mismatch")
    sealed_paths=[evidence(reference,root).resolve() for reference in sealed.values()]
    need(len(set(sealed_paths))==12 and set(sealed_paths)=={p.resolve() for p in paths},"completion supplied receipt mismatch")
    completion_ref={"path":str(completion_path.relative_to(root)),"sha256":digest(completion_path)}
    cells=[receipt(p,root,m,digest(manifest_path),"train") for p in paths]
    need(len(cells)==12 and sorted(c["id"] for c in cells)==sorted(ids("train")),"missing/duplicate training cells")
    need(sealed=={c["id"]:{"path":c["path"],"sha256":c["sha256"]} for c in cells},"completion cell identity mismatch")
    need(all(instant(c["acquisition_completed_at"])<=frozen_at for c in cells),"training acquisition incomplete at fit freeze")
    need(all(c["heldout_fit_sha256"] is None for c in cells),"training acquisition has held-out fit binding")
    cells.sort(key=lambda c:ids("train").index(c["id"]))
    solution=nnls([c["features"] for c in cells],[c["median_us"] for c in cells])
    return {"schema_version":1,"kind":"training_fit","manifest_sha256":digest(manifest_path),"solver_sha256":digest(Path(__file__)),"frozen_at":frozen_at.isoformat(),"run_completion":completion_ref,"cells":cells,"solution":solution,"status":"diagnostic_coefficient_only_not_production_approval"}


def validate(root,manifest_path,fit_path,fit_sha,paths):
    need(fit_path.resolve().is_relative_to(root.resolve()) and digest(fit_path)==fit_sha,"unfrozen/substituted fit")
    frozen=read(fit_path)
    m=manifest(manifest_path,root)
    need(frozen["kind"]=="training_fit" and frozen["manifest_sha256"]==digest(manifest_path) and frozen["solver_sha256"]==digest(Path(__file__)),"fit provenance mismatch")
    solution=frozen["solution"]
    beta=[solution[k] for k in ("a_us_per_cost","b_us_per_serial_work","c_us")]
    need(finite(beta[0],True) and all(finite(v) for v in beta[1:]),"invalid frozen solution")
    cells=[]
    for path in paths:
        need(path.resolve().is_relative_to(root.resolve()),"validation receipt outside root")
        role=read(path)["role"]
        need(role in ("holdout","existing_validation"),"training data passed to validation")
        c=receipt(path,root,m,digest(manifest_path),role)
        need(c["heldout_fit_sha256"]==fit_sha,"acquisition did not bind frozen fit before holdout")
        need(instant(c["acquisition_started_at"])>instant(frozen["frozen_at"]),"held-out response acquired before fit freeze")
        c["predicted_us"]=sum(a*b for a,b in zip(c["features"],beta))
        c["residual_us"]=c["median_us"]-c["predicted_us"]
        cells.append(c)
    need(len(cells)==12 and sorted(c["id"] for c in cells)==sorted(ids("holdout")+ids("existing_validation")),"missing/duplicate validation cells")
    return {"schema_version":1,"kind":"frozen_fit_validation_no_refit","fit_sha256":fit_sha,"cells":cells,"status":"diagnostic_residuals_only_strategy_order_requires_distinct_observed_modes"}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command",choices=("fit","validate"))
    parser.add_argument("--root",type=Path,required=True)
    parser.add_argument("--manifest",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--fit",type=Path)
    parser.add_argument("--fit-sha256")
    parser.add_argument("receipts",type=Path,nargs="+")
    args=parser.parse_args()
    try:
        root=args.root.resolve()
        need(args.output.resolve().is_relative_to(root),"output outside explicit root")
        need(args.command=="fit" or (args.fit and args.fit_sha256),"validation needs reviewed fit hash")
        result=fit(root,args.manifest,args.receipts) if args.command=="fit" else validate(root,args.manifest,args.fit,args.fit_sha256,args.receipts)
        encoded=json.dumps(result,indent=2,allow_nan=False)+"\n"
        with args.output.open("x") as stream:
            stream.write(encoded)
        print(json.dumps({"output":str(args.output),"sha256":digest(args.output)}))
        return 0
    except (ValueError,KeyError,TypeError,OSError,IndexError,OverflowError) as error:
        print(json.dumps({"status":"blocked","error":str(error)}),file=sys.stderr)
        return 1


if __name__=="__main__":
    sys.exit(main())
