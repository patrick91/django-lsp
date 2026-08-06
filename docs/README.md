# django-lsp documentation

The documentation is organized around the path from installing the server to understanding and
testing its completion behavior.

## Guides

1. [Getting started](getting-started.md) — build the binary and connect an LSP client.
2. [Completion examples](completions.md) — see query completions produced by the real server.
3. [Configuration](configuration.md) — control indexing for a Django workspace.
4. [Testing](testing.md) — run unit, protocol, executable documentation, and compatibility tests.

## Project boundaries

`django-lsp` is intentionally a focused Django query-completion server. It statically analyzes
Python source and does not initialize Django, connect to a database, or execute project code.

The current non-goals are:

- Django runtime introspection
- general Python language features
- full type-aware filtering of every Django lookup
- support for every dynamic import or model-loading pattern

## Executable documentation

[Completion examples](completions.md) keeps each authored scenario and rendered result together in
one Markdown file. The hidden scenario source is sent through the same JSON-RPC language server used
by editors, so the visible completion menus are checked rather than copied by hand. See
[Testing](testing.md#executable-completion-examples) for the authoring and update workflow.
