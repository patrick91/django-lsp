# Editor extensions

Editor integrations keep `django-lsp` focused on the Language Server Protocol while providing
one-click installation and lifecycle management in each editor.

| Editor | Source | Status |
| --- | --- | --- |
| Zed | [`zed-extension`](zed-extension) | Registry submission in review |
| VS Code and Cursor | [`vscode-extension`](vscode-extension) | Published on the [Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=patrick91.django-lsp) and [Open VSX](https://open-vsx.org/extension/patrick91/django-lsp) |

Extensions should run `django-lsp` alongside a general Python language server. They are responsible
only for locating or packaging the executable and connecting it over standard input and output.

Tagged releases publish the five platform-specific VS Code packages to both the Visual Studio
Marketplace and Open VSX. The Release workflow can also be dispatched manually to publish the
current version to Open VSX without republishing the Python package or GitHub release.

Visual Studio Marketplace publishing uses Microsoft Entra workload identity federation. The
`vscode-marketplace` GitHub environment needs two Actions variables: `AZURE_CLIENT_ID` and
`AZURE_TENANT_ID`.

The client ID belongs to a user-assigned Azure managed identity with:

- a federated credential for the subject
  `repo:patrick91/django-lsp:environment:vscode-marketplace` and audience
  `api://AzureADTokenExchange`; and
- Contributor access to the `patrick91` Visual Studio Marketplace publisher.

No long-lived publishing token is stored in GitHub. The workflow exchanges GitHub's short-lived
OIDC token for a Microsoft Entra token and passes that identity to `vsce --azure-credential`.

Open VSX publishing uses the `patrick91` namespace. The `open-vsx` GitHub environment needs an
environment secret named `OVSX_PAT`, containing a dedicated Open VSX access token. The workflow
passes it to the pinned `ovsx` CLI through its supported `OVSX_PAT` environment variable.
