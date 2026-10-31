"""Offline regression tests for candidate identity and evidence membership."""
import importlib.util
import json
import os
import sys
import io
from contextlib import redirect_stdout
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    'evidence', Path(__file__).with_name('check-optimization-evidence.py'))
evidence = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(evidence)


class CandidateChecks(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='evidence-test-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        self.git('init', '--quiet')
        self.write('Cargo.toml', '[workspace]\n')
        self.write('src/lib.rs', '// source\n')
        self.write('schema/api.tl', '// schema\n')
        self.write('benchmarks/evidence.json', '{}\n')
        self.write('.gitignore', '*.log\n')
        self.git('add', '.')
        self.tracked = set(filter(None, self.git('ls-files', '-z').decode().split('\0')))
        self.inputs = {'Cargo.toml': '', 'src/lib.rs': ''}
        # A tree is enough for git show TREE:path; no test creates a commit.
        self.checkpoint = self.git('write-tree').decode().strip()

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args],
                                       stderr=subprocess.PIPE)

    def write(self, name, text):
        path = self.repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def check_membership(self, inputs=None):
        evidence.check_membership(self.repo, Path('benchmarks'), self.tracked,
                                  self.inputs if inputs is None else inputs)

    def test_complete_candidate_and_unchanged_schema_pass(self):
        self.check_membership()
        self.assertEqual(evidence.check_unmanifested_build_inputs(
            self.repo, self.tracked, self.inputs, self.checkpoint), 1)

    def test_ignored_non_json_evidence_cannot_escape_index(self):
        self.write('benchmarks/missing.log', 'unrecorded run\n')
        self.assertEqual(self.git('ls-files', '--others', '--exclude-standard'), b'')
        with self.assertRaisesRegex(AssertionError, 'evidence missing from index'):
            self.check_membership()

    def test_manifest_cannot_omit_tracked_source(self):
        with self.assertRaisesRegex(AssertionError, 'source manifest membership'):
            self.check_membership({'Cargo.toml': ''})

    def test_untracked_source_cannot_escape_manifest(self):
        self.write('src/new.rs', '// unrecorded source\n')
        with self.assertRaisesRegex(AssertionError, 'untracked source'):
            self.check_membership()

    def test_staged_schema_change_cannot_reuse_old_build_receipts(self):
        self.write('schema/api.tl', '// changed schema\n')
        self.git('add', 'schema/api.tl')
        with self.assertRaisesRegex(AssertionError, 'unmanifested build input changed'):
            evidence.check_unmanifested_build_inputs(
                self.repo, self.tracked, self.inputs, self.checkpoint)

    def test_ignored_cargo_configuration_cannot_escape_build_identity(self):
        self.write('.gitignore', '*.log\n.cargo/\n')
        self.git('add', '.gitignore')
        self.write('.cargo/config.toml', '[build]\nrustflags = ["--cfg", "different"]\n')
        self.assertEqual(self.git('ls-files', '--others', '--exclude-standard'), b'')
        with self.assertRaisesRegex(AssertionError, 'unrecorded or deleted build input'):
            evidence.check_unmanifested_build_inputs(
                self.repo, self.tracked, self.inputs, self.checkpoint)

    def test_deleted_checkpoint_configuration_cannot_escape_build_identity(self):
        self.write('.cargo/config.toml', '[build]\njobs = 1\n')
        self.git('add', '.cargo/config.toml')
        checkpoint = self.git('write-tree').decode().strip()
        (self.repo / '.cargo/config.toml').unlink()
        self.git('add', '-u', '.cargo/config.toml')
        with self.assertRaisesRegex(AssertionError, 'unrecorded or deleted build input'):
            evidence.check_unmanifested_build_inputs(
                self.repo, self.tracked, self.inputs, checkpoint)

    def test_new_staged_toolchain_cannot_reuse_old_build_identity(self):
        self.write('rust-toolchain.toml', '[toolchain]\nchannel = "stable"\n')
        self.git('add', 'rust-toolchain.toml')
        with self.assertRaisesRegex(AssertionError, 'unmanifested build input added'):
            evidence.check_unmanifested_build_inputs(
                self.repo, self.tracked | {'rust-toolchain.toml'}, self.inputs, self.checkpoint)

    def test_assume_unchanged_does_not_hide_different_tested_source(self):
        exported = self.root / 'exported'
        exported.mkdir()
        self.git('checkout-index', '--all', '--prefix=' + str(exported) + '/')
        evidence.check_export_matches_worktree(self.repo, exported, self.tracked)
        self.git('update-index', '--assume-unchanged', 'src/lib.rs')
        self.write('src/lib.rs', '// different tested source\n')
        self.assertEqual(self.git('diff'), b'')
        with self.assertRaisesRegex(AssertionError, 'unstaged content'):
            evidence.check_export_matches_worktree(self.repo, exported, self.tracked)

    def test_canonical_diff_ignores_local_git_configuration(self):
        self.write('src/lib.rs', '// changed source\n')
        self.git('add', 'src/lib.rs')
        expected = evidence.source_diff(self.repo, '--cached', self.checkpoint)
        self.git('config', 'core.abbrev', '12')
        self.git('config', 'diff.noprefix', 'true')
        self.git('config', 'diff.context', '100')
        self.git('config', 'diff.algorithm', 'histogram')
        self.assertEqual(evidence.source_diff(self.repo, '--cached', self.checkpoint), expected)
        self.assertRegex(expected.decode(), r'index [0-9a-f]{40}\.\.[0-9a-f]{40}')

    def test_each_expected_identity_field_is_enforced(self):
        actual = [str(self.repo).encode(), b'candidate', b'1' * 40, b'3' * 40]
        expected = {'worktree': self.repo, 'branch': 'candidate', 'head': '1' * 40, 'tree': '3' * 40}
        with patch.object(evidence, 'git_at', side_effect=actual):
            self.assertEqual(evidence.candidate_identity(self.repo, **expected)['branch'], 'candidate')
        for field, wrong in [('worktree', self.root / 'scaffolding'),
                             ('branch', 'baseline'), ('head', '2' * 40), ('tree', '4' * 40)]:
            with self.subTest(field=field), patch.object(evidence, 'git_at', side_effect=actual):
                with self.assertRaisesRegex(AssertionError, 'wrong candidate'):
                    evidence.candidate_identity(self.repo, **(expected | {field: wrong}))


class BenchmarkChecks(unittest.TestCase):
    @staticmethod
    def success(name):
        return (f'test {name} ... metrics\nok\n'
                'test result: ok. 1 passed; 0 failed; 0 ignored; '
                '0 measured; 5 filtered out; finished in 0.01s\n')

    def test_benchmark_log_rejects_hidden_failure_empty_or_wrong_test(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / 'candidate.log'
            good = self.success('loopback_rpc_cost')
            log.write_text(good)
            evidence.check_benchmark_log(log, 'loopback_rpc_cost')
            for invalid in [
                good + 'test result: FAILED. 0 passed; 1 failed; 0 ignored\n',
                good + 'test result: malformed\n', good + good,
                good + 'test another ... FAILED\n', '',
                good.replace('1 passed', '0 passed'),
                good.replace('0 ignored', '1 ignored'),
                good.replace('loopback_rpc_cost', 'loopback_session_footprint'),
            ]:
                with self.subTest(log=invalid):
                    log.write_text(invalid)
                    with self.assertRaises(AssertionError):
                        evidence.check_benchmark_log(log, 'loopback_rpc_cost')

    def test_paired_logs_require_positive_counts_and_exact_membership(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for side in ['baseline', 'candidate']:
                (root / (side + '-00.log')).write_text(self.success('loopback_rpc_cost'))
            evidence.check_paired_benchmark_logs(root, 1, 'loopback_rpc_cost')
            for count in [0, -1, True, 1.0]:
                with self.subTest(count=count), self.assertRaisesRegex(AssertionError, 'invalid benchmark pair count'):
                    evidence.check_paired_benchmark_logs(root, count, 'loopback_rpc_cost')
            extra = root / 'candidate-01.log'
            extra.write_text(self.success('loopback_rpc_cost'))
            with self.assertRaisesRegex(AssertionError, 'membership differs'):
                evidence.check_paired_benchmark_logs(root, 1, 'loopback_rpc_cost')
            extra.unlink()
            (root / 'candidate-00.log').unlink()
            with self.assertRaisesRegex(AssertionError, 'membership differs'):
                evidence.check_paired_benchmark_logs(root, 1, 'loopback_rpc_cost')

    def test_current_components_require_successful_bound_complete_logs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            meta = {'source_inputs_sha256': 'current', 'benchmark_configurations': {},
                    'deadline_receipt': 'deadlines/receipt.json'}
            for name, test in [('heap', 'loopback_session_footprint'), ('allocations', 'loopback_rpc_cost')]:
                folder = root / name
                folder.mkdir()
                meta['benchmark_configurations'][name] = name + '/configuration.json'
                (folder / 'configuration.json').write_text(json.dumps({'runs_per_binary': 1}))
                for side in ['baseline', 'candidate']:
                    (folder / (side + '.log')).write_text(self.success(test))
            baseline = root / 'performance/post-opus/source/source-inputs.json'
            baseline.parent.mkdir(parents=True)
            baseline.write_text('{}')
            folder = root / 'deadlines'
            folder.mkdir()
            (folder / 'baseline-source-inputs.json').write_text('{"control": true}')
            test = 'message_box::tests::benchmark_pending_channel_deadlines'
            record = {'candidate_source_inputs_sha256': 'current',
                      'baseline_source_inputs_sha256': evidence.digest(baseline.read_bytes()),
                      'control_source_inputs_sha256': evidence.digest((folder / 'baseline-source-inputs.json').read_bytes()),
                      'builds': {side: {'exit_code': 0} for side in ['baseline', 'candidate']},
                      'pairs': 1, 'test': test}
            receipt = folder / 'receipt.json'
            receipt.write_text(json.dumps(record))
            for side in ['baseline', 'candidate']:
                (folder / (side + '-00.log')).write_text(self.success(test))
            evidence.check_current_component_logs(root, meta)
            for path in [root / 'heap/candidate.log', root / 'allocations/baseline.log', folder / 'candidate-00.log']:
                good = path.read_text()
                path.write_text(good + 'test result: FAILED\n')
                with self.assertRaises(AssertionError):
                    evidence.check_current_component_logs(root, meta)
                path.unlink()
                with self.assertRaisesRegex(AssertionError, 'membership differs'):
                    evidence.check_current_component_logs(root, meta)
                path.write_text(good)
            for field in ['candidate_source_inputs_sha256', 'baseline_source_inputs_sha256', 'control_source_inputs_sha256']:
                receipt.write_text(json.dumps(record | {field: 'stale'}))
                with self.assertRaisesRegex(AssertionError, 'stale deadline'):
                    evidence.check_current_component_logs(root, meta)
            receipt.write_text(json.dumps(record))
            record['builds']['candidate']['exit_code'] = 101
            receipt.write_text(json.dumps(record))
            with self.assertRaisesRegex(AssertionError, 'deadline build failed'):
                evidence.check_current_component_logs(root, meta)


class ValidationChecks(unittest.TestCase):
    def test_main_and_detached_identity_are_allowed_without_explicit_constraint(self):
        for branch in [b'main', b'']:
            with patch.object(evidence, 'git_at', side_effect=[b'/checkout', branch, b'1' * 40, b'3' * 40]):
                self.assertEqual(evidence.candidate_identity(Path('/checkout'))['branch'], branch.decode())

    def test_routing_receipt_rejects_failed_rows_and_mismatched_totals(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = root / 'routing-commands.json'
            result = {'exit_code': 0, 'totals': {'passed': 1, 'failed': 0, 'ignored': 0}}
            receipt.write_text(json.dumps({'results': {'routing-workspace': result}}))
            log = root / 'routing-workspace.log'
            log.write_text('test result: ok. 1 passed; 0 failed; 0 ignored\n')
            evidence.check_validation_receipt(receipt)
            for extra in ['test result: FAILED. 0 passed; 1 failed; 0 ignored\n', 'test hidden ... FAILED\n',
                          'test result: ok. 1 passed; 0 failed; 0 ignored\n']:
                log.write_text('test result: ok. 1 passed; 0 failed; 0 ignored\n' + extra)
                with self.assertRaises(AssertionError):
                    evidence.check_validation_receipt(receipt)
            result['exit_code'] = 101
            receipt.write_text(json.dumps({'results': {'routing-workspace': result}}))
            with self.assertRaisesRegex(AssertionError, 'failed command'):
                evidence.check_validation_receipt(receipt)

    def test_null_or_conflicting_total_alias_cannot_hide_missing_tests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = root / 'commands.json'
            row = {'exit_code': 0, 'totals': {'passed': 1, 'failed': 0, 'ignored': 0},
                   'raw_result_line_totals': None}
            receipt.write_text(json.dumps({'results': {'workspace': row}}))
            (root / 'workspace.log').write_text('success\n')
            with self.assertRaisesRegex(AssertionError, 'missing result rows'):
                evidence.check_validation_receipt(receipt)
            row['raw_result_line_totals'] = {'passed': 0, 'failed': 0, 'ignored': 0}
            receipt.write_text(json.dumps({'results': {'workspace': row}}))
            (root / 'workspace.log').write_text('test result: ok. 0 passed; 0 failed; 0 ignored\n')
            with self.assertRaisesRegex(AssertionError, 'conflicting test totals'):
                evidence.check_validation_receipt(receipt)

    def test_python_receipt_rejects_hidden_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = root / 'routing-commands.json'
            receipt.write_text(json.dumps({'results': {'routing-python': {'exit_code': 0, 'passed_tests': 10}}}))
            log = root / 'routing-python.log'
            log.write_text('Ran 10 tests in 0.1s\n\nOK\n')
            evidence.check_validation_receipt(receipt)
            log.write_text('Ran 10 tests in 0.1s\nFAILED (failures=1)\nOK\n')
            with self.assertRaisesRegex(AssertionError, 'Python failure'):
                evidence.check_validation_receipt(receipt)

    def test_external_privacy_patterns_cover_text_outside_benchmarks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'src').mkdir()
            (root / 'src/lib.rs').write_text('// /private-example/checkout\n')
            patterns = [{'label': 'local path', 'regex': '/private-example/'}]
            with self.assertRaisesRegex(AssertionError, 'private pattern local path: src/lib.rs'):
                evidence.check_privacy(root, {'src/lib.rs'}, patterns)
            (root / 'src/lib.rs').write_text('// public source\n')
            self.assertEqual(evidence.check_privacy(root, {'src/lib.rs'}, patterns), 1)

    def test_analysis_logs_must_show_workspace_work(self):
        for name in ['check', 'clippy', 'rustdoc']:
            for log in ['', 'Finished dev profile\n', 'Checking unrelated v1\n']:
                with self.subTest(name=name, log=log), self.assertRaises(AssertionError):
                    evidence.check_analysis_log(name, log)
        for name in ['check', 'clippy']:
            evidence.check_analysis_log(name, 'Checking grammers-client v0.1\n')
            with self.assertRaises(AssertionError):
                evidence.check_analysis_log(name, 'Checking grammers-client v0.1\nerror: failed\n')
        with self.assertRaises(AssertionError):
            evidence.check_analysis_log('rustdoc', 'Documenting grammers-client v0.1\n')
        evidence.check_analysis_log('rustdoc', 'Documenting grammers-client v0.1\nGenerated target/doc/index.html\n')

    def test_validation_requires_named_current_rows_and_source_binding(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'source-inputs.json'
            source.write_text('{}')
            current_hash = evidence.digest(source.read_bytes())
            meta = {'validation_receipt': 'commands.json', 'source_inputs_sha256': current_hash}
            tooling = {}
            for name in evidence.TOOLING_INPUTS:
                path = root / name
                path.parent.mkdir(exist_ok=True)
                path.write_text('fixture')
                tooling[name] = evidence.digest(path.read_bytes())
            record = {'source_inputs_sha256': current_hash, 'result_prefix': 'current-',
                      'analysis_target_initially_empty': True,
                      'tooling_inputs': tooling, 'results': {}}
            receipt = root / 'commands.json'
            receipt.write_text(json.dumps(record))
            with self.assertRaisesRegex(AssertionError, 'missing current validation rows'):
                evidence.check_current_validation(root, meta, root)
            for name in evidence.REQUIRED_VALIDATION:
                item = {'exit_code': 0, 'command': evidence.CURRENT_COMMANDS[name]}
                log = 'success\n'
                if name == 'python':
                    item['passed_tests'] = 1
                    log = 'Ran 1 test in 0.1s\nOK\n'
                elif item['command'][1] == 'test':
                    item['totals'] = {'passed': 1, 'failed': 0, 'ignored': 0}
                    log = 'test result: ok. 1 passed; 0 failed; 0 ignored\n'
                elif name == 'rustdoc':
                    item['environment'] = {'RUSTDOCFLAGS': '-D warnings'}
                    log = 'Documenting grammers-client v0.1\nGenerated target/doc/index.html\n'
                elif name in {'check', 'clippy'}:
                    log = 'Checking grammers-client v0.1\nFinished dev profile\n'
                record['results']['current-' + name] = item
                (root / ('current-' + name + '.log')).write_text(log)
            receipt.write_text(json.dumps(record))
            evidence.check_current_validation(root, meta, root)
            row = record['results']['current-workspace']
            del row['totals']
            receipt.write_text(json.dumps(record))
            with self.assertRaisesRegex(AssertionError, 'missing Rust test count'):
                evidence.check_current_validation(root, meta, root)
            row['totals'] = {'passed': 1, 'failed': 0, 'ignored': 0}
            row['command'] = ['cargo', 'test']
            receipt.write_text(json.dumps(record))
            with self.assertRaisesRegex(AssertionError, 'wrong validation command scope'):
                evidence.check_current_validation(root, meta, root)
            row['command'] = evidence.CURRENT_COMMANDS['workspace']
            (root / 'scripts/check-optimization-evidence.py').write_text('changed')
            receipt.write_text(json.dumps(record))
            with self.assertRaisesRegex(AssertionError, 'stale validation tooling'):
                evidence.check_current_validation(root, meta, root)
            (root / 'scripts/check-optimization-evidence.py').write_text('fixture')
            del record['source_inputs_sha256']
            receipt.write_text(json.dumps(record))
            with self.assertRaisesRegex(AssertionError, 'missing source binding'):
                evidence.check_current_validation(root, meta, root)
            record['source_inputs_sha256'] = '0' * 64
            receipt.write_text(json.dumps(record))
            with self.assertRaisesRegex(AssertionError, 'stale source binding'):
                evidence.check_current_validation(root, meta, root)

    def test_receipt_references_cannot_escape_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'evidence'
            root.mkdir()
            for name in ['../outside.json', str(root.parent / 'outside.json')]:
                with self.assertRaisesRegex(AssertionError, 'receipt path escapes evidence'):
                    evidence.evidence_path(root, name)

    def test_benchmark_receipts_reject_stale_sources_and_wrong_executables(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            meta = {'build_receipt': 'build.json', 'source_inputs_sha256': 'current',
                    'baseline_loopback_binary_sha256': evidence.digest(b'old'),
                    'final_loopback_binary_sha256': evidence.digest(b'new'),
                    'benchmark_configurations': {name: name + '.json' for name in ['paired', 'overload', 'heap', 'allocations']}}
            binaries = {'baseline': meta['baseline_loopback_binary_sha256'], 'candidate': meta['final_loopback_binary_sha256']}
            build = {'exit_code': 0, 'source_inputs_sha256': 'current', 'rustc': 'version', 'cargo': 'version', 'binaries': binaries}
            (root / 'build.json').write_text(json.dumps(build))
            for name in ['paired', 'overload', 'heap', 'allocations']:
                (root / (name + '.json')).write_text(json.dumps({'source_inputs_sha256': 'current', 'binaries': binaries}))
            evidence.check_benchmark_binding(root, meta)
            (root / 'baseline').write_bytes(b'old')
            (root / 'candidate').write_bytes(b'new')
            paths = {side: root / side for side in binaries}
            evidence.check_benchmark_binding(root, meta, paths)
            (root / 'candidate').write_bytes(b'other')
            with self.assertRaisesRegex(AssertionError, 'candidate executable hash mismatch'):
                evidence.check_benchmark_binding(root, meta, paths)
            build['source_inputs_sha256'] = 'outdated'
            (root / 'build.json').write_text(json.dumps(build))
            with self.assertRaisesRegex(AssertionError, 'stale benchmark build'):
                evidence.check_benchmark_binding(root, meta)


class ScenarioAndRetryChecks(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='evidence-components-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.meta = {'source_inputs_sha256': 'a' * 64,
                     'baseline_loopback_binary_sha256': 'b' * 64,
                     'final_loopback_binary_sha256': 'c' * 64,
                     'scenario_receipt': 'scenarios/receipt.json',
                     'retry_pass_receipt': 'retry/receipt.json'}

    def write_json(self, name, value):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value))
        return evidence.digest(path.read_bytes())

    def scenario_fixture(self):
        rows, scheduled, runtime = [], {}, {}
        common = dict(LOOPBACK_WINDOW_MS='1500', LOOPBACK_SHORT='1', LOOPBACK_PAYLOAD='plain',
                      LOOPBACK_CLIENT_CPUS='0', LOOPBACK_SERVER_CPUS='2,3')
        directory = self.root / 'scenarios'
        directory.mkdir(exist_ok=True)
        for side, binary in [('baseline', 'b' * 64), ('candidate', 'c' * 64)]:
            for case, (test, extra) in evidence.scenario_cases().items():
                rows.append(dict(version=side, case=case, test=test, exit_code=0,
                                 environment=common | extra))
                log = f'benchmark_binary_sha256={binary}\n' + BenchmarkChecks.success(test)
                log += '\tExit status: 0\n\tMaximum resident set size (kbytes): 123\n'
                if test == 'loopback_open_loop':
                    offered = int(extra['LOOPBACK_RATE']) * 3 // 2
                    cold, large = [str(key in extra).lower() for key in ['LOOPBACK_COLD', 'LOOPBACK_LARGE']]
                    for group in [0, 1]:
                        count = offered // 2 if group == 0 else offered - offered // 2
                        log += (f'OPEN group={group} scheduled={offered} completed={count} errors=0 '
                                f'dropped=0 p99_us={10 + group} cold={cold} large={large}\n')
                    scheduled.setdefault(case, {})[side] = dict(offered=offered, completed=offered,
                        errors=0, harness_drops=0, busy_p99_us=10.0, quiet_p99_us=11.0, process_rss_kib=123)
                elif test != 'loopback_session_footprint':
                    mode, index = case.rsplit('-', 1)
                    rate = (100 if side == 'baseline' else 200) + int(index)
                    log += f'BLOCKING rate={rate}\n' if test == 'loopback_blocking_control' else f'rate: {rate} RPC/s;\n'
                    runtime.setdefault(mode, {})[side] = {'rates': [rate - int(index) + n for n in range(3)],
                                                         'median': rate - int(index) + 1}
                (directory / f'{side}-{case}.log').write_text(log)
        config_hash = self.write_json('scenarios/configuration.json', rows)
        record = dict(source_inputs_sha256=self.meta['source_inputs_sha256'],
                      binaries={'baseline': 'b' * 64, 'candidate': 'c' * 64},
                      configuration_sha256=config_hash, executions=len(rows))
        self.write_json('scenarios/receipt.json', record)
        self.write_json('scenarios/samples.json', dict(scheduled_arrival=scheduled, runtime=runtime))
        evidence.check_scenario_logs(self.root, self.meta)
        return rows, record

    def test_scenario_receipt_and_execution_identity(self):
        rows, record = self.scenario_fixture()
        for field, value in [('source_inputs_sha256', 'd' * 64), ('configuration_sha256', 'd' * 64),
                             ('binaries', {'baseline': 'c' * 64, 'candidate': 'b' * 64})]:
            with self.subTest(field=field):
                self.write_json('scenarios/receipt.json', record | {field: value})
                with self.assertRaises(AssertionError): evidence.check_scenario_logs(self.root, self.meta)
        self.write_json('scenarios/receipt.json', record)
        for mutate in [lambda x: x.pop(), lambda x: x.__setitem__(0, x[1]),
                       lambda x: x[0].update(exit_code=101), lambda x: x[0].update(test='other'),
                       lambda x: x[0]['environment'].update(LOOPBACK_WINDOW_MS='2000')]:
            changed = json.loads(json.dumps(rows)); mutate(changed)
            config_hash = self.write_json('scenarios/configuration.json', changed)
            self.write_json('scenarios/receipt.json', record | {'configuration_sha256': config_hash})
            with self.assertRaises(AssertionError): evidence.check_scenario_logs(self.root, self.meta)

    def test_scenario_logs_and_metrics(self):
        self.scenario_fixture()
        path = self.root / 'scenarios/candidate-open-large.log'
        good = path.read_text()
        for bad in ['', good.replace('Exit status: 0', 'Exit status: 1'),
                    good + 'Exit status: 0\n', good + 'Exit status: 1\n',
                    good.replace('c' * 64, 'b' * 64), good + 'benchmark_binary_sha256=bad\n',
                    good.replace('large=true', 'large=false'), good.replace('completed=75', 'completed=74'),
                    good.replace('p99_us=10', 'p99_us=99'), good.replace('0 failed', '1 failed')]:
            with self.subTest(log=bad[-100:]):
                path.write_text(bad)
                with self.assertRaises(AssertionError): evidence.check_scenario_logs(self.root, self.meta)
        path.write_text(good); path.unlink()
        with self.assertRaisesRegex(AssertionError, 'membership'): evidence.check_scenario_logs(self.root, self.meta)
        path.write_text(good)
        (path.parent / 'extra.log').write_text(good)
        with self.assertRaisesRegex(AssertionError, 'membership'): evidence.check_scenario_logs(self.root, self.meta)

    def retry_fixture(self):
        directory = self.root / 'retry'; directory.mkdir()
        baseline_hash = self.write_json('performance/post-opus2/source/source-inputs.json', {'Cargo.toml': 'old'})
        control_hash = self.write_json('retry/control-source-inputs.json', {'Cargo.toml': 'control'})
        (directory / 'benchmark-only.patch').write_text('harness patch\n')
        test = 'message_box::tests::benchmark_missing_hash_retry_pass'
        record = dict(candidate_source_inputs_sha256=self.meta['source_inputs_sha256'],
                      baseline_source_inputs_sha256=baseline_hash, control_source_inputs_sha256=control_hash,
                      benchmark_only_patch_sha256=evidence.digest((directory / 'benchmark-only.patch').read_bytes()),
                      test=test, pairs=1,
                      build_command=['cargo', 'test', '--offline', '--locked', '--release', '-p',
                                     'grammers-session', '--no-default-features', '--lib', '--no-run', '--message-format=json'],
                      builds={side: dict(exit_code=0, binary_sha256=char * 64) for side, char in [('baseline', 'b'), ('candidate', 'c')]})
        samples = {}
        for side, char, scale in [('baseline', 'b', 1), ('candidate', 'c', 2)]:
            log = f'benchmark_binary_sha256={char * 64}\n' + BenchmarkChecks.success(test)
            samples[side] = {}
            for n in [1000, 10000]:
                log += f'missing_hash_retry channels={n} initial_pass_ns={n * scale} retry_pass_ns={n * scale + 1}\n'
                samples[side][str(n)] = [[n * scale, n * scale + 1]]
            (directory / f'{side}-00.log').write_text(log)
        self.write_json('retry/receipt.json', record); self.write_json('retry/samples.json', samples)
        evidence.check_retry_pass_logs(self.root, self.meta)
        return record

    def test_retry_source_build_and_execution_binding(self):
        record = self.retry_fixture()
        for field in ['candidate_source_inputs_sha256', 'baseline_source_inputs_sha256',
                      'control_source_inputs_sha256', 'benchmark_only_patch_sha256']:
            self.write_json('retry/receipt.json', record | {field: 'd' * 64})
            with self.assertRaises(AssertionError): evidence.check_retry_pass_logs(self.root, self.meta)
        for mutate in [lambda x: x['builds']['candidate'].update(exit_code=101),
                       lambda x: x['builds']['candidate'].update(binary_sha256='bad'),
                       lambda x: x['build_command'].remove('--release')]:
            changed = json.loads(json.dumps(record)); mutate(changed)
            self.write_json('retry/receipt.json', changed)
            with self.assertRaises(AssertionError): evidence.check_retry_pass_logs(self.root, self.meta)
        self.write_json('retry/receipt.json', record)
        path = self.root / 'retry/candidate-00.log'; good = path.read_text()
        for bad in [good.replace('c' * 64, 'b' * 64), good.split('\n', 1)[1],
                    good + 'benchmark_binary_sha256=bad\n', good + good.split('\n', 1)[0] + '\n',
                    good.replace('channels=1000 ', 'channels=10000 '),
                    good.replace('initial_pass_ns=2000 ', 'initial_pass_ns=0 '),
                    good.replace('retry_pass_ns=2001', 'retry_pass_ns=2002'),
                    good.replace('0 failed', '1 failed')]:
            path.write_text(bad)
            with self.assertRaises(AssertionError): evidence.check_retry_pass_logs(self.root, self.meta)

    def test_receiver_component_requires_bound_source_binary_and_metrics(self):
        record = self.retry_fixture()
        directory = self.root / 'retry'
        self.meta['receiver_receipt'] = 'retry/receipt.json'
        baseline = self.root / 'performance/post-opus2/source/source-inputs.json'
        target = self.root / 'performance/post-acceptance-round1/source/source-inputs.json'
        target.parent.mkdir(parents=True); target.write_bytes(baseline.read_bytes())
        record.update(test='benchmark_sqlite_self_lookup_and_live_receiver', cpu=0,
            build_command=['cargo', 'test', '--offline', '--locked', '--release', '-p',
                           'grammers-client', '--all-features', '--test', 'receiver_sqlite_cost',
                           '--no-run', '--message-format=json'])
        samples = {}
        for side, char in [('baseline', 'b'), ('candidate', 'c')]:
            log = f'benchmark_binary_sha256={char * 64}\n' + BenchmarkChecks.success(record['test'])
            samples[side] = {}
            for n in [1000, 100000]:
                log += f'sqlite_live_receiver peers={n} iterations=256 self_lookup_total_ns={n} live_total_ns={n+1}\n'
                samples[side][str(n)] = [[n, n+1]]
            (directory / f'{side}-00.log').write_text(log)
        self.write_json('retry/receipt.json', record); self.write_json('retry/samples.json', samples)
        evidence.check_receiver_logs(self.root, self.meta)
        path = directory / 'candidate-00.log'; good = path.read_text()
        for bad in [good.replace('iterations=256', 'iterations=255'),
                    good.replace('c' * 64, 'b' * 64), good + 'benchmark_binary_sha256=bad\n',
                    good.replace('live_total_ns=1001', 'live_total_ns=1002')]:
            path.write_text(bad)
            with self.assertRaises(AssertionError): evidence.check_receiver_logs(self.root, self.meta)
        path.write_text(good)
        self.write_json('retry/receipt.json', record | {'candidate_source_inputs_sha256': 'd' * 64})
        with self.assertRaisesRegex(AssertionError, 'stale receiver source'):
            evidence.check_receiver_logs(self.root, self.meta)

    def test_privacy_is_required_for_complete_pass(self):
        self.assertTrue(evidence.completion_message(False).startswith('INCOMPLETE:'))
        self.assertNotIn('PASS:', evidence.completion_message(False))
        self.assertTrue(evidence.completion_message(True).startswith('PASS:'))
        (self.root / 'public.txt').write_text('public content')
        for patterns in [[], None, {}]:
            with self.assertRaises(AssertionError): evidence.check_privacy(self.root, {'public.txt'}, patterns)
        self.assertEqual(evidence.check_privacy(self.root, {'public.txt'}, [{'label': 'private', 'regex': 'secret'}]), 1)
        with self.assertRaises(AssertionError): evidence.check_privacy(self.root, {'public.txt'}, [{'label': 'private', 'regex': 'content'}])


class CliChecks(unittest.TestCase):
    def cli(self, *arguments, python_flags=(), optimize=None):
        environment = os.environ.copy()
        environment.pop('PYTHONOPTIMIZE', None)
        if optimize is not None:
            environment['PYTHONOPTIMIZE'] = optimize
        return subprocess.run(
            [sys.executable, *python_flags, SPEC.origin, *arguments],
            env=environment, capture_output=True, text=True, check=False)

    def test_real_cli_refuses_optimization_before_checks_or_help(self):
        for flags, value in [(('-O',), None), (('-OO',), None),
                             ((), '1'), ((), '2'), (('-E',), '1')]:
            with self.subTest(flags=flags, value=value):
                result = self.cli('--help', python_flags=flags, optimize=value)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn('Python optimization disables evidence checks', result.stderr)
                self.assertNotIn('PASS:', result.stdout)

    def test_real_cli_missing_privacy_is_incomplete(self):
        result = self.cli()
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertTrue(result.stdout.startswith('INCOMPLETE: no evidence checks ran'), result.stdout)
        self.assertNotIn('PASS:', result.stdout)

    def test_real_cli_wrong_candidate_fails(self):
        result = self.cli('--privacy-patterns', 'unused.json', '--expect-head', 'invalid-head')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn('wrong candidate HEAD', result.stderr)
        self.assertNotIn('PASS:', result.stdout)

    def test_main_propagates_validation_result_and_failure(self):
        # Exercise the real main dispatcher without manufacturing a full benchmark
        # history. Individual validation checks have fixtures above; this is not
        # a receipt claiming acceptance of the current working tree.
        with patch.dict(os.environ, {}, clear=True), patch.object(sys, 'argv',
                [SPEC.origin, '--privacy-patterns', 'patterns.json']), \
                patch.object(evidence, 'check_evidence', return_value=0) as check:
            self.assertEqual(evidence.main(), 0)
            self.assertEqual(check.call_args.args[0].privacy_patterns, Path('patterns.json'))
            check.side_effect = AssertionError('invalid evidence')
            with self.assertRaisesRegex(AssertionError, 'invalid evidence'):
                evidence.main()
        with patch.dict(os.environ, {}, clear=True), patch.object(sys, 'argv', [SPEC.origin]), \
                patch.object(evidence, 'check_evidence') as check, redirect_stdout(io.StringIO()) as output:
            self.assertEqual(evidence.main(), 2)
            check.assert_not_called()
            self.assertTrue(output.getvalue().startswith('INCOMPLETE:'))


if __name__ == '__main__':
    unittest.main()
