import importlib.util
from pathlib import Path
import unittest
import tempfile

path = Path(__file__).with_name('compare-long-rss.py')
spec = importlib.util.spec_from_file_location('rss_snapshot_compare', path)
compare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compare)
analysis_spec = importlib.util.spec_from_file_location(
    'rss_page_breakdown', Path(__file__).with_name('rss_page_breakdown.py'))
analysis = importlib.util.module_from_spec(analysis_spec)
analysis_spec.loader.exec_module(analysis)


class SnapshotValidation(unittest.TestCase):
    log = ('RSS_ACCOUNTING status_kib=6904 page_table_kib=7540\n'
           'RSS_ACCOUNTING status_kib=6904 page_table_kib=7604\n')

    def test_page_table_snapshot_accepted(self):
        compare.validate_snapshot({'rss_steady_kib': 7540}, self.log)

    def test_fallback_rejected(self):
        with self.assertRaises(RuntimeError):
            compare.validate_snapshot({'rss_steady_kib': 7540},
                                      self.log + 'RSS_ACCOUNTING_FAILED\n')

    def test_missing_readings_rejected(self):
        for log in ['', self.log.splitlines()[0]]:
            with self.assertRaises(RuntimeError):
                compare.validate_snapshot({'rss_steady_kib': 7540}, log)

    def test_disagreement_rejected(self):
        with self.assertRaises(RuntimeError):
            compare.validate_snapshot({'rss_steady_kib': 6904}, self.log)

    def test_timeout_rejected(self):
        with self.assertRaises(RuntimeError):
            compare.validate_snapshot(None, 'timed out')

    def test_no_unrelated_scenarios(self):
        self.assertEqual(set(compare.paired.SCENARIOS), {'closed64'})

    def test_different_round_ids_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / 'normal').mkdir()
            for name, rounds in [('base', [0, 1]), ('final', [0, 2])]:
                for number in rounds:
                    (root / 'normal' / f'closed64-{name}-{number:02}.log').write_text(
                        'RSS_ACCOUNTING status_kib=9 page_table_kib=10 anonymous_kib=1\n')
            with self.assertRaises(RuntimeError):
                analysis.analyse(root)


if __name__ == '__main__':
    unittest.main()
