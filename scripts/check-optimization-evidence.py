#!/usr/bin/env python3
"""Check the staged October optimization evidence in a fresh index export.

Run after staging the intended source, documents, and artifacts. This does not
commit, run benchmarks, or establish review acceptance.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import statistics
import subprocess
import tempfile


def digest(data):
    return hashlib.sha256(data).hexdigest()


def git_at(repo, *args):
    return subprocess.check_output(['git', '-C', str(repo), *args])


def candidate_identity(repo, *, worktree=None, branch=None, head=None, tree=None):
    actual = {
        'worktree': git_at(repo, 'rev-parse', '--show-toplevel').decode().strip(),
        'branch': git_at(repo, 'branch', '--show-current').decode().strip(),
        'head': git_at(repo, 'rev-parse', 'HEAD').decode().strip(),
        'tree': git_at(repo, 'write-tree').decode().strip(),
    }
    if worktree is not None:
        assert Path(actual['worktree']).resolve() == Path(worktree).resolve(), 'wrong candidate worktree'
    if branch is not None:
        assert actual['branch'] == branch, 'wrong candidate branch'
    if head is not None:
        assert actual['head'] == head, 'wrong candidate HEAD'
    if tree is not None:
        assert actual['tree'] == tree, 'wrong candidate tree'
    return actual


def source_paths(paths):
    return {p for p in paths if p.endswith('.rs') or Path(p).name in {'Cargo.toml', 'Cargo.lock'}}


def check_membership(repo, prefix, tracked, inputs):
    missing = [str(p.relative_to(repo)) for p in (repo / prefix).rglob('*')
               if p.is_file() and str(p.relative_to(repo)) not in tracked]
    assert not missing, f'evidence missing from index: {missing}'
    assert set(inputs) == source_paths(tracked), 'source manifest membership differs'
    untracked = git_at(repo, 'ls-files', '--others', '--exclude-standard', '-z').decode().split('\0')
    assert not source_paths(untracked), 'untracked source files are outside the candidate'


def check_unmanifested_build_inputs(repo, tracked, inputs, checkpoint):
    # The historical manifests cover Rust and Cargo inputs. Anchor schemas and
    # repository build configuration too, without rewriting historical receipts.
    previous = set(filter(None, git_at(repo, 'ls-tree', '-r', '--name-only', '-z',
                                      checkpoint).decode().split('\0')))
    # Include ignored paths: local Cargo configuration can affect a build even
    # when Git ignores it. Include the old tree to detect staged deletions too.
    untracked = set(filter(None, git_at(repo, 'ls-files', '--others', '-z').decode().split('\0')))
    paths = {p for p in tracked | previous | untracked
             if p.endswith('.tl') or p.startswith('.cargo/')
             or Path(p).name in {'rust-toolchain', 'rust-toolchain.toml'}} - set(inputs)
    for path in sorted(paths):
        assert path in tracked, f'unrecorded or deleted build input: {path}'
        assert path in previous, f'unmanifested build input added: {path}'
        before = git_at(repo, 'show', checkpoint + ':' + path)
        current = git_at(repo, 'show', ':' + path)
        assert current == before, f'unmanifested build input changed: {path}'
    return len(paths)


def check_export_matches_worktree(repo, root, tracked):
    # Compare bytes, not only git diff: assume-unchanged/skip-worktree flags must
    # not allow tests on different contents from the staged review candidate.
    for path in sorted(tracked):
        live, staged = repo / path, root / path
        assert live.is_symlink() == staged.is_symlink(), f'unstaged file type: {path}'
        if staged.is_symlink():
            assert live.readlink() == staged.readlink(), f'unstaged symlink: {path}'
        else:
            assert live.read_bytes() == staged.read_bytes(), f'unstaged content: {path}'


def canonical_git(repo, *args):
    """Read original objects/index through an empty Git configuration."""
    with tempfile.TemporaryDirectory(prefix='evidence-git-') as directory:
        isolated = Path(directory)
        env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_')}
        env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull, LC_ALL='C')
        subprocess.check_call(['git', 'init', '--quiet', '--bare', str(isolated)], env=env)
        objects = git_at(repo, 'rev-parse', '--git-path', 'objects').decode().strip()
        index = git_at(repo, 'rev-parse', '--git-path', 'index').decode().strip()
        env.update(GIT_ALTERNATE_OBJECT_DIRECTORIES=str((repo / objects).resolve()),
                   GIT_INDEX_FILE=str((repo / index).resolve()))
        return subprocess.check_output(['git', '--git-dir=' + str(isolated), *args], env=env)


def source_diff(repo, *revisions):
    return canonical_git(repo, 'diff', '--full-index', '--binary', '--no-ext-diff',
                         '--no-textconv', '--no-renames', '--no-color', *revisions,
                         '--', 'Cargo.lock', ':(glob)**/Cargo.toml', ':(glob)**/*.rs')


def check_privacy(root, tracked, patterns):
    """Patterns are supplied outside the public checkout; scan all tracked text."""
    assert isinstance(patterns, list) and patterns, "privacy patterns must be nonempty"
    compiled = [(item['label'], re.compile(item['regex'])) for item in patterns]
    count = 0
    for path in sorted(tracked):
        try:
            content = (root / path).read_text()
        except UnicodeDecodeError:
            continue
        count += 1
        for label, pattern in compiled:
            assert not pattern.search(content), f'private pattern {label}: {path}'
    return count


CURRENT_COMMANDS = {
    'workspace': ['cargo', 'test', '--offline', '--locked', '--release', '--workspace', '--all-features'],
    'fmt': ['cargo', 'fmt', '--all', '--', '--check'],
    'check': ['cargo', 'check', '--offline', '--locked', '--workspace', '--all-targets', '--all-features'],
    'clippy': ['cargo', 'clippy', '--offline', '--locked', '--workspace', '--all-targets', '--all-features', '--', '-D', 'warnings'],
    'rustdoc': ['cargo', 'doc', '--offline', '--locked', '--workspace', '--all-features', '--no-deps'],
    'client-minimal': ['cargo', 'test', '--offline', '--locked', '--release', '-p', 'grammers-client', '--no-default-features', '--lib'],
    'debug-mentions': ['cargo', 'test', '--offline', '--locked', '-p', 'grammers-client', '--all-features', 'malformed_mentions'],
    'python': ['python3', '-m', 'unittest', 'discover', '-s', 'scripts', '-p', 'test_check_optimization_evidence.py', '-v'],
}
for suffix, feature in [('memory', None), ('import', 'session-import'),
                        ('string', 'session-string'), ('files', 'session-import-files')]:
    CURRENT_COMMANDS['session-' + suffix] = (
        ['cargo', 'test', '--offline', '--locked', '--release', '-p', 'grammers-session', '--no-default-features']
        + (['--features', feature] if feature else []) + ['--', '--test-threads=1'])
REQUIRED_VALIDATION = set(CURRENT_COMMANDS)
TOOLING_INPUTS = {'scripts/check-optimization-evidence.py', 'scripts/test_check_optimization_evidence.py'}


def evidence_path(root, relative):
    path = root / relative
    assert path.resolve().is_relative_to(root.resolve()), 'receipt path escapes evidence'
    return path


def check_source_binding(final, record):
    assert 'source_inputs_sha256' in record, 'missing source binding'
    source = evidence_path(final, record.get('source_inputs_file', 'source-inputs.json'))
    assert source.resolve().is_relative_to(final), 'source receipt escapes evidence'
    assert digest(source.read_bytes()) == record['source_inputs_sha256'], 'stale source binding'


def check_analysis_log(name, log):
    assert not re.search(r'^error(?:\[|:)', log, re.MULTILINE), (name, 'analysis error')
    if name in {'check', 'clippy'}:
        assert re.search(r'^\s*Checking grammers-', log, re.MULTILINE), (name, 'no workspace analysis')
    elif name == 'rustdoc':
        assert re.search(r'^\s*Documenting grammers-', log, re.MULTILINE), 'no workspace documentation'
        assert re.search(r'^\s*Generated ', log, re.MULTILINE), 'no generated documentation'


def check_current_validation(final, metadata, repo_root):
    receipt = evidence_path(final, metadata['validation_receipt'])
    record = json.loads(receipt.read_text())
    check_source_binding(final, record)
    assert record['source_inputs_sha256'] == metadata['source_inputs_sha256'], 'stale current validation'
    assert record.get('analysis_target_initially_empty') is True, 'analysis target was not fresh'
    tooling = record.get('tooling_inputs', {})
    assert set(tooling) == TOOLING_INPUTS, 'missing tooling input binding'
    for path, expected in tooling.items():
        assert digest((repo_root / path).read_bytes()) == expected, 'stale validation tooling'
    prefix = record['result_prefix']
    assert {prefix + name for name in REQUIRED_VALIDATION} <= set(record['results']), 'missing current validation rows'
    for name, command in CURRENT_COMMANDS.items():
        result = record['results'][prefix + name]
        assert result['command'] == command, (name, 'wrong validation command scope')
        if name == 'python':
            assert result.get('passed_tests', 0) > 0, 'missing Python test count'
        elif command[1] == 'test':
            assert result.get('totals', {}).get('passed', 0) > 0, (name, 'missing Rust test count')
        elif name == 'rustdoc':
            assert result.get('environment', {}).get('RUSTDOCFLAGS') == '-D warnings', 'rustdoc warning policy missing'
        if name in {'check', 'clippy', 'rustdoc'}:
            check_analysis_log(name, (receipt.parent / (prefix + name + '.log')).read_text())
    check_validation_receipt(receipt)


def check_benchmark_binding(final, metadata, binaries=None):
    build = json.loads(evidence_path(final, metadata['build_receipt']).read_text())
    assert build['exit_code'] == 0, 'benchmark build failed'
    assert build['source_inputs_sha256'] == metadata['source_inputs_sha256'], 'stale benchmark build'
    assert build['rustc'] and build['cargo'], 'missing compiler identity'
    expected = {'baseline': metadata['baseline_loopback_binary_sha256'],
                'candidate': metadata['final_loopback_binary_sha256']}
    assert build['binaries'] == expected, 'benchmark build binary mismatch'
    for name in ['paired', 'overload', 'heap', 'allocations']:
        configuration = evidence_path(final, metadata['benchmark_configurations'][name])
        config = json.loads(configuration.read_text())
        assert config['source_inputs_sha256'] == metadata['source_inputs_sha256'], 'stale benchmark measurement'
        actual = {side: value['sha256'] if isinstance(value, dict) else value
                  for side, value in config['binaries'].items()}
        assert actual == expected, 'benchmark measurement binary mismatch'
    if binaries:
        for side, path in binaries.items():
            assert digest(Path(path).read_bytes()) == expected[side], f'{side} executable hash mismatch'


def check_validation_receipt(receipt):
    validation = json.loads(receipt.read_text())
    results = ({row['name']: row for row in validation} if isinstance(validation, list)
               else validation['results'])
    for name, result in results.items():
        assert re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]*', name), 'invalid validation row name'
        assert result['exit_code'] == 0, (receipt.name, name, 'failed command')
        log = (receipt.parent / (name + '.log')).read_text()
        rows = re.findall(r'test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored', log)
        assert all(row[0] == 'ok' and int(row[2]) == 0 for row in rows), (name, 'failed tests')
        assert not re.search(r'^test .+ \.\.\. FAILED$', log, re.MULTILINE), (name, 'failed test row')
        raw_totals, totals = result.get('raw_result_line_totals'), result.get('totals')
        if raw_totals is not None and totals is not None:
            assert raw_totals == totals, (name, 'conflicting test totals')
        totals = raw_totals if raw_totals is not None else totals
        if totals is not None:
            assert rows, (name, 'missing result rows')
            for index, key in enumerate(['passed', 'failed', 'ignored'], 1):
                assert sum(int(row[index]) for row in rows) == totals[key], (name, key)
        if 'passed_tests' in result:
            runs = re.findall(r'^Ran (\d+) tests? in ', log, re.MULTILINE)
            assert runs and sum(map(int, runs)) == result['passed_tests'], (name, 'Python count')
            assert re.search(r'^OK$', log, re.MULTILINE), (name, 'Python result')
            assert not re.search(r'^FAILED(?: |$)', log, re.MULTILINE), (name, 'Python failure')


def check_benchmark_log(path, expected_test):
    log = path.read_text()
    rows = re.findall(r'^test result:.*$', log, re.MULTILINE)
    assert len(rows) == 1, (path.name, 'expected one benchmark result')
    assert re.fullmatch(r'test result: ok\. 1 passed; 0 failed; 0 ignored; '
                        r'0 measured; \d+ filtered out; finished in [\d.]+s', rows[0]), (
                            path.name, 'benchmark did not pass')
    assert not re.search(r'\bFAILED\b', log), (path.name, 'failed benchmark row')
    tests = re.findall(r'^test (\S+) \.\.\.', log, re.MULTILINE)
    assert tests == [expected_test], (path.name, 'wrong benchmark test')


def check_paired_benchmark_logs(directory, pairs, expected_test):
    assert type(pairs) is int and pairs > 0, 'invalid benchmark pair count'
    for side in ['baseline', 'candidate']:
        expected = {directory / f'{side}-{index:02d}.log' for index in range(pairs)}
        actual = set(directory.glob(side + '-*.log')) - {directory / (side + '-build.log')}
        assert actual == expected, (directory.name, 'benchmark log membership differs')
        for log in sorted(expected):
            check_benchmark_log(log, expected_test)


def check_current_component_logs(final, metadata):
    for name, expected_test in [('heap', 'loopback_session_footprint'),
                                ('allocations', 'loopback_rpc_cost')]:
        configuration = evidence_path(final, metadata['benchmark_configurations'][name])
        config = json.loads(configuration.read_text())
        assert type(config['runs_per_binary']) is int and config['runs_per_binary'] == 1, (
            name, 'expected one run per binary')
        expected = {configuration.parent / (side + '.log') for side in ['baseline', 'candidate']}
        assert set(configuration.parent.glob('*.log')) == expected, (name, 'benchmark log membership differs')
        for log in sorted(expected):
            check_benchmark_log(log, expected_test)

    receipt = evidence_path(final, metadata['deadline_receipt'])
    record = json.loads(receipt.read_text())
    assert record['candidate_source_inputs_sha256'] == metadata['source_inputs_sha256'], 'stale deadline source'
    baseline = final / 'performance/post-opus/source/source-inputs.json'
    assert digest(baseline.read_bytes()) == record['baseline_source_inputs_sha256'], 'stale deadline baseline'
    control = receipt.parent / 'baseline-source-inputs.json'
    assert digest(control.read_bytes()) == record['control_source_inputs_sha256'], 'stale deadline control'
    for side in ['baseline', 'candidate']:
        assert record['builds'][side]['exit_code'] == 0, (side, 'deadline build failed')
    expected_test = 'message_box::tests::benchmark_pending_channel_deadlines'
    assert record['test'] == expected_test, 'wrong deadline benchmark'
    check_paired_benchmark_logs(receipt.parent, record['pairs'], expected_test)


def scenario_cases():
    cases = {}
    for index in range(3):
        for name, runtime, cpus, test in [
                ('current-1cpu', 'current', '0', 'loopback_rpc_cost'),
                ('multi-1cpu', 'multi', '0', 'loopback_rpc_cost'),
                ('multi-2cpu', 'multi', '0,1', 'loopback_rpc_cost'),
                ('blocking-1cpu', 'current', '0', 'loopback_blocking_control'),
                ('blocking-2cpu', 'current', '0,1', 'loopback_blocking_control')]:
            cases[f'{name}-{index}'] = (test, dict(LOOPBACK_RUNTIME=runtime, LOOPBACK_CLIENT_CPUS=cpus))
    for name, extra in [
            ('open-warm', dict(LOOPBACK_RATE='20000')),
            ('open-overload', dict(LOOPBACK_RATE='120000')),
            ('open-cold', dict(LOOPBACK_RATE='20000', LOOPBACK_COLD='1')),
            ('open-large', dict(LOOPBACK_RATE='100', LOOPBACK_LARGE='1', LOOPBACK_COLD='1')),
            ('open-large-burst', dict(LOOPBACK_RATE='2000', LOOPBACK_LARGE='1', LOOPBACK_COLD='1'))]:
        cases[name] = ('loopback_open_loop', extra)
    cases['saturated-heap'] = ('loopback_session_footprint', {})
    return cases


def check_scenario_logs(final, metadata):
    receipt = evidence_path(final, metadata['scenario_receipt'])
    record = json.loads(receipt.read_text())
    assert record['source_inputs_sha256'] == metadata['source_inputs_sha256'], 'stale scenario source'
    expected_binaries = {'baseline': metadata['baseline_loopback_binary_sha256'],
                         'candidate': metadata['final_loopback_binary_sha256']}
    assert record['binaries'] == expected_binaries, 'scenario binary mismatch'
    config = receipt.parent / 'configuration.json'
    assert digest(config.read_bytes()) == record['configuration_sha256'], 'stale scenario configuration'
    rows = json.loads(config.read_text())
    cases = scenario_cases()
    expected = {(side, case) for side in expected_binaries for case in cases}
    assert record['executions'] == len(rows) == len(expected), 'scenario execution count'
    assert {(r['version'], r['case']) for r in rows} == expected, 'scenario execution membership'
    assert {p.name for p in receipt.parent.glob('*.log')} == {
        f'{side}-{case}.log' for side, case in expected}, 'scenario log membership'
    common = dict(LOOPBACK_WINDOW_MS='1500', LOOPBACK_SHORT='1', LOOPBACK_PAYLOAD='plain',
                  LOOPBACK_CLIENT_CPUS='0', LOOPBACK_SERVER_CPUS='2,3')
    scheduled, runtime = {}, {}
    for row in rows:
        test, extra = cases[row['case']]
        assert row['exit_code'] == 0 and row['test'] == test, 'scenario command failed or changed'
        assert row['environment'] == common | extra, 'scenario environment mismatch'
        path = receipt.parent / f"{row['version']}-{row['case']}.log"
        check_benchmark_log(path, test)
        log = path.read_text()
        assert re.findall(r'^\s*Exit status: (.*)$', log, re.MULTILINE) == ['0'], 'scenario process failed'
        assert re.findall(r'^benchmark_binary_sha256=(.*)$', log, re.MULTILINE) == [
            expected_binaries[row['version']]], 'scenario execution binary mismatch'
        if test == 'loopback_open_loop':
            groups = [dict(re.findall(r'(\w+)=([\w.]+)', line))
                      for line in log.splitlines() if 'OPEN group=' in line]
            assert len(groups) == 2 and [g['group'] for g in groups] == ['0', '1'], 'scenario groups'
            offered = int(extra['LOOPBACK_RATE']) * int(common['LOOPBACK_WINDOW_MS']) // 1000
            for g in groups:
                assert int(g['scheduled']) == offered, 'scenario offered count'
                assert g['cold'] == str('LOOPBACK_COLD' in extra).lower(), 'scenario cold mode'
                assert g['large'] == str('LOOPBACK_LARGE' in extra).lower(), 'scenario large mode'
            assert groups[0]['dropped'] == groups[1]['dropped'], 'scenario dropped count'
            values = dict(offered=offered, completed=sum(int(g['completed']) for g in groups),
                          errors=sum(int(g['errors']) for g in groups), harness_drops=int(groups[0]['dropped']),
                          busy_p99_us=float(groups[0]['p99_us']), quiet_p99_us=float(groups[1]['p99_us']),
                          process_rss_kib=int(re.search(r'Maximum resident set size \(kbytes\): (\d+)', log)[1]))
            assert values['completed'] + values['errors'] + values['harness_drops'] == offered, 'scenario accounting'
            scheduled.setdefault(row['case'], {})[row['version']] = values
        elif test != 'loopback_session_footprint':
            mode, index = row['case'].rsplit('-', 1)
            pattern = r'BLOCKING rate=(\d+)' if test == 'loopback_blocking_control' else r': (\d+) RPC/s;'
            rates = re.findall(pattern, log)
            assert len(rates) == 1, 'scenario rate count'
            runtime.setdefault(mode, {}).setdefault(row['version'], {})[int(index)] = int(rates[0])
    runtime = {mode: {side: {'rates': [values[i] for i in range(3)],
                             'median': statistics.median(values.values())}
                      for side, values in sides.items()} for mode, sides in runtime.items()}
    samples = json.loads((receipt.parent / 'samples.json').read_text())
    assert samples == {'scheduled_arrival': scheduled, 'runtime': runtime}, 'scenario samples differ from logs'


def check_retry_pass_logs(final, metadata):
    receipt = evidence_path(final, metadata['retry_pass_receipt'])
    record = json.loads(receipt.read_text())
    assert record['candidate_source_inputs_sha256'] == metadata['source_inputs_sha256'], 'stale retry source'
    baseline = final / 'performance/post-opus2/source/source-inputs.json'
    assert digest(baseline.read_bytes()) == record['baseline_source_inputs_sha256'], 'stale retry baseline'
    control = receipt.parent / 'control-source-inputs.json'
    assert digest(control.read_bytes()) == record['control_source_inputs_sha256'], 'stale retry control'
    patch = receipt.parent / 'benchmark-only.patch'
    assert digest(patch.read_bytes()) == record['benchmark_only_patch_sha256'], 'stale retry harness'
    test = 'message_box::tests::benchmark_missing_hash_retry_pass'
    assert record['test'] == test, 'wrong retry test'
    assert record['build_command'] == ['cargo', 'test', '--offline', '--locked', '--release',
                                       '-p', 'grammers-session', '--no-default-features', '--lib',
                                       '--no-run', '--message-format=json'], 'retry build scope'
    for side in ['baseline', 'candidate']:
        assert record['builds'][side]['exit_code'] == 0, 'retry build failed'
        binary_hash = record['builds'][side]['binary_sha256']
        assert re.fullmatch(r'[0-9a-f]{64}', binary_hash), 'retry binary hash'
    check_paired_benchmark_logs(receipt.parent, record['pairs'], test)
    samples = {side: {str(n): [] for n in [1000, 10000]} for side in ['baseline', 'candidate']}
    for side in samples:
        for index in range(record['pairs']):
            log = (receipt.parent / f'{side}-{index:02d}.log').read_text()
            assert re.findall(r'^benchmark_binary_sha256=(.*)$', log, re.MULTILINE) == [
                record['builds'][side]['binary_sha256']], 'retry execution binary mismatch'
            rows = re.findall(r'missing_hash_retry channels=(\d+) initial_pass_ns=(\d+) retry_pass_ns=(\d+)', log)
            assert len(rows) == 2 and [int(r[0]) for r in rows] == [1000, 10000], 'retry sample sizes'
            for count, first, second in rows:
                assert int(first) > 0 and int(second) > 0, 'retry elapsed time'
                samples[side][count].append([int(first), int(second)])
    assert json.loads((receipt.parent / 'samples.json').read_text()) == samples, 'retry samples differ from logs'


def check_receiver_logs(final, metadata):
    receipt = evidence_path(final, metadata['receiver_receipt'])
    record = json.loads(receipt.read_text())
    assert record['candidate_source_inputs_sha256'] == metadata['source_inputs_sha256'], 'stale receiver source'
    for field, path in [
        ('baseline_source_inputs_sha256', final / 'performance/post-acceptance-round1/source/source-inputs.json'),
        ('control_source_inputs_sha256', receipt.parent / 'control-source-inputs.json'),
        ('benchmark_only_patch_sha256', receipt.parent / 'benchmark-only.patch'),
    ]:
        assert digest(path.read_bytes()) == record[field], 'stale receiver control or harness'
    test = 'benchmark_sqlite_self_lookup_and_live_receiver'
    assert record['test'] == test and record['cpu'] == 0, 'receiver execution scope'
    assert record['build_command'] == ['cargo', 'test', '--offline', '--locked', '--release', '-p',
        'grammers-client', '--all-features', '--test', 'receiver_sqlite_cost', '--no-run', '--message-format=json'], 'receiver build scope'
    check_paired_benchmark_logs(receipt.parent, record['pairs'], test)
    samples = {side: {str(n): [] for n in [1000, 100000]} for side in ['baseline', 'candidate']}
    for side in samples:
        build = record['builds'][side]
        assert build['exit_code'] == 0 and re.fullmatch(r'[0-9a-f]{64}', build['binary_sha256']), 'receiver build identity'
        for index in range(record['pairs']):
            log = (receipt.parent / f'{side}-{index:02d}.log').read_text()
            assert re.findall(r'^benchmark_binary_sha256=(.*)$', log, re.MULTILINE) == [build['binary_sha256']], 'receiver execution binary mismatch'
            rows = re.findall(r'sqlite_live_receiver peers=(\d+) iterations=(\d+) self_lookup_total_ns=(\d+) live_total_ns=(\d+)', log)
            assert len(rows) == 2 and [r[0] for r in rows] == ['1000', '100000'], 'receiver sample sizes'
            for count, iterations, lookup, live in rows:
                assert iterations == '256' and int(lookup) > 0 and int(live) > 0, 'receiver workload or timing'
                samples[side][count].append([int(lookup), int(live)])
    assert json.loads((receipt.parent / 'samples.json').read_text()) == samples, 'receiver samples differ from logs'


def completion_message(privacy_checked):
    if privacy_checked:
        return 'PASS: staged evidence and supplied privacy checks are complete; review acceptance and publication require separate receipts'
    return 'INCOMPLETE: structural evidence checks passed; privacy scan is required before acceptance or publication'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--expect-worktree', help='Require this exact candidate checkout')
    parser.add_argument('--expect-branch', help='Require this candidate branch')
    parser.add_argument('--expect-head', help='Require this full checkpoint SHA')
    parser.add_argument('--expect-tree', help='Require this exact staged Git tree')
    parser.add_argument('--verify-upstream', action='store_true',
                        help='Verify canonical Git objects; first fetch the recorded Codeberg tip')
    parser.add_argument('--privacy-patterns', type=Path,
                        help='External JSON list of label/regex objects; scan all tracked text')
    parser.add_argument('--baseline-binary', type=Path, help='Verify retained baseline executable')
    parser.add_argument('--candidate-binary', type=Path, help='Verify retained candidate executable')
    args = parser.parse_args()
    assert bool(args.baseline_binary) == bool(args.candidate_binary), 'supply both executables'

    repo = Path(__file__).resolve().parents[1]

    def git(*args):
        return git_at(repo, *args)

    identity = candidate_identity(repo, worktree=args.expect_worktree,
                                  branch=args.expect_branch, head=args.expect_head,
                                  tree=args.expect_tree)
    print(f"candidate checkpoint: {identity['head']}")
    print(f"candidate branch: {identity['branch'] or '(detached)'}")
    print(f"candidate tree: {identity['tree']}")
    if not any([args.expect_worktree, args.expect_branch, args.expect_head, args.expect_tree]):
        print('identity not constrained: candidate identity is reported, not preselected')
    prefix = Path('benchmarks/2026-10-30')
    tracked = set(filter(None, git('ls-files', '-z').decode().split('\0')))

    with tempfile.TemporaryDirectory(prefix='grammers-evidence-') as temporary:
        root = Path(temporary)
        git('checkout-index', '--all', '--prefix=' + str(root) + '/')
        check_export_matches_worktree(repo, root, tracked)
        count = 0
        documents = [root / p for p in ['README.md', 'CHANGELOG.md', 'PUBLICATION.md',
                                       'OPTIMIZATION-2026-10.md', 'REVIEW-ROUND1.md',
                                       'REVIEW-ROUND2.md', 'REVIEW-FINAL.md',
                                       'PERFORMANCE.md', 'UPSTREAM.md', 'REVIEW-OPUS-CORRECTION.md',
                                       'REVIEW-OPUS-ROUND2.md']]
        documents.extend((root / prefix).rglob('*.md'))
        for document in documents:
            for target in re.findall(r'\[[^\]]*\]\(([^)\s]+)\)', document.read_text()):
                if '://' in target or target.startswith('#'):
                    continue
                target = target.split('#', 1)[0]
                path = (document.parent / target).resolve()
                assert path.is_relative_to(root), (document.name, target)
                assert path.exists(), (document.name, target)
                count += 1
        print(f'{count} local Markdown links resolve in a fresh index export')

        for name, revision in [('round1', '5b16ac41ae9d6b0cdd0bf42df50d6064561bc19e'),
                               ('round2', '5f0cb0511416e666b5476c078c0aaa86d9bce69e')]:
            source_file = root / prefix / name / 'source-inputs.json'
            metadata = json.loads((source_file.parent / 'source.json').read_text())
            assert digest(source_file.read_bytes()) == metadata['source_inputs_sha256']
            manifest = json.loads(source_file.read_text())
            for path, expected in manifest.items():
                assert digest(git('show', revision + ':' + path)) == expected, (name, path)
            before = '8f109a6' if name == 'round1' else '5b16ac4'
            patch = source_diff(repo, before, revision)
            assert digest(patch) == metadata['canonical_full_index_diff_sha256']
            print(f'{name}: {len(manifest)} historical source hashes match {revision[:7]}')

        final = root / prefix / 'final'
        metadata = json.loads((final / 'source.json').read_text())
        assert digest((final / 'source-inputs.json').read_bytes()) == metadata['source_inputs_sha256']
        print(f"candidate source manifest SHA-256: {metadata['source_inputs_sha256']}")
        patch = source_diff(repo, '--cached', metadata['correction_checkpoint'])
        assert digest(patch) == metadata['diff_sha256']
        assert (final / 'source.patch').read_bytes() == patch
        assert digest((root / 'grammers-mtsender/tests/loopback.rs').read_bytes()) == metadata['harness_sha256']
        inputs = json.loads((final / 'source-inputs.json').read_text())
        check_membership(repo, prefix, tracked, inputs)
        anchored = check_unmanifested_build_inputs(repo, tracked, inputs,
                                                   metadata['correction_checkpoint'])
        print(f'candidate: live files match index; {anchored} additional build inputs match checkpoint')
        for path, expected in inputs.items():
            assert digest((root / path).read_bytes()) == expected, path
        artifacts = json.loads((final / 'artifact-hashes.json').read_text())
        for path, expected in artifacts.items():
            assert digest((root / path).read_bytes()) == expected, path
        # A newly added artifact must not silently escape the manifest. The
        # manifest cannot hash itself. Gate/review/publication receipts live
        # outside this tree, so the reviewed index can be committed unchanged.
        excluded = {'artifact-hashes.json'}
        actual = {str(p.relative_to(root)) for p in final.rglob('*') if p.is_file()
                  and str(p.relative_to(final)) not in excluded}
        assert actual == set(artifacts), 'final artifact manifest membership differs'
        print(f'final: {len(inputs)} source and {len(artifacts)} artifact hashes match index')

        # Current required checks must refer to this exact source. Historical
        # receipts retain their own manifests and are checked where they live.
        check_current_validation(final, metadata, root)
        receipts = sorted((root / prefix).rglob('*commands.json'))
        for receipt in receipts:
            check_validation_receipt(receipt)
            if receipt.parent == final / 'validation':
                check_source_binding(final, json.loads(receipt.read_text()))
        check_benchmark_binding(final, metadata,
                                {'baseline': args.baseline_binary, 'candidate': args.candidate_binary}
                                if args.baseline_binary else None)
        print('current validation and benchmark receipts bind to the candidate source manifest')
        print('retained executable hashes checked' if args.baseline_binary
              else 'executable bytes not supplied; build/measurement hash consistency checked only')
        negative = json.loads((final / 'validation/fairness-negative-control.json').read_text())
        assert negative['actual_exit'] == negative['expected_exit'] == 101
        assert digest((final / 'performance/negative-control.patch').read_bytes()) == negative['patch_sha256']
        assert 'completed 512' in (final / 'validation/fairness-negative-control.log').read_text()
        print('validation: receipts match result-line totals; negative control failed as expected')

        for configuration in (root / prefix).rglob('configuration.json'):
            config = json.loads(configuration.read_text())
            if not isinstance(config, dict) or 'pairs' not in config:
                continue
            command = shlex.split(config['command'])
            expected_test = command[command.index('--exact') + 1]
            check_paired_benchmark_logs(configuration.parent, config['pairs'], expected_test)
        check_current_component_logs(final, metadata)
        check_scenario_logs(final, metadata)
        check_retry_pass_logs(final, metadata)
        check_receiver_logs(final, metadata)
        print('benchmarks: paired and current component logs contain exactly their successful named test')

        upstream = json.loads((final / 'upstream/source.json').read_text())
        for item in upstream['commits']:
            if 'saved_patch' in item:
                saved = final / 'upstream' / item['saved_patch']
                assert digest(saved.read_bytes()) == item['git_show_sha256'], item['commit']
            if args.verify_upstream:
                try:
                    raw = canonical_git(repo, 'show', '--format=fuller', '--full-index',
                                        '--binary', '--no-ext-diff', '--no-textconv', item['commit'])
                except subprocess.CalledProcessError as error:
                    raise AssertionError('upstream objects unavailable; run the fetch_command in upstream/source.json') from error
                assert digest(raw) == item['canonical_full_index_show_sha256'], item['commit']
        print('upstream: canonical objects verified' if args.verify_upstream
              else 'upstream: saved artifact hashes checked; Git object verification not requested')

        if args.privacy_patterns:
            scanned = check_privacy(root, tracked, json.loads(args.privacy_patterns.read_text()))
            print(f'privacy: {scanned} tracked text files checked with external patterns')
        else:
            print('privacy: external patterns not supplied; privacy scan not performed')
        print(completion_message(args.privacy_patterns is not None))
        return 0 if args.privacy_patterns is not None else 2


if __name__ == '__main__':
    raise SystemExit(main())
