# django-lsp for Zed

This extension runs [`django-lsp`](https://github.com/patrick91/django-lsp) alongside Zed's general
Python language servers to provide Django ORM query completions.

It complements Zed's existing Django extension, which provides Django template language support;
this extension attaches only to Python files.

## Development installation

Until the extension is published in Zed's extension gallery:

1. Clone the `django-lsp` repository.
2. In Zed's command palette, run **zed: install dev extension**.
3. Select the repository's `extensions/zed-extension` directory.
4. Add `django-lsp` to the Python language servers in Zed's `settings.json`:

   ```json
   {
     "languages": {
       "Python": {
         "language_servers": ["django-lsp", "..."]
       }
     }
   }
   ```

The `"..."` entry preserves Pyright, Pylsp, Ruff, and any other registered Python language servers.

## Server installation

The extension resolves the server in this order:

1. `django-lsp` already available on the worktree's `PATH`.
2. The executable for the latest GitHub release, downloaded and cached by the extension.

Downloaded servers are stored by release version. After a successful update, older cached versions
are removed.

The extension supports macOS on Intel and Apple Silicon, Linux on x86-64 and ARM64, and Windows on
x86-64.

## Development

Validate the WebAssembly extension from the repository root:

```console
rustup target add wasm32-wasip1
cargo check --locked --target wasm32-wasip1 --manifest-path extensions/zed-extension/Cargo.toml
```

To use a local debug build of the server, build the root crate and override the `django-lsp` binary
with its absolute path in the worktree's `.zed/settings.json`:

```json
{
  "lsp": {
    "django-lsp": {
      "binary": {
        "path": "/absolute/path/to/django-lsp/target/debug/django-lsp",
        "arguments": []
      }
    }
  }
}
```

## License

MIT
