#!/usr/bin/env python3
"""Repeated, paired local open-loop overload measurements; no Telegram traffic."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import re
import statistics
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--pairs', type=int, default=12)
    args = parser.parse_args()
    if args.pairs < 2:
        parser.error('at least two pairs required')
    args.output.mkdir(parents=True, exist_ok=True)
    config = dict(LOOPBACK_WINDOW_MS='1500', LOOPBACK_RATE='120000',
                  LOOPBACK_CLIENT_CPUS='0', LOOPBACK_SERVER_CPUS='2,3',
                  LOOPBACK_RUNTIME='current')
    env = {k: v for k, v in os.environ.items() if not k.startswith('LOOPBACK_')}
    env.update(config)
    metadata = dict(environment=config, pairs=args.pairs,
                    command='<binary> --exact loopback_open_loop --ignored --nocapture --test-threads=1',
                    binaries={name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()
                              for name in ('baseline', 'candidate')})
    (args.output / 'configuration.json').write_text(json.dumps(metadata, indent=2)+'\n')
    samples = {'baseline': [], 'candidate': []}
    for pair in range(args.pairs):
        for name in (('baseline', 'candidate') if pair % 2 == 0 else ('candidate', 'baseline')):
            binary = str(getattr(args, name).resolve())
            result = subprocess.run(['/usr/bin/time', '-v', binary, '--exact', 'loopback_open_loop',
                                     '--ignored', '--nocapture', '--test-threads=1'], env=env, text=True,
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60)
            log = result.stdout.replace(binary, '<binary>')
            (args.output / f'{name}-{pair:02d}.log').write_text(log)
            if result.returncode or '1 passed; 0 failed' not in log:
                raise RuntimeError(f'{name}/{pair} failed; inspect preserved log')
            groups = [dict(re.findall(r'(\w+)=([\w.]+)', line))
                      for line in log.splitlines() if 'OPEN group=' in line]
            assert len(groups) == 2
            groups.sort(key=lambda g: int(g['group']))
            assert groups[0]['dropped'] == groups[1]['dropped']
            assert groups[0]['peak'] == groups[1]['peak']
            row = dict(completed=sum(int(g['completed']) for g in groups),
                       errors=sum(int(g['errors']) for g in groups),
                       harness_drops=int(groups[0]['dropped']), peak=int(groups[0]['peak']))
            assert row['completed'] + row['errors'] + row['harness_drops'] == 180000
            for group, label in zip(groups, ('busy', 'quiet')):
                row.update({f'{label}_{key}': float(group[key]) for key in ('p50_us', 'p95_us', 'p99_us')})
            row['process_rss_kib'] = int(re.search(r'Maximum resident set size \(kbytes\): (\d+)', log)[1])
            samples[name].append(row)
        print(f'completed pair {pair+1}/{args.pairs}', flush=True)
    summary = {}
    rng = random.Random(20261030)
    for metric in samples['baseline'][0]:
        a = [row[metric] for row in samples['baseline']]
        b = [row[metric] for row in samples['candidate']]
        item = dict(baseline_median=statistics.median(a), candidate_median=statistics.median(b))
        if all(a):
            ratios = [100*(y/x-1) for x, y in zip(a, b)]
            draws = sorted(statistics.median(rng.choices(ratios, k=len(ratios))) for _ in range(10000))
            item.update(paired_delta_percent=statistics.median(ratios), bootstrap_95_percent=[draws[250], draws[9749]])
        summary[metric] = item
    (args.output / 'samples.json').write_text(json.dumps(samples, indent=2)+'\n')
    (args.output / 'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    print(json.dumps(summary, indent=2))


if __name__ == '__main__':
    main()
