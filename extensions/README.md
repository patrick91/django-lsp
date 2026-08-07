# Editor extensions

Editor integrations keep `django-lsp` focused on the Language Server Protocol while providing
one-click installation and lifecycle management in each editor.

| Editor | Source | Status |
| --- | --- | --- |
| Zed | [`zed-extension`](zed-extension) | Ready for development installation |
| VS Code | [`vscode-extension`](vscode-extension) | Ready for development installation |

Extensions should run `django-lsp` alongside a general Python language server. They are responsible
only for locating or packaging the executable and connecting it over standard input and output.
