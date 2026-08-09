# Editor extensions

Editor integrations keep `django-lsp` focused on the Language Server Protocol while providing
one-click installation and lifecycle management in each editor.

| Editor | Source | Status |
| --- | --- | --- |
| Zed | [`zed-extension`](zed-extension) | Registry submission in review |
| VS Code | [`vscode-extension`](vscode-extension) | Marketplace publication in progress |

Extensions should run `django-lsp` alongside a general Python language server. They are responsible
only for locating or packaging the executable and connecting it over standard input and output.

Tagged releases publish the five platform-specific VS Code packages through GitHub Actions using
Microsoft Entra workload identity federation. The `vscode-marketplace` GitHub environment needs
two Actions variables: `AZURE_CLIENT_ID` and `AZURE_TENANT_ID`.

The client ID belongs to a user-assigned Azure managed identity with:

- a federated credential for the subject
  `repo:patrick91/django-lsp:environment:vscode-marketplace` and audience
  `api://AzureADTokenExchange`; and
- Contributor access to the `patrick91` Visual Studio Marketplace publisher.

No long-lived publishing token is stored in GitHub. The workflow exchanges GitHub's short-lived
OIDC token for a Microsoft Entra token and passes that identity to `vsce --azure-credential`.
