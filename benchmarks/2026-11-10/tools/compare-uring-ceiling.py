#!/usr/bin/env python3
"""Paired runs of uring_ceiling: the epoll pattern (A) against one io_uring variant (B).

Loopback only, no Telegram traffic. Strictly one process at a time, in alternating order
(A,B then B,A) so that drift cancels. Every result is appended to DIR/runs.jsonl as the
benchmark printed it, with the invocation, pair, side, position and load average added.
DIR/summary.json and the table describe the runs of this invocation alone.

A metric is compared by the median of the paired percentage changes 100*(B/A-1) with an
unadjusted paired bootstrap 95% interval, the method of compare-overload.py and
compare-bot-workload.py. An interval that includes zero is no measured change. Fewer
than six pairs get no verdict.

Run it alone: not beside a build, a test or another measurement.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import statistics
import subprocess
import sys
import time

# B's extra arguments. A is always --mode epoll.
VARIANTS = {
    'uring': [],
    'uring-send-inline': ['--uring-send-inline'],
    'uring-wait-past-sends': ['--uring-wait-past-sends'],
}
# Options handed to both sides unchanged, and only when given: the benchmark's defaults
# stay in one place.
PASS_THROUGH = (('conns', int), ('inflight', int), ('req', int), ('rep', int),
                ('work-iters', int), ('seconds', float), ('warmup', float), ('pin', str))
COMPARED = ('rps', 'cpu_us_per_rpc', 'user_us_per_rpc', 'sys_us_per_rpc',
            'server_cpu_us_per_rpc', 'p50_us', 'p95_us', 'p99_us')
COUNTS = ('send_per_rpc', 'recv_per_rpc', 'epoll_wait_per_rpc', 'uring_enter_per_rpc',
          'sqe_per_rpc', 'cqe_per_rpc', 'resched_irq_per_rpc')
SAME_ON_BOTH_SIDES = ('conns', 'inflight', 'req', 'rep', 'work_iters', 'pin')
BOOTSTRAP_DRAWS = 10000
# With n pairs and no real difference, every change has the same sign once in 2**(n-1)
# times and the interval then excludes zero: below six pairs that is more than 5%.
MIN_PAIRS_FOR_VERDICT = 6
# The benchmark's own defaults and watchdog margin, used only to bound a hung run.
DEFAULT_SECONDS, DEFAULT_WARMUP, WATCHDOG_MARGIN = 3.0, 0.5, 30.0


def compare(a, b, rng):
    """Medians of both sides and the paired change of B against A, in percent."""
    item = dict(a_median=statistics.median(a), b_median=statistics.median(b))
    # A side that is zero somewhere has no ratio to give.
    if all(a):
        changes = [100 * (y / x - 1) for x, y in zip(a, b)]
        draws = sorted(statistics.median(rng.choices(changes, k=len(changes)))
                       for _ in range(BOOTSTRAP_DRAWS))
        low, high = draws[250], draws[9749]
        verdict = ('too few pairs' if len(changes) < MIN_PAIRS_FOR_VERDICT else
                   'B higher' if low > 0 else 'B lower' if high < 0 else 'no measured change')
        item.update(paired_delta_percent=statistics.median(changes),
                    bootstrap_95_percent=[low, high], verdict=verdict,
                    paired_changes_percent=changes)
    return item


def run_once(command, timeout):
    """One benchmark process: its JSON object and what it wrote to stderr."""
    try:
        result = subprocess.run(command, text=True, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, timeout=timeout)
    except subprocess.TimeoutExpired as error:
        raise RuntimeError(f'timed out: {" ".join(command)}\n{error.stderr or ""}') from None
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    if result.returncode or len(lines) != 1:
        raise RuntimeError(f'exit {result.returncode}: {" ".join(command)}\n'
                           f'{result.stdout}{result.stderr}')
    return json.loads(lines[0]), result.stderr


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('binary', type=Path, help='the compiled uring_ceiling')
    parser.add_argument('--pairs', type=int, default=12)
    parser.add_argument('--out', type=Path, required=True, metavar='DIR')
    parser.add_argument('--variant', choices=sorted(VARIANTS), default='uring',
                        help='the B side; A is always --mode epoll')
    for name, kind in PASS_THROUGH:
        parser.add_argument(f'--{name}', type=kind, help='passed to both sides')
    args = parser.parse_args()
    if args.pairs < 2:
        parser.error('at least two pairs required')
    binary = args.binary.resolve()
    common = []
    for name, _ in PASS_THROUGH:
        value = getattr(args, name.replace('-', '_'))
        if value is not None:
            common += [f'--{name}', str(value)]
    sides = {'A': ('epoll', ['--mode', 'epoll']),
             'B': (args.variant, ['--mode', 'uring', *VARIANTS[args.variant]])}
    timeout = ((DEFAULT_SECONDS if args.seconds is None else args.seconds)
               + (DEFAULT_WARMUP if args.warmup is None else args.warmup) + WATCHDOG_MARGIN)
    args.out.mkdir(parents=True, exist_ok=True)
    invocation = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())

    rows = {'A': [], 'B': []}
    diagnostics = {}
    with (args.out / 'runs.jsonl').open('a') as raw:
        for pair in range(args.pairs):
            for position, side in enumerate('AB' if pair % 2 == 0 else 'BA'):
                variant, extra = sides[side]
                load = os.getloadavg()[0]
                row, stderr = run_once([str(binary), *extra, *common], timeout)
                if row.get('variant') != variant:
                    raise RuntimeError(f'asked for {variant}, got {row.get("variant")}')
                diagnostics.setdefault(side, stderr)
                for line in stderr.splitlines():
                    if 'warning' in line:
                        print(f'pair {pair + 1} {side}: {line}', file=sys.stderr, flush=True)
                rows[side].append(row)
                raw.write(json.dumps(dict(row, invocation=invocation, pair=pair, side=side,
                                          position=position, load1=load)) + '\n')
                raw.flush()
            a, b = rows['A'][-1], rows['B'][-1]
            print(f'pair {pair + 1}/{args.pairs}: cpu_us_per_rpc A {a["cpu_us_per_rpc"]:.3f} '
                  f'B {b["cpu_us_per_rpc"]:.3f}, rps A {a["rps"]:.0f} B {b["rps"]:.0f}, '
                  f'load {os.getloadavg()[0]:.2f}', flush=True)

    first = rows['A'][0]
    for row in rows['A'] + rows['B']:
        for key in SAME_ON_BOTH_SIDES:
            if row[key] != first[key]:
                raise RuntimeError(f'{key} differs between runs: {row[key]!r} and {first[key]!r}')
    rng = random.Random(20261110)

    def column(side, key):
        return [row[key] for row in rows[side]]

    summary = dict(
        a=sides['A'][0], b=sides['B'][0], pairs=args.pairs, invocation=invocation,
        binary=dict(path=binary.name, sha256=hashlib.sha256(binary.read_bytes()).hexdigest()),
        arguments=common,
        configuration={key: first[key] for key in SAME_ON_BOTH_SIDES + ('kernel',)},
        method=('median of the paired percentage changes 100*(B/A-1); unadjusted paired '
                f'bootstrap 95% interval, {BOOTSTRAP_DRAWS} resamples of the pairs; an interval '
                'that includes zero is no measured change'),
        seconds_median={side: statistics.median(column(side, 'seconds')) for side in 'AB'},
        compared={key: compare(column('A', key), column('B', key), rng) for key in COMPARED},
        per_request={key: dict(a_median=statistics.median(column('A', key)),
                               b_median=statistics.median(column('B', key))) for key in COUNTS},
        # What would make a run a poor model: work that left the client thread, sends
        # that did not go out whole, and other activity on the host.
        checks={side: dict(
            max_other_cpu_fraction=max(row['other_cpu_us'] / row['client_cpu_us']
                                       for row in rows[side]),
            send_retries=sum(row['send_retries'] for row in rows[side]),
            client_cpu_share_median=statistics.median(
                row['client_cpu_us'] / (row['seconds'] * 1e6) for row in rows[side]),
        ) for side in 'AB'},
        diagnostics=diagnostics,
    )
    (args.out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')

    print(f'\nA {summary["a"]}, B {summary["b"]}: {args.pairs} pairs, '
          + ' '.join(f'{key}={first[key]}' for key in SAME_ON_BOTH_SIDES)
          + f', {summary["seconds_median"]["A"]:.2f} s windows')
    print(f'{"metric":<24}{"A median":>14}{"B median":>14}   B against A, 95% interval')
    for key in COMPARED:
        item = summary['compared'][key]
        line = f'{key:<24}{item["a_median"]:>14.3f}{item["b_median"]:>14.3f}   '
        if 'verdict' in item:
            low, high = item['bootstrap_95_percent']
            line += (f'{item["paired_delta_percent"]:>+7.2f}% [{low:+.2f}, {high:+.2f}]  '
                     f'{item["verdict"]}')
        else:
            line += 'no ratio'
        print(line)
    print('user and sys are a tick-sampled split of the exact cpu figure: weigh cpu first')
    print(f'\n{"per request":<24}{"A median":>14}{"B median":>14}')
    for key in COUNTS:
        item = summary['per_request'][key]
        print(f'{key:<24}{item["a_median"]:>14.4f}{item["b_median"]:>14.4f}')
    for side in 'AB':
        check = summary['checks'][side]
        print(f'{side}: client thread busy {100 * check["client_cpu_share_median"]:.1f}% of the '
              f'window, other threads at most {100 * check["max_other_cpu_fraction"]:.2f}% of '
              f'its CPU, {check["send_retries"]} sends outside the model')
        if check['max_other_cpu_fraction'] > 0.01 or check['send_retries']:
            print(f'warning: side {side} left the model, see the line above', file=sys.stderr)


if __name__ == '__main__':
    main()
