use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{DjangoLspError, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DjangoLspConfig {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub workspace_root: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct PyProject {
    tool: Option<ToolSection>,
}

#[derive(Debug, Deserialize)]
struct ToolSection {
    #[serde(rename = "django-lsp")]
    django_lsp: Option<RawDjangoLspConfig>,
}

#[derive(Debug, Deserialize)]
struct RawDjangoLspConfig {
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    workspace_root: Option<String>,
}

impl DjangoLspConfig {
    pub fn load(workspace_root: &Path) -> Result<Self> {
        let pyproject_path = workspace_root.join("pyproject.toml");
        if !pyproject_path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(&pyproject_path)
            .map_err(|source| DjangoLspError::io(pyproject_path.display().to_string(), source))?;
        let parsed: PyProject =
            toml::from_str(&contents).map_err(|source| DjangoLspError::toml(pyproject_path.display().to_string(), source))?;

        let Some(raw) = parsed.tool.and_then(|tool| tool.django_lsp) else {
            return Ok(Self::default());
        };

        Ok(Self {
            include: raw.include.unwrap_or_default(),
            exclude: raw.exclude.unwrap_or_default(),
            workspace_root: raw.workspace_root.map(PathBuf::from),
        })
    }

    pub fn effective_root(&self, workspace_root: &Path) -> PathBuf {
        self.workspace_root
            .as_ref()
            .map(|configured| {
                if configured.is_absolute() {
                    configured.clone()
                } else {
                    workspace_root.join(configured)
                }
            })
            .unwrap_or_else(|| workspace_root.to_path_buf())
    }
}
