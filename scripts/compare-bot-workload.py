#!/usr/bin/env python3
"""Repeated local bot workload runs of several executables, in rotating order.

Every binary is a build of `grammers-client/tests/bot_workload.rs`, which measures the
client process alone against a mock in another process. No Telegram traffic is involved.
The last `--binary` is compared against each earlier one by the median of paired
percentage changes, with an unadjusted paired bootstrap 95% interval.
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

SCENARIOS = {
    # A closed loop finds capacity; open loops model fixed request rates.
    'closed64': dict(BOT_CONCURRENCY='64', BOT_CLIENT_CPUS='0'),
    'closed256': dict(BOT_CONCURRENCY='256', BOT_CLIENT_CPUS='0'),
    'open5000': dict(BOT_RATE='5000', BOT_RUNTIME='multi', BOT_WORKERS='2',
                     BOT_CONCURRENCY='4096', BOT_CLIENT_CPUS='0,1'),
    'open500': dict(BOT_RATE='500', BOT_RUNTIME='multi', BOT_WORKERS='2',
                    BOT_CONCURRENCY='4096', BOT_CLIENT_CPUS='0,1',
                    BOT_WARM_MS='3000', BOT_WINDOW_MS='6000'),
    # A session in a real file, with one sender or a new sender for every update.
    'sqlite500': dict(BOT_RATE='500', BOT_RUNTIME='multi', BOT_WORKERS='2', BOT_SESSION='sqlite',
                      BOT_CONCURRENCY='4096', BOT_CLIENT_CPUS='0,1',
                      BOT_WARM_MS='3000', BOT_WINDOW_MS='6000'),
    'sqlite500new': dict(BOT_RATE='500', BOT_RUNTIME='multi', BOT_WORKERS='2', BOT_SESSION='sqlite',
                         BOT_PEERS='1000000', BOT_CONCURRENCY='4096', BOT_CLIENT_CPUS='0,1',
                         BOT_WARM_MS='3000', BOT_WINDOW_MS='6000'),
    'sqlite2000': dict(BOT_RATE='2000', BOT_RUNTIME='multi', BOT_WORKERS='2', BOT_SESSION='sqlite',
                       BOT_CONCURRENCY='4096', BOT_CLIENT_CPUS='0,1',
                       BOT_WARM_MS='3000', BOT_WINDOW_MS='6000'),
}
METRICS = ('rate', 'updates_per_s', 'updates_lost', 'cpu_us', 'user_us', 'sys_us', 'core_share',
           'allocs_per_rpc', 'bytes_per_rpc', 'p50_us', 'p95_us', 'p99_us', 'rss_steady_kib',
           'rss_peak_kib', 'live_heap_b')


def run_once(binary, env):
    path = str(Path(binary).resolve())
    try:
        result = subprocess.run([path, '--exact', 'bot_workload', '--ignored', '--nocapture',
                                 '--test-threads=1'], env=env, text=True, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, timeout=120)
    except subprocess.TimeoutExpired:
        # A build that never finished is a result; its pair has no ratio.
        return None, 'timed out\n'
    log = result.stdout.replace(path, '<binary>')
    if result.returncode or '1 passed; 0 failed' not in log:
        raise RuntimeError(f'{binary} failed:\n{log}')
    line = next(line for line in log.splitlines() if 'BOT rate=' in line)
    row = {key: float(value) for key, value in re.findall(r'(\w+)=([\d.]+)', line)}
    if row['errors']:
        raise RuntimeError(f'{binary} reported errors:\n{log}')
    return row, log


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', action='append', required=True, metavar='NAME=PATH')
    parser.add_argument('--scenario', action='append', choices=sorted(SCENARIOS))
    parser.add_argument('--rounds', type=int, default=12)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--server-cpus', default='2,3')
    args = parser.parse_args()
    binaries = dict(item.split('=', 1) for item in args.binary)
    names = list(binaries)
    args.output.mkdir(parents=True, exist_ok=True)
    scenarios = args.scenario or sorted(SCENARIOS)
    (args.output / 'configuration.json').write_text(json.dumps(dict(
        scenarios={name: SCENARIOS[name] for name in scenarios}, rounds=args.rounds,
        server_cpus=args.server_cpus,
        binaries={name: hashlib.sha256(Path(path).read_bytes()).hexdigest()
                  for name, path in binaries.items()}), indent=2) + '\n')
    base = {k: v for k, v in os.environ.items() if not k.startswith('BOT_')}
    base['BOT_SERVER_CPUS'] = args.server_cpus
    summary = {}
    for scenario in scenarios:
        env = {**base, **SCENARIOS[scenario]}
        samples = {name: [] for name in names}
        for round_index in range(args.rounds):
            order = names[round_index % len(names):] + names[:round_index % len(names)]
            for name in order:
                row, log = run_once(binaries[name], env)
                samples[name].append(row)
                (args.output / f'{scenario}-{name}-{round_index:02d}.log').write_text(log)
            print(f'{scenario} round {round_index + 1}/{args.rounds} '
                  f'load={os.getloadavg()[0]:.2f}', flush=True)
        (args.output / f'{scenario}-samples.json').write_text(json.dumps(samples, indent=2) + '\n')
        rng = random.Random(1234)
        candidate = names[-1]
        result = {'medians': {name: {key: statistics.median(row[key] for row in samples[name] if row)
                                     for key in METRICS} for name in names},
                  'timeouts': {name: samples[name].count(None) for name in names},
                  'against': {}}
        for baseline in names[:-1]:
            result['against'][baseline] = {}
            pairs = [(x, y) for x, y in zip(samples[baseline], samples[candidate]) if x and y]
            for key in METRICS:
                ratios = [y[key] / x[key] - 1 for x, y in pairs if x[key]]
                # A count that is usually zero, such as lost updates, has no ratio to give.
                if len(ratios) < len(pairs):
                    continue
                draws = sorted(statistics.median(rng.choices(ratios, k=len(ratios))) * 100
                               for _ in range(10000))
                result['against'][baseline][key] = dict(
                    paired_delta_percent=round(statistics.median(ratios) * 100, 2),
                    bootstrap_95_percent=[round(draws[250], 2), round(draws[9749], 2)])
        summary[scenario] = result
        (args.output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    for scenario, result in summary.items():
        print(f'\n{scenario}')
        header = f'{"metric":<16}' + ''.join(f'{name:>12}' for name in names)
        for baseline in names[:-1]:
            header += f'   vs {baseline:<24}'
        print(header)
        for key in METRICS:
            line = f'{key:<16}' + ''.join(f'{result["medians"][name][key]:>12.2f}' for name in names)
            for baseline in names[:-1]:
                delta = result['against'][baseline].get(key)
                if delta is None:
                    line += '  ' + 'no ratio'.ljust(29)
                    continue
                low, high = delta['bootstrap_95_percent']
                mark = '*' if low > 0 or high < 0 else ' '
                line += f'  {delta["paired_delta_percent"]:>+7.2f}% [{low:+.2f},{high:+.2f}]{mark}'
            print(line)


if __name__ == '__main__':
    main()
