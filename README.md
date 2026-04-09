# django-lsp

`django-lsp` is a Rust language server focused on Django ORM query completions.

It currently uses Ruff's Python parser for static analysis and is intentionally narrow in scope:

- workspace model indexing
- relation traversal
- completion inside `filter(...)`, `exclude(...)`, and `get(...)`

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
