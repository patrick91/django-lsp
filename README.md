# django-lsp

`django-lsp` is a Rust language server focused on Django ORM query completions. It uses Ruff's
Python parser to build a static workspace index and follows Django model relations without importing
or executing the project.

```python
from .models import Blog

Blog.objects.filter(author__team__name__icontains="Django")
```

## Quick start

`django-lsp` requires Rust 1.95 or newer.

```console
cargo build --release
```

Point an editor's LSP client at `target/release/django-lsp`, with no arguments. The server uses
standard input and output for LSP communication.

## Documentation

- [Getting started](docs/getting-started.md) covers building and connecting an editor.
- [Completion examples](docs/completions.md) shows executable examples generated from the real LSP.
- [Configuration](docs/configuration.md) documents `pyproject.toml` options.
- [Testing](docs/testing.md) explains the Rust, protocol, documentation, and Django compatibility
  test layers.
- [Documentation index](docs/README.md) provides the complete guide map.

## Current scope

- workspace model indexing
- completion inside `filter(...)`, `exclude(...)`, and `get(...)`
- forward, reverse, and recursive relation traversal
- `AUTH_USER_MODEL` support
- function-local and dotted import resolution
- unsaved editor buffer updates

## Development

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo run --bin render-docs -- --check
cargo test --all-targets
```

The project intentionally does not provide Django runtime introspection, general Python language
features, or exhaustive support for dynamic model-loading patterns.
