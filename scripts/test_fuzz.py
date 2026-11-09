"""Verify selected fuzz targets retain replay, budgets and failure propagation."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('fuzz.sh').resolve()


class FuzzRunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for target in ('first', 'second'):
            seed = self.root / 'fuzz/seeds' / target
            seed.mkdir(parents=True)
            (seed / 'seed').write_text('reviewed input')
        binary = self.root / 'bin'; binary.mkdir()
        cargo = binary / 'cargo'
        cargo.write_text('''#!/usr/bin/env python3
import json, os, sys
with open(os.environ['RECORD'], 'a') as record:
    record.write(json.dumps(sys.argv[1:]) + '\\n')
if sys.argv[1:] == ['+nightly', 'fuzz', 'list']:
    print(os.environ.get('TARGETS', 'first\\nsecond'))
if 'run' in sys.argv and os.environ.get('FAIL_RUN'):
    raise SystemExit(8)
''')
        cargo.chmod(0o755)
        self.env = {**os.environ, 'PATH': str(binary) + os.pathsep + os.environ['PATH'], 'RECORD': str(self.root / 'record')}

    def run_script(self, *args):
        return subprocess.run(['bash', str(SCRIPT), *args], env=self.env, cwd=self.root, capture_output=True)

    def runs(self):
        return [args for line in (self.root / 'record').read_text().splitlines()
                if 'run' in (args := json.loads(line))]

    def test_default_runs_all_targets_with_replay_and_same_limits(self):
        self.assertEqual(self.run_script().returncode, 0)
        runs = self.runs()
        self.assertEqual([r[3] for r in runs], ['first', 'first', 'second', 'second'])
        self.assertIn('-runs=0', runs[0]); self.assertIn('-max_total_time=30', runs[1])
        for run in runs:
            self.assertTrue({'-max_len=4096', '-timeout=10', '-rss_limit_mb=2048'} <= set(run))

    def test_selection_still_replays_reviewed_seeds(self):
        self.assertEqual(self.run_script('30', 'second').returncode, 0)
        self.assertEqual([r[3] for r in self.runs()], ['second', 'second'])
        self.assertEqual((self.root / 'fuzz/corpus/second/seed').read_text(), 'reviewed input')
        self.assertFalse((self.root / 'fuzz/corpus/first').exists())

    def test_bad_selection_or_duration_and_campaign_failure_cannot_pass(self):
        for args in (('0',), ('30', 'missing'), ('30', 'first', 'first'), ('30', '../first')):
            self.assertNotEqual(self.run_script(*args).returncode, 0)
        self.env['FAIL_RUN'] = '1'
        self.assertEqual(self.run_script('30', 'first').returncode, 8)
        self.env['TARGETS'] = ''
        self.assertNotEqual(self.run_script().returncode, 0)


if __name__ == '__main__':
    unittest.main()
