"""Use the established paired harness with longer closed-loop RSS windows."""
import importlib.util
import argparse
import hashlib
import json
import os
import re
from pathlib import Path

root = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location('paired_bot', root / 'scripts/compare-bot-workload.py')
paired = importlib.util.module_from_spec(spec)
spec.loader.exec_module(paired)
paired.SCENARIOS = {'closed64': dict(paired.SCENARIOS['closed64'],
                                   BOT_WARM_MS='3000', BOT_WINDOW_MS='12000')}
original_run_once = paired.run_once


def validate_snapshot(row, log):
    if row is None or 'RSS_ACCOUNTING_FAILED' in log:
        raise RuntimeError('RSS snapshot failed or workload timed out')
    readings = re.findall(r'RSS_ACCOUNTING status_kib=(\d+) page_table_kib=(\d+)', log)
    if len(readings) != 2:
        raise RuntimeError('Expected both page-table RSS diagnostics')
    if float(readings[0][1]) != row['rss_steady_kib']:
        raise RuntimeError('Steady RSS does not match the page-table snapshot')


def run_once(binary, env):
    row, log = original_run_once(binary, env)
    validate_snapshot(row, log)
    return row, log


if __name__ == '__main__':
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument('--output', type=Path, required=True)
    args, _ = parser.parse_known_args()
    preload = Path(os.environ['LD_PRELOAD']).resolve()
    if not preload.is_file():
        raise RuntimeError('One RSS snapshot preload library is required')
    args.output.mkdir(parents=True, exist_ok=True)
    source = Path(__file__).with_name('rss_snapshot.c')
    (args.output / 'rss-observer.json').write_text(json.dumps({
        'preload_sha256': hashlib.sha256(preload.read_bytes()).hexdigest(),
        'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
        'GLIBC_TUNABLES': os.environ.get('GLIBC_TUNABLES', ''),
        'method': 'identical preload on both retained binaries; page-table VmRSS; closed64 only',
        'limits': 'DSO/stack pages and proc-read allocation differ from uninstrumented runs',
    }, indent=2) + '\n')
    paired.run_once = run_once
    paired.main()
