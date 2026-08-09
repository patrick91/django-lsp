# Editor extensions

Editor integrations keep `django-lsp` focused on the Language Server Protocol while providing
one-click installation and lifecycle management in each editor.

| Editor | Source | Status |
| --- | --- | --- |
| Zed | [`zed-extension`](zed-extension) | Registry submission in review |
| VS Code | [`vscode-extension`](vscode-extension) | Marketplace publication in progress |

Extensions should run `django-lsp` alongside a general Python language server. They are responsible
only for locating or packaging the executable and connecting it over standard input and output.

Tagged releases publish the five platform-specific VS Code packages through GitHub Actions and
Visual Studio Marketplace trusted publishing. The Marketplace policy must allow repository owner
`patrick91`, repository `django-lsp`, and workflow `release.yml`; the workflow does not use a
long-lived publishing token.
