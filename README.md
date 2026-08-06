# django-lsp

`django-lsp` is a Rust language server focused on Django ORM query completions.

It currently uses Ruff's Python parser for static analysis and is intentionally narrow in scope:

- workspace model indexing
- relation traversal
- completion inside `filter(...)`, `exclude(...)`, and `get(...)`

## Building and Running

`django-lsp` requires Rust 1.95 or newer.

```console
cargo build --release
```

The language server communicates over standard input and output. Point your editor's LSP client at
`target/release/django-lsp`, with no arguments.

For local development, run the full quality suite with:

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

The protocol tests exercise both an in-process JSON-RPC service and the compiled binary over real
LSP `Content-Length` framing. They initialize the server against the project in
`tests/fixtures/django_project`, request relation completions, apply unsaved document changes, and
verify shutdown behavior.

CI also checks that fixture and the protocol tests against the latest compatible patch releases of
Django 5.2, 6.0, and 6.1. To run one of those combinations locally:

```console
cd tests/fixtures/django_project
uv run --no-project --python 3.12 --with 'Django~=6.1.0' python manage.py test
```

## Configuration

Configuration lives in the Django workspace's `pyproject.toml`:

```toml
[tool.django-lsp]
include = ["apps/**"]
exclude = ["apps/generated/**"]
workspace_root = "src"
settings_module = "project.settings.production"
```

All keys are optional:

- `include` limits indexing to matching Python files.
- `exclude` adds patterns to the built-in environment, cache, build, and migration exclusions.
- `workspace_root` changes the Python import root, relative to the editor workspace unless absolute.
- `settings_module` identifies the single module from which Django settings should be read. Without
  it, only a module whose final component is `settings` (for example `project.settings`) contributes
  settings.

## Completion Model

Completions are query-expression oriented, not kwarg-name-only.

That means these are both intended:

```python
Blog.objects.filter(title__icontains="hello")
```

```python
Blog.objects.filter(ti)
```

At `ti`, the server may suggest:

- `title`
- `title__exact`
- `title__icontains`

This is deliberate. The goal is to help build Django query expressions anywhere inside the query call, not only after a keyword boundary.

## Current Scope

- Django model detection from static analysis
- forward and reverse relation completions
- `AUTH_USER_MODEL` support from settings-like assignments
- function-local import resolution
- recursive descendant-path suggestions up to a bounded depth

## Non-Goals for Now

- Django runtime introspection
- full type-aware lookup filtering
- support for every dynamic import or model-loading pattern
- general Python language features outside Django query completion
