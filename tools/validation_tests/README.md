# Validation contract

The files in `cases/` are test cases for markdown and link validation in `inspect_markdown.py` and `inspect_links.py`.

You can run it against those implementations like so:

```
python3 -m unittest discover -s tools/validation_tests -p 'test_*.py'
```

Each case is a multi-document Markdown file. Its first document is JSON metadata with an ordered `files` list and expected diagnostics. Each remaining document supplies the contents of one virtual file named by the corresponding entry in `files`. An optional `links` field checks the ordered normalized links. The standalone `---` line is reserved as the document separator.

Cases in `cases/` run against both the Python and Rust implementations. Cases
in `rust_cases/` exercise Rust-only checks.

The fixtures include direct behavior cases and historical regressions.
`recent_files.json` defines a shared multi-directory ordering case. Separate
tests in both implementations consume that same scenario, and another test
checks that the ten most recent published issues are clean.
