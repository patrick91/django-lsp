use std::io;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, DjangoLspError>;

#[derive(Debug, Error)]
pub enum DjangoLspError {
    #[error("failed to read `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse `{path}` as TOML: {source}")]
    Toml {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid glob pattern `{pattern}`: {source}")]
    GlobPattern {
        pattern: String,
        #[source]
        source: globset::Error,
    },
    #[error("invalid file URI `{0}`")]
    InvalidFileUri(String),
}

impl DjangoLspError {
    pub fn io(path: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub fn toml(path: impl Into<String>, source: toml::de::Error) -> Self {
        Self::Toml {
            path: path.into(),
            source,
        }
    }

    pub fn glob(pattern: impl Into<String>, source: globset::Error) -> Self {
        Self::GlobPattern {
            pattern: pattern.into(),
            source,
        }
    }
}
