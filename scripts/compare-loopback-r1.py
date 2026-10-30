#!/usr/bin/env python3
"""CPU-pinned, alternating paired release measurements; localhost only.

Use identical loopback.rs and development dependencies at both revisions.
Confidence intervals resample paired execution ratios, not individual RPCs.
"""
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
    parser.add_argument('--runs', type=int, default=22)
    parser.add_argument('--window-ms', type=int, default=1500)
    args = parser.parse_args()
    if args.runs < 2 or args.window_ms < 100:
        parser.error('need at least two pairs and 100 ms windows')
    args.output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, LOOPBACK_SHORT='1', LOOPBACK_WINDOW_MS=str(args.window_ms),
               LOOPBACK_CLIENT_CPUS='0', LOOPBACK_SERVER_CPUS='2,3', LOOPBACK_RUNTIME='current')
    env.pop('LOOPBACK_PAYLOAD', None)
    metadata = {'binaries': {name: {'name': getattr(args, name).name,
                        'sha256': hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()}
                for name in ('baseline', 'candidate')},
                'environment': {k: v for k, v in env.items() if k.startswith('LOOPBACK_')},
                'pairs': args.runs, 'command': '<binary> --exact loopback_rpc_cost --ignored --nocapture --test-threads=1',
                'cpu_affinity': 'client thread 0; responder threads 2,3'}
    (args.output / 'configuration.json').write_text(json.dumps(metadata, indent=2)+'\n')
    samples = {name: {payload: [] for payload in ('Plain', 'Gzip')} for name in ('baseline', 'candidate')}
    for run in range(args.runs):
        for name in (('baseline', 'candidate') if run % 2 == 0 else ('candidate', 'baseline')):
            binary = getattr(args, name).resolve()
            result = subprocess.run(['/usr/bin/time', '-v', str(binary), '--exact', 'loopback_rpc_cost',
                '--ignored', '--nocapture', '--test-threads=1'], env=env, text=True,
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60, check=True)
            text = result.stdout.replace(str(binary), '<binary>')
            (args.output / f'{name}-{run:02d}.log').write_text(text)
            for payload in ('Plain', 'Gzip'):
                rate, user, system = map(float, re.search(rf'{payload} reply .*? round 0: ([\d.]+) RPC/s; client thread ([\d.]+) us user \+ ([\d.]+) us kernel', text).groups())
                latency = list(map(float, re.search(rf'{payload} round 0: sampled RPC latency p50/p95/p99 ([\d.]+)/([\d.]+)/([\d.]+) us', text).groups()))
                rss = int(re.search(r'Maximum resident set size \(kbytes\): (\d+)', text)[1])
                samples[name][payload].append(dict(rate=rate, user_us=user, sys_us=system, cpu_us=user+system,
                    p50_us=latency[0], p95_us=latency[1], p99_us=latency[2], process_rss_kib=rss))
        print(f'completed pair {run+1}/{args.runs}', flush=True)
    summary = {}
    rng = random.Random(20261030)
    for payload in ('Plain', 'Gzip'):
        summary[payload] = {}
        for key in samples['baseline'][payload][0]:
            a = [row[key] for row in samples['baseline'][payload]]
            b = [row[key] for row in samples['candidate'][payload]]
            ratios = [y/x - 1 for x, y in zip(a, b)]
            draws = sorted(statistics.median(rng.choices(ratios, k=len(ratios))) * 100 for _ in range(10000))
            summary[payload][key] = dict(baseline_median=statistics.median(a), candidate_median=statistics.median(b),
                paired_delta_percent=statistics.median(ratios)*100, bootstrap_95_percent=[draws[250], draws[9749]])
    (args.output / 'samples.json').write_text(json.dumps(samples, indent=2)+'\n')
    (args.output / 'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    print(json.dumps(summary, indent=2))


if __name__ == '__main__':
    main()
