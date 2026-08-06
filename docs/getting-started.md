# Getting started

## Requirements

- Rust 1.95 or newer
- an editor or plugin that can launch a language server over standard input and output
- a Django workspace containing statically declared models

Django itself is not required to run the language server. The server reads Python source without
importing the project.

## Build the server

From the repository root:

```console
cargo build --release
```

The resulting executable is `target/release/django-lsp`.

For a faster development build, use `cargo build` and point the client at
`target/debug/django-lsp`.

## Connect an editor

Configure a Python language-server entry with:

- command: the absolute path to the `django-lsp` executable
- arguments: none
- transport: standard input and output
- workspace root: the Django project root

The server writes diagnostics and lifecycle logging to standard error so standard output remains a
valid LSP stream.

After the client initializes the server, open a Python file in the workspace and request completion
inside a Django `filter`, `exclude`, or `get` call:

```python
Blog.objects.filter(author__te)
```

The completion list should include paths such as `author__team`.

## Next steps

- Browse the generated [completion examples](completions.md).
- Add project-specific indexing rules in [configuration](configuration.md).
- Use the protocol and documentation checks described in [testing](testing.md) when contributing.
