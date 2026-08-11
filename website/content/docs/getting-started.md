---
title: Getting started
description: Install django-lsp and connect it to Cursor, Visual Studio Code, or Zed.
section: Start
order: 1
---

# Getting started

## Requirements

- an editor or plugin that can launch a language server over standard input and output
- a Django workspace containing statically declared models

Django itself is not required to run the language server. The server reads Python source without
importing the project.

## Install the server

Visual Studio Code and Cursor users can skip this step because the platform-specific extension
packages bundle the server. For Zed, another LSP client, or extension development, the recommended
installation uses `uv` to keep the executable isolated from project dependencies:

```console
uv tool install django-lsp
```

You can also use `pipx install django-lsp` or `python -m pip install django-lsp`. Verify the command
is on your path before configuring an editor:

```console
django-lsp --version
```

Upgrade an existing `uv` installation with `uv tool upgrade django-lsp`.

## Build from source

Building from source requires Rust 1.95 or newer. From the repository root:

```console
cargo build --release
```

The resulting executable is `target/release/django-lsp`.

For a faster development build, use `cargo build` and point the client at
`target/debug/django-lsp`.

## Connect an editor

Configure a Python language-server entry with:

- command: `django-lsp`, or the absolute path to the executable if it is not on the editor's path
- arguments: none
- transport: standard input and output
- workspace root: the Django project root

The server writes diagnostics and lifecycle logging to standard error so standard output remains a
valid LSP stream.

### Visual Studio Code and Cursor

Install **Django ORM Language Server** from the built-in Extensions view. The extension is
published on the
[Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=patrick91.django-lsp)
for Visual Studio Code and [Open VSX](https://open-vsx.org/extension/patrick91/django-lsp) for
Cursor. It attaches `django-lsp` to Python files without replacing Pylance, Pyright, Ruff, or
another general Python language server.

The platform-specific packages include the matching native server, so no separate Python or Rust
installation is required on supported macOS, Linux, and Windows systems. To install manually,
download the VSIX for your platform from the
[latest GitHub release](https://github.com/patrick91/django-lsp/releases/latest), then run
**Extensions: Install from VSIX...** from the editor's command palette.

The extension uses `djangoLsp.server.path` when configured, then looks for `django-lsp` on `PATH`,
then uses its bundled executable. Use **django-lsp: Restart Django ORM Language Server** after
changing the executable or project configuration.

In a monorepo or multi-root workspace, the extension starts a client per detected Django project.
It searches upward from each opened Python file for `manage.py`, then `pyproject.toml`, without
leaving the containing workspace folder. Set `djangoLsp.workspaceRoot` to a relative or absolute
path when explicit control is needed.

### Zed

The first-party [Zed extension](https://github.com/patrick91/django-lsp/tree/main/extensions/zed-extension) attaches `django-lsp` to Python files
without replacing Pyright, Pylsp, Ruff, or another general Python language server. Until the
extension is available in Zed's gallery, clone this repository and run **zed: install dev
extension**, selecting `extensions/zed-extension`.

Enable it alongside the rest of your Python language servers:

```json
{
  "languages": {
    "Python": {
      "language_servers": ["django-lsp", "..."]
    }
  }
}
```

The `"..."` entry preserves other registered Python language servers. The extension first checks
for `django-lsp` on `PATH`; otherwise it downloads the executable matching the current platform
from the latest GitHub release.

After the client initializes the server, open a Python file in the workspace and request completion
inside a Django `filter`, `exclude`, or `get` call:

```python
Blog.objects.filter(author__te)
```

The completion list should include paths such as `author__team`.

## Next steps

- Browse the generated [completion examples](/docs/completions/).
- Add project-specific indexing rules in [configuration](/docs/configuration/).
- Use the protocol and documentation checks described in [testing](/docs/testing/) when contributing.
