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


CASES_DIRECTORY = Path(__file__).with_name("cases")


def cases():
    for path in sorted(CASES_DIRECTORY.glob("*.md")):
        documents = path.read_text().split("\n---\n")
        metadata = json.loads(documents[0])
        filenames = metadata["files"]
        if len(filenames) != len(documents) - 1:
            raise ValueError(
                f"{path}: expected {len(filenames)} Markdown documents, "
                f"found {len(documents) - 1}"
            )
        yield path, metadata, list(zip(filenames, documents[1:]))


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

    def assert_diagnostics(self, metadata):
        errors, warning_messages = inspect_links.diagnostics.drain_errors_and_warnings()
        self.assertEqual(errors, metadata.get("errors", []))
        self.assertEqual(warning_messages, metadata.get("warnings", []))

    def test_cases(self):
        for path, metadata, files in cases():
            with self.subTest(case=path.stem), tempfile.TemporaryDirectory() as directory:
                directory = Path(directory)
                for filename, text in files:
                    (directory / filename).write_text(text)
                filenames = [filename for filename, _text in files]
                with working_directory(directory):
                    links = inspect_links.inspect_files(filenames)
                    for filename in filenames:
                        html = inspect_markdown.render_file(filename)
                        inspect_markdown.check_tags(html, filename)
                if "links" in metadata:
                    self.assertEqual(links, metadata["links"])
                self.assert_diagnostics(metadata)

    def test_ten_most_recent_published_issues_are_clean(self):
        paths = inspect_links.get_recent_files(str(ROOT / "content"), 10)
        self.assertEqual(len(paths), 10)
        inspect_links.inspect_files(paths)
        for path in paths:
            html = inspect_markdown.render_file(path)
            inspect_markdown.check_tags(html, path)
        self.assert_diagnostics({})


if __name__ == "__main__":
    unittest.main()
