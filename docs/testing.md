# Testing

The test suite is split into layers so fast static-analysis checks remain easy to run while protocol
and Django compatibility behavior are still exercised end to end.

## Rust quality suite

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Unit tests cover indexing, import resolution, relation traversal, completion ranking, configuration,
and LSP position conversion.

## Protocol tests

`tests/lsp_protocol.rs` uses the project in `tests/fixtures/django_project` and exercises two paths:

- an in-process JSON-RPC service covering initialization, document open/change/close events,
  completion requests, and unsaved model changes
- the compiled `django-lsp` executable using real `Content-Length` framing over standard input and
  output

Run only this layer with:

```console
cargo test --test lsp_protocol
```

### Real-project responsiveness benchmark

The protocol suite also contains an ignored benchmark for measuring initialization and the request
barrier immediately after document open and change notifications against a real Django workspace.
Point it at the directory containing `manage.py`:

```console
DJANGO_LSP_BENCHMARK_ROOT=/path/to/django/backend \
  cargo test --test lsp_protocol benchmarks_real_workspace_responsiveness \
  -- --ignored --nocapture
```

Set `DJANGO_LSP_BENCHMARK_DOCUMENT` when the representative document is somewhere other than
`$DJANGO_LSP_BENCHMARK_ROOT/manage.py`. Run the benchmark several times so the first cold filesystem
scan remains distinguishable from warm runs.

## Executable completion examples

The human-facing [completion examples](completions.md) contain hidden scenario directives next to
their visible generated output. Each directive contains normal Python plus one `<cursor>` marker:

````markdown
<!-- django-lsp-example file=blog/views.py limit=8
from .models import Blog

Blog.objects.filter(
    author__te<cursor>
)
-->
<!-- django-lsp-output:start -->
```text
The renderer writes the completion menu here.
```
<!-- django-lsp-output:end -->
````

The renderer opens that source through the real language server, requests completion at the marker,
and rewrites only the adjacent output block. The scenario and its result therefore remain together
in the existing documentation page.

Regenerate the checked-in page after an intentional completion change:

```console
cargo run --bin render-docs
```

Verify that generated pages are current without modifying them:

```console
cargo run --bin render-docs -- --check
```

`tests/markdown_docs.rs` runs the check as part of `cargo test --all-targets`.

## Editor extensions

Validate the Zed extension from its directory:

```console
cd extensions/zed-extension
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Validate and package the universal Visual Studio Code development extension from its directory:

```console
cd extensions/vscode-extension
npm ci
npm run check
npm run package:universal
```

The universal VSIX intentionally contains no server executable. It tests the same client code used
by release packages while resolving `django-lsp` from the configured path or `PATH`.

## Django compatibility matrix

CI validates the fixture and protocol tests against the latest compatible patch releases in the
Django 5.2, 6.0, and 6.1 series. To run one series locally:

```console
cd tests/fixtures/django_project
uv run --no-project --python 3.12 --with 'Django~=6.1.0' python manage.py test
```

The fixture's own Django tests verify that its model and relation metadata remains valid. The Rust
protocol tests then verify that those same patterns produce the expected editor completions.

## Distribution packages

The release workflow builds the same platform wheels used for tagged releases on every pull request:

- Linux x86-64 and ARM64
- macOS Intel and Apple Silicon
- Windows x86-64

Each job installs its wheel with `uv tool install` on a native runner and executes
`django-lsp --version`. Tagged `v*` builds publish those already-tested artifacts to GitHub Releases
and PyPI; pull requests never receive publishing permissions.

The release workflow also packages five platform-specific Visual Studio Code extensions from those
tested executables: Linux x86-64 and ARM64, macOS Intel and Apple Silicon, and Windows x86-64. Every
VSIX contains the server for exactly one VS Code target. Pull requests retain the packages as CI
artifacts; tagged builds add them to the GitHub Release. The VS Code extension and Rust server
versions stay in sync, and the tag release guard verifies both before publishing.
