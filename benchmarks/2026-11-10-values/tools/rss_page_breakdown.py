"""Reproduce the paired RSS mapping-class analysis from the observer logs."""
import json
from pathlib import Path
import random
import re
import statistics


def analyse(root):
    result = {}
    for mode in ['normal', 'tcache0']:
        rows = {}
        round_ids = {}
        for name in ['base', 'final']:
            rows[name] = []
            paths = {}
            for path in (root / mode).glob('closed64-' + name + '-*.log'):
                round_id = int(path.stem.rsplit('-', 1)[1])
                if round_id in paths:
                    raise RuntimeError('Duplicate paired round')
                paths[round_id] = path
            round_ids[name] = set(paths)
            for _, path in sorted(paths.items()):
                line = next(line for line in path.read_text().splitlines()
                            if 'RSS_ACCOUNTING ' in line)
                values = {key: int(value) for key, value
                          in re.findall(r'(\w+)=(\d+)', line)}
                values['non_anonymous_kib'] = values['page_table_kib'] - values['anonymous_kib']
                rows[name].append(values)
        if round_ids['base'] != round_ids['final'] or len(rows['base']) < 2:
            raise RuntimeError('Incomplete paired observer logs')
        stats = {}
        rng = random.Random(1774)
        for key in ['page_table_kib', 'anonymous_kib', 'non_anonymous_kib',
                    'private_clean_kib', 'pss_file_kib']:
            differences = [b[key] - a[key] for a, b in zip(rows['base'], rows['final'])]
            draws = sorted(statistics.median(rng.choices(differences, k=len(differences)))
                           for _ in range(10000))
            stats[key] = {
                'base': statistics.median(row[key] for row in rows['base']),
                'final': statistics.median(row[key] for row in rows['final']),
                'paired_delta_kib': statistics.median(differences),
                'bootstrap95_delta_kib': [draws[250], draws[9749]],
            }
        result[mode] = stats
    return result


if __name__ == '__main__':
    root = Path(__file__).resolve().parents[1] / 'peers-rss'
    result = analyse(root)
    expected = json.loads((root / 'page-breakdown.json').read_text())
    if result != expected:
        raise RuntimeError('Saved mapping-class analysis does not reproduce')
    print('Paired mapping-class analysis reproduced')
