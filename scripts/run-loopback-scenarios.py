#!/usr/bin/env python3
"""Run version-identical local runtime, scheduled-arrival and saturated-heap controls."""
import argparse
import json
import os
from pathlib import Path
import subprocess


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--baseline', type=Path, required=True)
    p.add_argument('--candidate', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    args = p.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    common = dict(LOOPBACK_WINDOW_MS='1500', LOOPBACK_SHORT='1', LOOPBACK_PAYLOAD='plain',
                  LOOPBACK_CLIENT_CPUS='0', LOOPBACK_SERVER_CPUS='2,3')
    cases = []
    for repeat in range(3):
        for name, runtime, cpus, test in [
            ('current-1cpu', 'current', '0', 'loopback_rpc_cost'),
            ('multi-1cpu', 'multi', '0', 'loopback_rpc_cost'),
            ('multi-2cpu', 'multi', '0,1', 'loopback_rpc_cost'),
            ('blocking-1cpu', 'current', '0', 'loopback_blocking_control'),
            ('blocking-2cpu', 'current', '0,1', 'loopback_blocking_control')]:
            cases.append((f'{name}-{repeat}', test, dict(LOOPBACK_RUNTIME=runtime, LOOPBACK_CLIENT_CPUS=cpus), 60))
    for name, extra in [
        ('open-warm', dict(LOOPBACK_RATE='20000')),
        ('open-overload', dict(LOOPBACK_RATE='120000')),
        ('open-cold', dict(LOOPBACK_RATE='20000', LOOPBACK_COLD='1')),
        ('open-large', dict(LOOPBACK_RATE='100', LOOPBACK_LARGE='1', LOOPBACK_COLD='1')),
        ('open-large-burst', dict(LOOPBACK_RATE='2000', LOOPBACK_LARGE='1', LOOPBACK_COLD='1'))]:
        cases.append((name, 'loopback_open_loop', extra, 60))
    cases.append(('saturated-heap', 'loopback_session_footprint', {}, 240))
    manifest = []
    for index, (case, test, extra, timeout) in enumerate(cases):
        for name in (('baseline', 'candidate') if index % 2 == 0 else ('candidate', 'baseline')):
            binary = getattr(args, name).resolve()
            config = dict(common, **extra)
            env = {k: v for k, v in os.environ.items() if not k.startswith('LOOPBACK_')}
            env.update(config)
            command = ['/usr/bin/time', '-v', str(binary), '--exact', test, '--ignored', '--nocapture', '--test-threads=1']
            result = subprocess.run(command, text=True, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout)
            text = result.stdout.replace(str(binary), '<binary>')
            (args.output / f'{name}-{case}.log').write_text(text)
            manifest.append(dict(version=name, case=case, test=test, environment=config, exit_code=result.returncode))
            (args.output / 'configuration.json').write_text(json.dumps(manifest, indent=2)+'\n')
            print(f'{name} {case}: exit {result.returncode}', flush=True)
            if result.returncode: raise RuntimeError(f'{name} {case} failed; inspect preserved log')


if __name__ == '__main__':
    main()
