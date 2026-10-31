"""Descriptive complete-sample report. No filtering, thresholds or refitting."""
import json
import pathlib
import statistics

ROOT = pathlib.Path(__file__).resolve().parent
records = [json.loads(p.read_text()) for p in sorted((ROOT / 'paired-run1/receipts').glob('record-*.json'))]

def stats(values):
    mean = statistics.mean(values)
    return {'n': len(values), 'median': statistics.median(values), 'min': min(values),
            'max': max(values), 'mean': mean,
            'sample_cv': statistics.stdev(values) / mean if mean else 0,
            'samples_in_order': values}

report = {'scope': 'Descriptor construction through EOF/close/frees; not whole SQL. Consumption includes dispatch; consumption-minus-dispatch is residual, not isolated materialization. Diagnostic capture is included in total and not subtracted.', 'fixtures': {}}
for fixture in ('int2_count_star', 'int8_count_filter'):
    summary = {}
    for phase in ('Warmup', 'Warm', 'FreshArtifact'):
        samples = [r['payload'] for r in records if r['kind'] == 'computed' and r['payload']['fixture'] == fixture and r['payload']['phase'] == phase]
        arms = {}
        for arm in ('Legacy', 'Raw'):
            evidence = [r['evidence'] for r in samples if r['arm'] == arm]
            fields = ('descriptor_executor_ns', 'preparation_ns', 'consumption_ns', 'dispatch_ns', 'diagnostic_capture_ns', 'built_artifact_preparation_us', 'construction_bytes', 'observed_calls', 'verified_data_calls', 'mask_calls', 'control_calls')
            arms[arm] = {name: stats([r[name] for r in evidence]) for name in fields}
            arms[arm]['consumption_minus_dispatch_ns'] = stats([r['consumption_ns'] - r['dispatch_ns'] for r in evidence])
        pairs = []
        for pair in sorted({r['pair'] for r in samples}):
            by_arm = {r['arm']: r['evidence']['descriptor_executor_ns'] for r in samples if r['pair'] == pair}
            pairs.append(by_arm['Legacy'] / by_arm['Raw'])
        summary[phase] = {'arms': arms, 'paired_legacy_over_raw': stats(pairs)}
    recipe = next(r['payload'] for r in records if r['kind'] == 'recipes' and r['payload']['fixture'] == fixture)
    summary['costs'] = {key: recipe[key] for key in ('legacy_cost', 'raw_cost')}
    summary['default_plan'] = next(r['payload']['lines'] for r in records if r['kind'] == 'default_plan' and r['payload']['fixture'] == fixture)
    report['fixtures'][fixture] = summary
print(json.dumps(report, indent=2))
