#!/usr/bin/env python3
"""Synthetic protocol tests only: no PostgreSQL or GPU execution."""
import copy
import importlib.util
import json
import pathlib
import unittest

BASE=pathlib.Path(__file__).resolve().parent
SPEC=importlib.util.spec_from_file_location('checker',BASE/'check-paired.py')
CHECK=importlib.util.module_from_spec(SPEC);SPEC.loader.exec_module(CHECK)

def synthetic_records():
    records=[]
    def add(kind,payload):records.append({'ordinal':len(records),'kind':kind,'payload':payload})
    add('header',{'schema':1,'scheduled_executions':100,'jit_probes':4,'file_limit':CHECK.FILE_LIMIT,'total_limit':CHECK.TOTAL_LIMIT,'record_limit':512,'row_limit':CHECK.ROW_LIMIT,'synthetic':True})
    for fixture,oid,groups,masks in [('int2_count_star',21,16389,1),('int8_count_filter',20,16388,2)]:
        whole=CHECK.where_population(1000017)
        keys=[None]+[index*3-24582 for index in range(16388)] if oid==21 else [index*1000000-8194000000 for index in range(16388)]
        rows=[[key,1] for key in keys]
        if oid==21:rows[0][1]=whole-(groups-1)
        tagged=[['Null' if key is None else {{21:'I16',20:'I64'}[oid]:key},count] for key,count in rows]
        add('oracle',{'fixture':fixture,'rows':tagged})
        add('recipes',{'fixture':fixture,**{key:[1] for key in ('legacy_spec','raw_spec','legacy_projection','raw_projection','legacy_cost','raw_cost')}})
        add('default_plan',{'fixture':fixture,'lines':['SYNTHETIC PROTOCOL FIXTURE, NOT A QUERY PLAN']})
        schedule=[({'fixture':fixture,'phase':'Jit','arm':arm},None) for arm in ('Legacy','Raw')]
        for phase,count in [('Warmup',5),('Warm',10),('FreshArtifact',10)]:
            for pair in range(count):
                for position,arm in enumerate(('Legacy','Raw') if pair%2==0 else ('Raw','Legacy')):
                    schedule.append(({'fixture':fixture,'phase':phase,'pair':pair,'position':position,'arm':arm},phase))
        for sequence,(context,phase) in enumerate(schedule,1):
            raw_arm=context['arm']=='Raw';verified=1;calls=1+16*masks if raw_arm else 3
            mode='parallel_hash' if raw_arm else 'parallel_dense_count'
            trace={'actual_v2_calls':1,'leaves':[{'whole':whole,'partition':whole,'input':1000017}],'capacities':[32768]} if raw_arm else {'actual_v2_calls':0,'leaves':[],'capacities':[]}
            add('begin',context)
            if phase=='FreshArtifact' and not raw_arm:add('invalidation',{'context':context,'bytes':64,'identity':[1]})
            raw={'context':context,'sequence':sequence,'key_type':oid,'completed':True,'mode':mode,'calls':calls,'planned':calls,'dispatch_us':7,'policy':'ephemeral_fused' if raw_arm else 'cached_reusable','artifact_bytes':64,'rows':rows,'populations':trace,'preparation_ns':1000,'consumption_ns':8000,'descriptor_executor_ns':10000,'diagnostic_capture_ns':100,'bridge_delta':calls,'stock_delta':0,'verified_data_calls':verified,'construction_bytes':64,'built_artifact_preparation_us':1,'retained_bytes_before':100,'retained_bytes_after':100,'derived_bytes_before':64,'cost':'synthetic'}
            add('raw',raw)
            if phase is not None:
                evidence={'physical_mode':mode,'completed':True,'observed_calls':calls,'scheduled_calls':calls,'bridge_delta':calls,'verified_data_calls':verified,'mask_calls':calls-1 if raw_arm else 0,'control_calls':0 if raw_arm else calls-1,'explicit_capacity_override':None,'artifact_cache_hit':not raw_arm and phase!='FreshArtifact','resident_load_included':False}
                for key in ('preparation_ns','consumption_ns','descriptor_executor_ns','diagnostic_capture_ns','construction_bytes','built_artifact_preparation_us'):evidence[key]=raw[key]
                add('computed',{**context,'rows':groups,'evidence':evidence})
                add('verdict',{'fixture':fixture,'phase':phase,'pair':context['pair'],'position':context['position'],'error':None})
            else:add('verdict',{'context':context,'error':None})
        add('fixture_complete',{'fixture':fixture})
    add('complete',{'fixtures':2,'scheduled_executions':100,'jit_probes':4})
    return records

class ProtocolTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root=BASE/'paired-checker-negative1'
        cls.root.mkdir(mode=0o700)
        cls.receipts=cls.root/'receipts';cls.receipts.mkdir(mode=0o700)
        cls.cases=cls.root/'negative-cases';cls.cases.mkdir(mode=0o700)
        cls.records=synthetic_records()
        if len(cls.records)!=442:raise RuntimeError('invalid synthetic inventory')
        for record in cls.records:
            cls.path(record['ordinal']).write_bytes(cls.encode(record))
    @classmethod
    def path(cls,index):return cls.receipts/f'record-{index:06}.json'
    @staticmethod
    def encode(record):return (json.dumps(record,separators=(',',':'))+'\n').encode()
    def rejection(self,name,index,mutate):
        original=self.path(index).read_bytes()
        modified=copy.deepcopy(self.records[index]);mutate(modified)
        encoded=self.encode(modified)
        (self.cases/f'{name}.json').write_bytes(encoded)
        self.path(index).write_bytes(encoded)
        try:
            with self.assertRaises((ValueError,KeyError,TypeError,IndexError)):
                CHECK.verify(self.receipts)
        finally:self.path(index).write_bytes(original)
    def test_complete_synthetic_protocol(self):
        self.assertEqual(CHECK.verify(self.receipts)['scheduled_executions'],100)
    def test_negative_receipt_mutations(self):
        cases=[
            ('ordinal',0,lambda r:r.update(ordinal=True)),
            ('bounds',0,lambda r:r['payload'].update(record_limit=513)),
            ('arm',4,lambda r:r['payload'].update(arm='Raw')),
            ('sequence',8,lambda r:r['payload'].update(sequence=1)),
            ('incomplete',8,lambda r:r['payload'].update(completed=False)),
            ('fallback',8,lambda r:r['payload'].update(stock_delta=1)),
            ('mode',8,lambda r:r['payload'].update(mode='parallel_dense_count')),
            ('type',8,lambda r:r['payload'].update(key_type=20)),
            ('duplicate-row',8,lambda r:r['payload']['rows'].__setitem__(1,r['payload']['rows'][2])),
            ('negative-count',8,lambda r:r['payload']['rows'][0].__setitem__(1,-1)),
            ('missing-row',8,lambda r:r['payload']['rows'].pop()),
            ('no-v2',8,lambda r:r['payload']['populations'].update(actual_v2_calls=0)),
            ('bad-leaf',8,lambda r:r['payload']['populations']['leaves'][0].update(partition=1)),
            ('wrong-where',8,lambda r:r['payload']['populations']['leaves'][0].update(whole=1,partition=1)),
            ('retained',8,lambda r:r['payload'].update(retained_bytes_after=101)),
            ('timers',8,lambda r:r['payload'].update(descriptor_executor_ns=1)),
            ('verdict',9,lambda r:r['payload'].update(error='failed')),
            ('forced',12,lambda r:r['payload']['evidence'].update(explicit_capacity_override=32)),
            ('call-equality',12,lambda r:r['payload']['evidence'].update(bridge_delta=999)),
            ('control-accounting',12,lambda r:r['payload']['evidence'].update(control_calls=999)),
            ('warm-miss',52,lambda r:r['payload']['evidence'].update(artifact_cache_hit=False)),
            ('fresh-hit',133,lambda r:r['payload']['evidence'].update(artifact_cache_hit=True)),
            ('fresh-load',133,lambda r:r['payload']['evidence'].update(resident_load_included=True)),
            ('completion',441,lambda r:r['payload'].update(scheduled_executions=99)),
        ]
        for name,index,mutate in cases:
            with self.subTest(name=name):self.rejection(name,index,mutate)
    def test_truncation_and_duplicate_json_field(self):
        original=self.path(0).read_bytes()
        for name,bad in [('truncated',original[:-1]),('duplicate',original.replace(b'"ordinal":0',b'"ordinal":0,"ordinal":0',1))]:
            (self.cases/f'{name}.json').write_bytes(bad);self.path(0).write_bytes(bad)
            try:
                with self.assertRaises(ValueError):CHECK.verify(self.receipts)
            finally:self.path(0).write_bytes(original)
    def test_missing_unknown_and_symlink_files(self):
        last=self.path(441);retained=self.root/'held-last.json';last.rename(retained)
        try:
            with self.assertRaises(ValueError):CHECK.verify(self.receipts)
        finally:retained.rename(last)
        extra=self.receipts/'unknown';extra.write_text('never accepted')
        try:
            with self.assertRaises(ValueError):CHECK.verify(self.receipts)
        finally:extra.rename(self.cases/'unknown')
        last.rename(retained);last.symlink_to(retained)
        try:
            with self.assertRaises(ValueError):CHECK.verify(self.receipts)
        finally:last.unlink();retained.rename(last)

if __name__=='__main__':unittest.main(verbosity=2)
