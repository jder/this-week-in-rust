"""Run the language-neutral validation contract against the Python checkers."""

import contextlib
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
import warnings

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))

import inspect_links  # noqa: E402
import inspect_markdown  # noqa: E402
import markdown  # noqa: E402


CASES = json.loads((Path(__file__).with_name("cases.json")).read_text())


@contextlib.contextmanager
def working_directory(path):
    previous = Path.cwd()
    os.chdir(path)
    try:
        yield
    finally:
        os.chdir(previous)


class ValidationContractTests(unittest.TestCase):
    maxDiff = None

    def setUp(self):
        # unittest enables ResourceWarning after importing this module, while
        # the legacy checkers intentionally keep their file-opening behavior.
        warnings.simplefilter("ignore", ResourceWarning)
        inspect_links.diagnostics.drain_errors_and_warnings()

    def assert_diagnostics(self, case):
        errors, warnings = inspect_links.diagnostics.drain_errors_and_warnings()
        self.assertEqual(errors, case["errors"])
        self.assertEqual(warnings, case["warnings"])

    def test_link_cases(self):
        for case in CASES["link_cases"]:
            with self.subTest(case=case["id"]):
                links = inspect_links.extract_links(markdown.markdown(case["markdown"]))
                if "links" in case:
                    self.assertEqual(links, case["links"])
                self.assert_diagnostics(case)

    def test_duplicate_cases(self):
        for case in CASES["duplicate_cases"]:
            with self.subTest(case=case["id"]), tempfile.TemporaryDirectory() as directory:
                directory = Path(directory)
                for filename, text in case["files"].items():
                    (directory / filename).write_text(text)
                with working_directory(directory):
                    inspect_links.inspect_files(list(case["files"]))
                self.assert_diagnostics(case)

    def test_markdown_cases(self):
        for case in CASES["markdown_cases"]:
            with self.subTest(case=case["id"]), tempfile.TemporaryDirectory() as directory:
                directory = Path(directory)
                with working_directory(directory):
                    Path("case.md").write_text(case["markdown"])
                    html = inspect_markdown.render_file("case.md")
                    inspect_markdown.check_tags(html, "case.md")
                self.assert_diagnostics(case)

    def test_recent_published_corpus(self):
        for case in CASES["corpus_cases"]:
            with self.subTest(case=case["id"]):
                paths = [str(ROOT / path) for path in case["paths"]]
                self.assertTrue(all(Path(path).is_file() for path in paths))
                inspect_links.inspect_files(paths)
                for path in paths:
                    html = inspect_markdown.render_file(path)
                    inspect_markdown.check_tags(html, path)
                self.assert_diagnostics(case)


if __name__ == "__main__":
    unittest.main()
