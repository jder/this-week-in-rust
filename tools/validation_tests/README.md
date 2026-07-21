# Validation contract

The files in `cases/` are the language-neutral contract for the Markdown and link validation currently implemented by `inspect_markdown.py` and `inspect_links.py`.

The Python test runner executes the contract against the existing implementation:

```
python3 -m unittest discover -s tools/validation_tests -p 'test_*.py'
```

Each case is a multi-document Markdown file. Its first document is JSON metadata with an ordered `files` list and expected diagnostics. Each remaining document supplies the contents of one virtual file named by the corresponding entry in `files`. An optional `links` field checks the ordered normalized links. The standalone `---` line is reserved as the document separator.

The fixtures include direct behavior cases and historical regressions. A separate test dynamically checks that the ten most recent published issues are clean, without recording a Git revision or file list.

The suite deliberately does not include the current editable draft. It can contain work-in-progress submissions and is expected to be validated by the normal check before publishing.
