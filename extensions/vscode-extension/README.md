# django-lsp for Visual Studio Code

This extension runs [`django-lsp`](https://github.com/patrick91/django-lsp) alongside Pylance,
Pyright, Ruff, or another general Python language server. It attaches only to Python files and adds
Django ORM query completions.

## Development installation

Until the extension is published to the Visual Studio Marketplace and Open VSX, build an
installable VSIX from the repository:

```console
cd extensions/vscode-extension
npm ci
npm run package:universal
code --install-extension dist/django-lsp-universal.vsix
```

The universal development package expects `django-lsp` on `PATH` or an explicit executable path in
settings. Tagged releases will also produce platform-specific VSIX packages containing the server.

## Server resolution

The extension resolves the server in this order:

1. The executable configured in `djangoLsp.server.path`.
2. `django-lsp` from the VS Code extension host's `PATH`.
3. The executable bundled in a platform-specific VSIX.

Install the server from PyPI when using the universal development package:

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

## Development

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
