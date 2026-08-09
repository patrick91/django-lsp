# Django ORM Language Server

Static Django ORM query completions for Visual Studio Code and Cursor, powered by Rust.

`django-lsp` understands model fields, relationships, and query lookups without importing or
executing your Django project. It runs alongside Pylance, Pyright, Ruff, or another general Python
language server and contributes only Django-specific completions.

```python
Blog.objects.filter(author__team__name__icontains="Django")
```

## Features

- Model field completions in `filter`, `exclude`, and `get`
- Forward, reverse, and recursive relationship traversal
- Django field lookups such as `contains`, `icontains`, and `isnull`
- Relation completions in `select_related` and `prefetch_related`
- `AUTH_USER_MODEL`, package re-exports, monorepos, and multi-root workspaces
- Unsaved editor buffer updates

Explore the [visual completion examples](https://django-lsp.patrick.wtf/docs/completions) for the
current supported query patterns.

## Installation

In Visual Studio Code, install **Django ORM Language Server** from the
[Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=patrick91.django-lsp).

Cursor uses an Open VSX-compatible extension gallery. Until the Open VSX publication is available,
download the VSIX for your platform from the [latest GitHub release](https://github.com/patrick91/django-lsp/releases/latest),
then run **Extensions: Install from VSIX...** from Cursor's command palette.

The platform-specific Marketplace and release packages include the native `django-lsp` server, so
no separate Python or Rust installation is required on supported macOS, Linux, and Windows systems.

Open a Django project containing `manage.py` or `pyproject.toml`, then start typing inside a
supported ORM query. The extension starts automatically for Python files.

## Server resolution

The extension resolves the server in this order:

1. The executable configured in `djangoLsp.server.path`.
2. `django-lsp` from the VS Code extension host's `PATH`.
3. The executable bundled in a platform-specific VSIX.

Install the server from PyPI only when using the universal development package or an unsupported
platform:

```console
uv tool install django-lsp
```

If VS Code does not inherit the shell's `PATH`, configure the absolute path:

```json
{
  "djangoLsp.server.path": "/absolute/path/to/django-lsp"
}
```

Use **django-lsp: Restart Django ORM Language Server** after changing the executable. The
`djangoLsp.trace.server` setting enables protocol tracing for troubleshooting.

## Monorepos and multi-root workspaces

The extension starts one language server for each detected Django project instead of attaching the
whole editor to the first workspace folder. For every opened Python file it looks upward, without
leaving that file's workspace folder, for:

1. `manage.py`
2. `pyproject.toml`
3. the workspace folder itself as a fallback

Override detection per workspace folder when a project uses another layout:

```json
{
  "djangoLsp.workspaceRoot": "backend"
}
```

Relative values resolve from the containing workspace folder; absolute paths are also supported.

## Troubleshooting

Open **View: Output**, select **django-lsp**, and check the server log. Set
`djangoLsp.trace.server` to `messages` or `verbose` when protocol tracing is needed.

Use **django-lsp: Restart Django ORM Language Server** after changing the executable or workspace
root. See the complete [configuration guide](https://django-lsp.patrick.wtf/docs/configuration) for
project indexing options.

## Development

Build an installable universal VSIX from the repository:

```console
cd extensions/vscode-extension
npm ci
npm run package:universal
code --install-extension dist/django-lsp-universal.vsix
```

The universal development package expects `django-lsp` on `PATH` or an explicit executable path in
settings.

Run the TypeScript tests, type checker, and production bundler:

```console
npm test
npm run typecheck
npm run bundle
```

Create a platform-specific package with a matching server executable:

```console
npm run package:platform -- darwin-arm64 /path/to/django-lsp dist
```

CI packages macOS Intel and Apple Silicon, Linux x86-64 and ARM64, and Windows x86-64 from the
native binaries already tested by the release workflow.

The extension version tracks the server version because each platform package contains that exact
server release. Bump `Cargo.toml`, `package.json`, and `package-lock.json` together before tagging;
the release workflow rejects a tag if the versions differ.

## License

MIT
