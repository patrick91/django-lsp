---
title: Configuration
description: Control how django-lsp discovers and indexes a Django workspace.
section: Reference
order: 1
---

# Configuration

Configuration lives in the Django workspace's `pyproject.toml` under `tool.django-lsp`.

```toml
[tool.django-lsp]
include = ["apps/**"]
exclude = ["apps/generated/**"]
workspace_root = "src"
settings_module = "project.settings.production"
```

All options are optional.

## `include`

Limits indexing to Python files matching at least one glob. Without this option, the complete
workspace is eligible for indexing subject to the exclusion rules.

```toml
include = ["apps/**", "packages/domain/**"]
```

## `exclude`

Adds glob patterns to the built-in exclusions. Virtual environments, dependency directories,
caches, build output, Git metadata, and Django migration directories are excluded by default at any
workspace depth.

```toml
exclude = ["apps/generated/**", "vendor/**"]
```

## `workspace_root`

Changes the Python import root. Relative paths are resolved from the editor workspace; absolute
paths are used directly.

For a `src` layout:

```toml
workspace_root = "src"
```

## `settings_module`

Selects the one module from which settings-like assignments such as `AUTH_USER_MODEL` are read.
This prevents a similarly named constant in an unrelated module from changing model resolution.

```toml
settings_module = "project.settings.production"
```

Without this option, only a module whose final component is `settings`, such as
`project.settings`, contributes settings.
