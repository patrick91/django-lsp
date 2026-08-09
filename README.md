# django-lsp

`django-lsp` is a Rust language server focused on Django ORM query completions. It uses Ruff's
Python parser to build a static workspace index and follows Django model relations without importing
or executing the project.

```python
from .models import Blog

Blog.objects.filter(author__team__name__icontains="Django")
```

## Installation

Install the language server from PyPI with `uv`:

```console
uv tool install django-lsp
```

`pipx install django-lsp` and `python -m pip install django-lsp` are also supported. Confirm the
installed executable is available with:

```console
django-lsp --version
```

Point an editor's LSP client at the `django-lsp` command, with no arguments. The server uses standard
input and output for LSP communication.

### Visual Studio Code

The repository includes a [Visual Studio Code extension](extensions/vscode-extension) that runs
`django-lsp` alongside Pylance, Pyright, Ruff, or another general Python language server. It uses a
configured executable, one already installed on `PATH`, or the executable bundled into a
platform-specific VSIX. See the extension README for development installation and packaging.

### Zed

The repository includes a [Zed extension](extensions/zed-extension) that registers `django-lsp`
alongside a general Python language server. It uses a server already installed on `PATH`, or
downloads the matching executable from the latest GitHub release. See the extension README for
development installation while its extension-gallery submission is in progress.

## Documentation

- [Getting started](website/content/docs/getting-started.md) covers installing, building, and
  connecting an editor.
- [Completion examples](website/content/docs/completions.md) shows executable examples generated
  from the real LSP.
- [Configuration](website/content/docs/configuration.md) documents `pyproject.toml` options.
- [Testing](website/content/docs/testing.md) explains the Rust, protocol, documentation, and
  Django compatibility test layers.

The website uses CrossDocs, the same documentation framework as Cross-Inertia. Preview the complete
FastAPI and React application locally at `http://localhost:8000` with:

```console
cd website
uv sync --locked
bun install --frozen-lockfile
bun run serve
```

## Current scope

- workspace model indexing
- completion inside `filter(...)`, `exclude(...)`, `get(...)`, `select_related(...)`, and
  `prefetch_related(...)`
- forward, reverse, and recursive relation traversal
- `AUTH_USER_MODEL` support
- models re-exported from package `__init__.py` modules
- function-local and dotted import resolution
- unsaved editor buffer updates

## Development

Building from source requires Rust 1.95 or newer.

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo run --bin render-docs -- --check
cargo test --all-targets
```

Validate the documentation website separately with:

```console
cd website
uv sync --locked
bun install --frozen-lockfile
bun run check
bun run build
```

The project intentionally does not provide Django runtime introspection, general Python language
features, or exhaustive support for dynamic model-loading patterns.
