# Validation contract

`cases.json` is the language-neutral contract for the Markdown and link validation currently implemented by `inspect_markdown.py` and `inspect_links.py`.

The Python test runner executes the contract against the existing implementation:

```
python3 -m unittest discover -s tools/validation_tests -p 'test_*.py'
```

It includes direct behavior cases, a historical malformed-link regression from the 2026-07-08 draft, and the ten most recently published issues at the recorded source revision. Cases specify only Markdown inputs, ordered normalized links, and ordered diagnostics so a future Rust implementation can consume the same JSON without depending on Python APIs.

The suite deliberately does not include the current editable draft. It can contain work-in-progress submissions and is expected to be validated by the normal check before publishing.
