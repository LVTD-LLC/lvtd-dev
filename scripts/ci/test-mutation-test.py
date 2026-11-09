"""Exercise the CI helper against real Git histories and a recording Cargo double."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("mutation-test.sh").resolve()


class MutationHelperTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.test")
        for directory in ("app/src", "nested/app/src"):
            path = self.repo / directory
            path.mkdir(parents=True)
            (path / "lib.rs").write_text("pub fn check() -> bool { true }\n")
        self.git("add", ".")
        self.git("commit", "-qm", "base")
        self.base = self.git("rev-parse", "HEAD").strip()
        for directory in ("app/src", "nested/app/src"):
            (self.repo / directory / "lib.rs").write_text("pub fn check() -> bool { false }\n")
        binary = self.repo / "bin"
        binary.mkdir()
        cargo = binary / "cargo"
        cargo.write_text("""#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
patch = pathlib.Path(args[args.index('--in-diff') + 1]).read_text() if '--in-diff' in args else None
pathlib.Path(os.environ['RECORD']).write_text(json.dumps({'args': args, 'patch': patch, 'offline': os.environ.get('SQLX_OFFLINE'), 'target_dir': os.environ.get('CARGO_TARGET_DIR')}))
sys.exit(int(os.environ.get('CARGO_EXIT', '0')))
""")
        cargo.chmod(0o755)
        self.env = {**os.environ, "PATH": str(binary) + os.pathsep + os.environ["PATH"],
                    "CARGO_TARGET_DIR": str(self.repo / "shared-target"),
                    "DATABASE_URL": "postgres://fixture", "RECORD": str(self.repo / "record.json")}

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.repo, text=True)

    def run_helper(self, *args, nested=False):
        return subprocess.run(["bash", str(SCRIPT), *args], cwd=self.repo / "nested" if nested else self.repo,
                              env=self.env, capture_output=True, text=True)

    def record(self):
        return json.loads((self.repo / "record.json").read_text())

    def test_root_diff_includes_working_tree(self):
        self.assertEqual(self.run_helper(self.base).returncode, 0)
        self.assertIn("b/app/src/lib.rs", self.record()["patch"])
        self.assertIn("+pub fn check() -> bool { false }", self.record()["patch"])
        self.assertEqual(self.record()["offline"], "true")

    def test_nested_diff_is_workspace_relative(self):
        self.assertEqual(self.run_helper(self.base, nested=True).returncode, 0)
        self.assertIn("b/app/src/lib.rs", self.record()["patch"])
        self.assertNotIn("nested/", self.record()["patch"])
        self.assertEqual(self.record()["patch"].count("diff --git"), 1)

    def test_untracked_rust_is_included_in_both_workspace_locations(self):
        for directory in ("app/src", "nested/app/src"):
            (self.repo / directory / "new.rs").write_text("pub fn new() -> bool { true }\n")
        for nested in (False, True):
            self.assertEqual(self.run_helper(self.base, nested=nested).returncode, 0)
            self.assertIn("b/app/src/new.rs", self.record()["patch"])
            self.assertIn("+pub fn new() -> bool { true }", self.record()["patch"])
            if nested:
                self.assertNotIn("nested/", self.record()["patch"])

    def test_full_audit_and_argument_forwarding(self):
        self.assertEqual(self.run_helper("", "--file", "app/src/*.rs").returncode, 0)
        self.assertEqual(self.record()["args"], ["mutants", "--workspace", "--file", "app/src/*.rs"])
        self.assertIsNone(self.record()["patch"])

    def test_failures_are_not_swallowed(self):
        for code in (2, 3, 4, 5, 6, 70):
            for args in ((), (self.base,)):
                with self.subTest(code=code, args=args):
                    self.env["CARGO_EXIT"] = str(code)
                    self.assertEqual(self.run_helper(*args).returncode, code)

    def test_mutations_never_use_the_shared_build_cache(self):
        for args in ((), (self.base,)):
            with self.subTest(args=args):
                self.assertEqual(self.run_helper(*args).returncode, 0)
                self.assertEqual(self.record()["target_dir"], "target")
                self.assertFalse((self.repo / "shared-target").exists())

    def test_invalid_base_fails_before_cargo(self):
        self.assertNotEqual(self.run_helper("nonexistent-revision").returncode, 0)
        self.assertFalse((self.repo / "record.json").exists())

    def test_missing_database_fails_before_cargo(self):
        del self.env["DATABASE_URL"]
        self.assertNotEqual(self.run_helper(self.base).returncode, 0)
        self.assertFalse((self.repo / "record.json").exists())


if __name__ == "__main__":
    unittest.main()
