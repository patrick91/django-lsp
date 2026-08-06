use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{DjangoLspError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSnapshot {
    pub text: String,
    pub version: i32,
}

#[derive(Debug, Clone, Default)]
pub struct DocumentStore {
    documents: HashMap<PathBuf, DocumentSnapshot>,
}

impl DocumentStore {
    pub fn open(&mut self, path: PathBuf, version: i32, text: String) {
        self.documents
            .insert(path, DocumentSnapshot { text, version });
    }

    pub fn update(&mut self, path: PathBuf, version: i32, text: String) {
        self.documents
            .insert(path, DocumentSnapshot { text, version });
    }

    pub fn close(&mut self, path: &Path) {
        self.documents.remove(path);
    }

    pub fn get(&self, path: &Path) -> Option<&DocumentSnapshot> {
        self.documents.get(path)
    }

    pub fn source_for_path(&self, path: &Path) -> Result<String> {
        if let Some(snapshot) = self.get(path) {
            return Ok(snapshot.text.clone());
        }

        fs::read_to_string(path)
            .map_err(|source| DjangoLspError::io(path.display().to_string(), source))
    }
}
