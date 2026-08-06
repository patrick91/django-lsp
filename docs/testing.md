# Testing

The test suite is split into layers so fast static-analysis checks remain easy to run while protocol
and Django compatibility behavior are still exercised end to end.

## Rust quality suite

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Unit tests cover indexing, import resolution, relation traversal, completion ranking, configuration,
and LSP position conversion.

## Protocol tests

`tests/lsp_protocol.rs` uses the project in `tests/fixtures/django_project` and exercises two paths:

- an in-process JSON-RPC service covering initialization, document open/change/close events,
  completion requests, and unsaved model changes
- the compiled `django-lsp` executable using real `Content-Length` framing over standard input and
  output

Run only this layer with:

```console
cargo test --test lsp_protocol
```

## Executable completion examples

The human-facing [completion examples](completions.md) are generated from Markdown in
`docs/src/completions.md`. A `django-lsp` fence contains normal Python plus one `<cursor>` marker:

````markdown
```django-lsp file=blog/views.py limit=8
from .models import Blog

Blog.objects.filter(
    author__te<cursor>
)
```
````

The renderer opens that source through the real language server, requests completion at the marker,
and writes a text representation of the returned menu.

Regenerate the checked-in page after an intentional completion change:

```console
cargo run --bin render-docs
```

Verify that generated pages are current without modifying them:

```console
cargo run --bin render-docs -- --check
```

`tests/markdown_docs.rs` runs the check as part of `cargo test --all-targets`.

## Django compatibility matrix

CI validates the fixture and protocol tests against the latest compatible patch releases in the
Django 5.2, 6.0, and 6.1 series. To run one series locally:

```console
cd tests/fixtures/django_project
uv run --no-project --python 3.12 --with 'Django~=6.1.0' python manage.py test
```

The fixture's own Django tests verify that its model and relation metadata remains valid. The Rust
protocol tests then verify that those same patterns produce the expected editor completions.
