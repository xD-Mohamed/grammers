#!/usr/bin/env python3
"""Combine disjoint paired phases after verifying their logs and existing summaries."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import random
import re
import statistics

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location('bot_compare', ROOT/'scripts/compare-bot-workload.py')
comparison = importlib.util.module_from_spec(spec)
spec.loader.exec_module(comparison)
METRICS = comparison.METRICS

def summarize(samples):
    rng = random.Random(1234)
    result = dict(medians={name:{metric:statistics.median(row[metric] for row in rows)
                                for metric in METRICS} for name,rows in samples.items()},
                  timeouts={name:0 for name in samples}, against={'base':{}})
    pairs = list(zip(samples['base'], samples['final']))
    for metric in METRICS:
        ratios = [y[metric]/x[metric]-1 for x,y in pairs if x[metric]]
        if len(ratios)!=len(pairs):
            continue
        draws = sorted(statistics.median(rng.choices(ratios,k=len(ratios)))*100
                       for _ in range(10000))
        result['against']['base'][metric] = dict(
            paired_delta_percent=round(statistics.median(ratios)*100,2),
            bootstrap_95_percent=[round(draws[250],2),round(draws[9749],2)])
    return result

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--phase',type=Path,action='append',required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--scenario',default='closed64',
                        help='The one scenario every phase measured (default closed64).')
    parser.add_argument('--allocator-provenance',required=True,
                        help='Separate execution evidence; original configurations omit GLIBC_TUNABLES.')
    args = parser.parse_args()
    scenario = args.scenario
    phases = [phase.resolve() for phase in args.phase]
    if len(set(phases))!=len(phases):
        raise ValueError('A phase may appear only once')
    if args.output.resolve() in phases:
        raise ValueError('Output must not overwrite an input phase')
    merged = {'base':[], 'final':[]}
    configuration = None
    membership = []
    log_paths, log_inodes, log_hashes = set(), set(), set()
    for phase in phases:
        config = json.loads((phase/'configuration.json').read_text())
        binaries = config.get('binaries',{})
        if set(binaries)!={'base','final'} or any(
            not isinstance(digest,str) or re.fullmatch(r'[a-f0-9]{64}',digest) is None
            for digest in binaries.values()
        ):
            raise ValueError('Explicit base/final SHA-256 identities are required')
        identity = {key:value for key,value in config.items() if key!='rounds'}
        if configuration is None:
            configuration = identity
        if identity!=configuration or set(identity['scenarios'])!={scenario}:
            raise ValueError('Mismatched binary, scenario or affinity configuration')
        samples = json.loads((phase/f'{scenario}-samples.json').read_text())
        if set(samples)!={'base','final'}:
            raise ValueError('Exactly base and final binaries are required')
        for name,rows in samples.items():
            if len(rows)!=config['rounds'] or any(row is None for row in rows):
                raise ValueError('Incomplete phase')
            for index,row in enumerate(rows):
                path = phase/f'{scenario}-{name}-{index:02d}.log'
                resolved = path.resolve()
                status = path.stat()
                inode = (status.st_dev,status.st_ino)
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                if resolved in log_paths or inode in log_inodes or digest in log_hashes:
                    raise ValueError(f'Reused log observation: {path}')
                log_paths.add(resolved); log_inodes.add(inode); log_hashes.add(digest)
                log = path.read_text()
                results = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed;',log)
                if results!=[('1','0')] or log.count('test bot_workload ...')!=1:
                    raise ValueError(f'Unexpected test execution: {path}')
                lines = [line for line in log.splitlines() if 'BOT rate=' in line]
                if len(lines)!=1:
                    raise ValueError(f'Unexpected measurement lines: {path}')
                parsed = {k:float(v) for k,v in re.findall(r'(\w+)=([\d.]+)',lines[0])}
                if parsed!=row or parsed['errors']!=0:
                    raise ValueError(f'Log/sample mismatch: {path}')
        expected = json.loads((phase/'summary.json').read_text())
        if expected!={scenario:summarize(samples)}:
            raise ValueError(f'Existing summary was not reproduced: {phase}')
        for index in range(config['rounds']):
            membership.append(dict(round=len(membership),phase=str(phase.relative_to(ROOT)),
                                   source_round=index, logs={name:str((phase/f'{scenario}-{name}-{index:02d}.log').relative_to(ROOT))
                                                            for name in merged}))
        for name in merged:
            merged[name].extend(samples[name])
    output = args.output
    output.mkdir(parents=True,exist_ok=True)
    (output/'configuration.json').write_text(json.dumps(dict(configuration,rounds=len(membership),
        allocator_provenance=args.allocator_provenance,
        method='Unadjusted paired bootstrap 95% intervals, same seeded median-ratio method as original harness; all phases retained.'),indent=2)+'\n')
    (output/'membership.json').write_text(json.dumps(membership,indent=2)+'\n')
    (output/f'{scenario}-samples.json').write_text(json.dumps(merged,indent=2)+'\n')
    summary = {scenario:summarize(merged)}
    (output/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    print(json.dumps(summary,indent=2))

if __name__=='__main__':
    main()
