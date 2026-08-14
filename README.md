# django-lsp

`django-lsp` is a Rust language server and command-line checker focused on Django ORM queries. It
uses Ruff's Python parser and an incremental Salsa database to follow Django model relations without
importing or executing the project.

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
input and output for LSP communication. Editors supporting LSP 3.17 pull diagnostics receive
`DJ001` warnings for both open documents and unopened files across the workspace.

Run the same repeated-query analysis from a terminal with:

```console
django-lsp check
django-lsp check path/to/views.py
django-lsp check --format github
```

The checker exits with status 0 when no diagnostics are found, 1 when it reports warnings, and 2
for invalid arguments or analysis failures. Use `--format json` for structured output or
`--format github` for native GitHub Actions annotations. Suppress an intentional warning on its
source line with `# django-lsp: ignore[DJ001]`.

### Visual Studio Code

Install **Django ORM Language Server** from the built-in Extensions view in Visual Studio Code or
Cursor. It is published on the
[Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=patrick91.django-lsp)
and [Open VSX](https://open-vsx.org/extension/patrick91/django-lsp). The extension runs `django-lsp`
alongside Pylance, Pyright, Ruff, or another general Python language server and bundles the native
server for supported platforms.

Platform-specific VSIX files are also available from the
[latest GitHub release](https://github.com/patrick91/django-lsp/releases/latest). See the
[extension README](extensions/vscode-extension) for configuration and development installation.

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
- `DJ001` warnings for missing `select_related()` or `prefetch_related()` across QuerySet loops,
  comprehensions, collected QuerySets, related managers, and helper calls, available through both
  the LSP and `django-lsp check`
- bounded cross-module call summaries, custom QuerySet chains, collection wrappers, typed model
  parameters, and typed QuerySet-returning functions
- Django admin display-method awareness, including eager loading declared by `get_queryset()`
- Django 6.1 `FETCH_PEERS` and `RAISE` fetch-mode awareness for single-valued relations
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
