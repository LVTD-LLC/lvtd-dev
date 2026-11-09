"""The summary must distinguish branch coverage from line coverage."""
import importlib.util
import pathlib
import unittest

spec = importlib.util.spec_from_file_location(
    "coverage_summary", pathlib.Path(__file__).with_name("coverage-summary.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def report(covered=2, count=4):
    branches = {"covered": covered, "count": count}
    return {"type": "llvm.coverage.json.export", "data": [{
        "totals": {"branches": branches, "lines": {"percent": 100}},
        "files": [{"filename": "app/src/lib.rs", "summary": {"branches": branches}}],
    }]}


class CoverageSummaryTests(unittest.TestCase):
    def test_full_line_coverage_does_not_hide_untested_branches(self):
        text = module.summarize(report())
        self.assertIn("**50.00%**", text)
        self.assertIn("2 uncovered", text)
        self.assertIn("app/src/lib.rs | 2 / 4 | 50.00%", text)

    def test_missing_and_empty_instrumentation_fail(self):
        for data in ({}, report(0, 0)):
            with self.assertRaises((ValueError, KeyError)):
                module.summarize(data)
        data = report()
        del data["data"][0]["totals"]["branches"]
        with self.assertRaises(KeyError):
            module.summarize(data)

    def test_invalid_counts_fail(self):
        for covered, count in ((5, 4), (-1, 4), (0, -1), ("2", 4)):
            with self.assertRaises(ValueError):
                module.summarize(report(covered, count))

    def test_no_covered_branches_is_reported_honestly(self):
        self.assertIn("**0.00%**", module.summarize(report(0, 4)))


if __name__ == "__main__":
    unittest.main()
