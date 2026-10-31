#!/usr/bin/env python3
"""Strict fixed-protocol verifier; no thresholds, dropping samples or retries."""
import json
import pathlib
import sys

FILE_LIMIT=4*1024*1024
TOTAL_LIMIT=512*1024*1024
ROW_LIMIT=32768

def unique_object(pairs):
    result={}
    for key,value in pairs:
        if key in result: raise ValueError('duplicate JSON field')
        result[key]=value
    return result

def where_population(rows):
    result=0
    for block in range((rows+16387)//16388):
        if block%4==0:continue
        low=block*16388+1;high=min(rows,(block+1)*16388)
        result+=high-low+1-(high//13-(low-1)//13)
    return result

def require(condition, message):
    if not condition:
        raise ValueError(message)

def integer(value):
    require(type(value) is int and value>=0, 'nonnegative integer required')
    return value

def canonical(rows, oid, tagged=False):
    require(type(rows) is list and len(rows)<=ROW_LIMIT,'bounded full rows required')
    result=[]
    for row in rows:
        require(type(row) is list and len(row)==2,'exact row pair')
        key,count=row
        if tagged:
            if key=='Null': key=None
            else:
                tag={21:'I16',23:'I32',20:'I64'}[oid]
                require(type(key) is dict and set(key)=={tag},'declared key type')
                key=key[tag]
        if key is not None:
            bits={21:16,23:32,20:64}[oid]
            require(type(key) is int and -(1<<(bits-1))<=key<(1<<(bits-1)),'signed key range')
        integer(count)
        require(count<(1<<63),'COUNT int8 range')
        result.append((key,count))
    # A sorted vector retains duplicates; never normalize into a key map.
    return sorted(result,key=lambda row:(row[0] is not None,row[0] or 0,row[1]))

def verify(directory):
    directory=pathlib.Path(directory)
    require(directory.is_dir() and not directory.is_symlink(),'real receipts directory')
    paths=sorted(directory.iterdir())
    require(len(paths)==442,'exact442 records including104 executions required')
    records=[]; total=0
    for ordinal,path in enumerate(paths):
        require(path.name==f'record-{ordinal:06}.json' and path.is_file() and not path.is_symlink(),'no missing/duplicate/unknown/alias record')
        size=path.stat().st_size;total+=size
        require(0<size<=FILE_LIMIT and total<=TOTAL_LIMIT,'byte bounds')
        with path.open('rb') as handle:content=handle.read(FILE_LIMIT+1)
        require(len(content)==size and len(content)<=FILE_LIMIT and content.endswith(b'\n'),'complete bounded newline-terminated record')
        record=json.loads(content,object_pairs_hook=unique_object,parse_constant=lambda value:(_ for _ in ()).throw(ValueError('nonfinite JSON value')))
        require(set(record)=={'ordinal','kind','payload'} and type(record['ordinal']) is int and record['ordinal']==ordinal,'record envelope')
        records.append(record)
    cursor=0
    def take(kind):
        nonlocal cursor
        require(cursor<len(records) and records[cursor]['kind']==kind,f'expected {kind} at{cursor}')
        payload=records[cursor]['payload'];cursor+=1
        return payload
    header=take('header')
    require(header['schema']==1 and header['scheduled_executions']==100 and header['jit_probes']==4,'frozen schedule')
    require((header['file_limit'],header['total_limit'],header['record_limit'],header['row_limit'])==(FILE_LIMIT,TOTAL_LIMIT,512,ROW_LIMIT),'frozen bounds')
    for fixture,oid,groups,masks in [('int2_count_star',21,16389,1),('int8_count_filter',20,16388,2)]:
        oracle_record=take('oracle');require(oracle_record['fixture']==fixture,'oracle fixture')
        oracle=canonical(oracle_record['rows'],oid,True)
        require(len(oracle)==groups and len({row[0] for row in oracle})==groups,'full actual group inventory')
        recipes=take('recipes');require(recipes['fixture']==fixture,'recipe fixture')
        require(all(recipes[key] for key in ('legacy_spec','raw_spec','legacy_projection','raw_projection','legacy_cost','raw_cost')),'independent complete recipes')
        default=take('default_plan');require(default['fixture']==fixture and default['lines'],'default plan retained separately')
        schedule=[({'fixture':fixture,'phase':'Jit','arm':arm},None) for arm in ('Legacy','Raw')]
        for phase,count in [('Warmup',5),('Warm',10),('FreshArtifact',10)]:
            for pair in range(count):
                arms=('Legacy','Raw') if pair%2==0 else ('Raw','Legacy')
                for position,arm in enumerate(arms):
                    context={'fixture':fixture,'phase':phase,'pair':pair,'position':position,'arm':arm}
                    schedule.append((context,phase))
        for sequence,(context,phase) in enumerate(schedule,1):
            require(take('begin')==context,'fixed arm/order/sample identity')
            if phase=='FreshArtifact' and context['arm']=='Legacy':
                invalidation=take('invalidation');require(invalidation['context']==context and integer(invalidation['bytes'])>0 and invalidation['identity'],'exact targeted invalidation')
            raw=take('raw');require(raw['context']==context and raw['sequence']==sequence,'raw sample identity')
            require(raw['key_type']==oid and raw['completed'] is True,'typed complete output')
            require(canonical(raw['rows'],oid)==oracle,'full multiset stock parity')
            calls=integer(raw['calls']);verified=integer(raw['verified_data_calls'])
            require(calls==integer(raw['planned'])==integer(raw['bridge_delta']) and 0<verified<=calls and raw['stock_delta']==0,'actual no-fallback lifecycle')
            require(integer(raw['preparation_ns'])+integer(raw['consumption_ns'])<=integer(raw['descriptor_executor_ns']),'descriptor interval')
            require(integer(raw['diagnostic_capture_ns'])<=raw['descriptor_executor_ns'],'included instrumentation interval')
            trace=raw['populations']
            if context['arm']=='Raw':
                require(raw['mode']=='parallel_hash' and 'ephemeral_fused' in raw['policy'],'actual raw fused mode')
                require(raw['retained_bytes_before']==raw['retained_bytes_after'],'zero retained Raw bytes')
                require(trace['actual_v2_calls']==verified==len(trace['capacities']) and all(type(cap) is int and cap>0 for cap in trace['capacities']),'actual v2 calls/capacities')
                leaves=trace['leaves'];require(type(leaves) is list and leaves,'successful leaves required')
                whole=integer(leaves[0]['whole']);input_rows=integer(leaves[0]['input'])
                require(1_000_017<=input_rows<=10_000_000,'fixed diagnostic input resource envelope')
                require(all(integer(leaf['whole'])==whole and integer(leaf['input'])==input_rows and integer(leaf['partition'])<=whole<=input_rows for leaf in leaves),'whole population identity')
                require(sum(leaf['partition'] for leaf in leaves)==whole,'checked successful partition conservation')
                require(whole==where_population(input_rows),'exact unchanged WHERE population')
                if fixture=='int2_count_star':require(sum(row[1] for row in oracle)==whole,'COUNTstar population')
                else:require(sum(row[1] for row in oracle)<=whole,'filtered COUNT population')
                require(calls==verified+masks*((input_rows+65535)//65536),'distinct mask conversions once')
            else:
                require(raw['mode'] in ('parallel_dense_count','parallel_dense_integer'),'actual Legacy mode')
                require(trace=={'actual_v2_calls':0,'leaves':[],'capacities':[]},'Legacy cannot be mislabeled v2')
            if phase is not None:
                computed=take('computed')
                require(all(computed[key]==context[key] for key in ('fixture','phase','pair','position','arm')) and computed['rows']==groups,'computed sample binding')
                evidence=computed['evidence']
                for field,raw_field in [('physical_mode','mode'),('observed_calls','calls'),('scheduled_calls','planned'),('bridge_delta','bridge_delta'),('verified_data_calls','verified_data_calls'),('preparation_ns','preparation_ns'),('consumption_ns','consumption_ns'),('descriptor_executor_ns','descriptor_executor_ns'),('diagnostic_capture_ns','diagnostic_capture_ns'),('construction_bytes','construction_bytes'),('built_artifact_preparation_us','built_artifact_preparation_us')]:
                    require(evidence[field]==raw[raw_field],'computed/raw receipt equality')
                require(evidence['explicit_capacity_override'] is None and evidence['completed'] is True,'no capacity forcing')
                require(evidence['verified_data_calls']+evidence['mask_calls']+evidence['control_calls']==calls,'data/mask/control conservation')
                if phase=='Warm' and context['arm']=='Legacy':require(evidence['artifact_cache_hit'] is True,'warm Legacy must hit')
                if phase=='FreshArtifact':require(evidence['artifact_cache_hit'] is False and evidence['resident_load_included'] is False,'fresh is not reload-inclusive')
                verdict=take('verdict')
                require(verdict=={'fixture':fixture,'phase':phase,'pair':context['pair'],'position':context['position'],'error':None},'complete success verdict')
            else:
                require(take('verdict')=={'context':context,'error':None},'separate JIT verdict')
        require(take('fixture_complete')=={'fixture':fixture},'fixture terminal')
    require(take('complete')=={'fixtures':2,'scheduled_executions':100,'jit_probes':4} and cursor==len(records),'whole-run terminal and exact inventory')
    return {'records':len(records),'bytes':total,'scheduled_executions':100,'jit_probes':4,'errors':[]}

if __name__=='__main__':
    print(json.dumps(verify(sys.argv[1]),sort_keys=True))
