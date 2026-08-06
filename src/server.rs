use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::{
    CompletionOptions, CompletionResponse, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, InitializeParams, InitializeResult, InitializedParams, MessageType,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
};
use tower_lsp_server::{Client, LanguageServer};
use tracing::warn;

use crate::completion::complete_lsp_items;
use crate::config::DjangoLspConfig;
use crate::document_store::DocumentStore;
use crate::error::DjangoLspError;
use crate::index::WorkspaceIndex;

#[derive(Debug)]
pub struct Backend {
    client: Client,
    state: Arc<ServerState>,
}

#[derive(Debug, Default)]
pub struct ServerState {
    inner: RwLock<ServerSnapshot>,
}

#[derive(Debug, Default)]
struct ServerSnapshot {
    workspace_root: Option<PathBuf>,
    config: DjangoLspConfig,
    documents: DocumentStore,
    index: WorkspaceIndex,
}

impl Backend {
    pub fn new(client: Client, state: Arc<ServerState>) -> Self {
        Self { client, state }
    }

    async fn rebuild_index(&self) {
        let (workspace_root, config, documents) = {
            let snapshot = self.state.inner.read().await;
            (
                snapshot.workspace_root.clone(),
                snapshot.config.clone(),
                snapshot.documents.clone(),
            )
        };

        let Some(workspace_root) = workspace_root else {
            return;
        };

        match WorkspaceIndex::build(&workspace_root, config.clone(), &documents) {
            Ok(index) => {
                let mut snapshot = self.state.inner.write().await;
                snapshot.index = index;
            }
            Err(error) => {
                warn!("index rebuild failed: {error}");
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("django-lsp failed to refresh index: {error}"),
                    )
                    .await;
            }
        }
    }

    #[allow(deprecated)]
    fn workspace_root_from_initialize(params: &InitializeParams) -> Option<PathBuf> {
        params
            .workspace_folders
            .as_ref()
            .and_then(|folders| folders.first())
            .and_then(|folder| folder.uri.to_file_path())
            .map(|path| path.into_owned())
            .or_else(|| {
                params
                    .root_uri
                    .as_ref()
                    .and_then(|uri| uri.to_file_path())
                    .map(|path| path.into_owned())
            })
    }

    fn path_from_uri(
        uri: &tower_lsp_server::ls_types::Uri,
    ) -> std::result::Result<PathBuf, DjangoLspError> {
        uri.to_file_path()
            .map(|path| path.into_owned())
            .ok_or_else(|| DjangoLspError::InvalidFileUri(uri.to_string()))
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let workspace_root = Self::workspace_root_from_initialize(&params);
        let config = if let Some(workspace_root) = &workspace_root {
            DjangoLspConfig::load(workspace_root).unwrap_or_default()
        } else {
            DjangoLspConfig::default()
        };
        let index = workspace_root
            .as_ref()
            .and_then(|workspace_root| {
                WorkspaceIndex::build(workspace_root, config.clone(), &DocumentStore::default())
                    .ok()
            })
            .unwrap_or_else(|| {
                WorkspaceIndex::empty(
                    workspace_root.clone().unwrap_or_else(|| PathBuf::from(".")),
                    config.clone(),
                )
            });

        let mut snapshot = self.state.inner.write().await;
        snapshot.workspace_root = workspace_root;
        snapshot.config = config;
        snapshot.index = index;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec!["_".to_string()]),
                    ..CompletionOptions::default()
                }),
                ..ServerCapabilities::default()
            },
            ..InitializeResult::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "django-lsp initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let Ok(path) = Self::path_from_uri(&params.text_document.uri) else {
            return;
        };

        {
            let mut snapshot = self.state.inner.write().await;
            snapshot.documents.open(
                path,
                params.text_document.version,
                params.text_document.text,
            );
        }

        self.rebuild_index().await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let Ok(path) = Self::path_from_uri(&params.text_document.uri) else {
            return;
        };
        let Some(change) = params.content_changes.into_iter().next() else {
            return;
        };

        {
            let mut snapshot = self.state.inner.write().await;
            snapshot
                .documents
                .update(path, params.text_document.version, change.text);
        }

        self.rebuild_index().await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let Ok(path) = Self::path_from_uri(&params.text_document.uri) else {
            return;
        };

        {
            let mut snapshot = self.state.inner.write().await;
            snapshot.documents.close(&path);
        }

        self.rebuild_index().await;
    }

    async fn completion(
        &self,
        params: tower_lsp_server::ls_types::CompletionParams,
    ) -> Result<Option<CompletionResponse>> {
        let Ok(path) = Self::path_from_uri(&params.text_document_position.text_document.uri) else {
            return Ok(None);
        };

        let source = {
            let snapshot = self.state.inner.read().await;
            match snapshot.documents.source_for_path(&path) {
                Ok(source) => source,
                Err(_) => return Ok(None),
            }
        };

        let cursor = position_to_offset(&source, params.text_document_position.position);
        let snapshot = self.state.inner.read().await;
        let items = complete_lsp_items(&snapshot.index, Path::new(&path), &source, cursor);
        if items.is_empty() {
            Ok(None)
        } else {
            Ok(Some(CompletionResponse::Array(items)))
        }
    }
}

fn position_to_offset(source: &str, position: tower_lsp_server::ls_types::Position) -> usize {
    let mut line = 0u32;
    let mut column = 0u32;
    let mut offset = 0usize;

    for ch in source.chars() {
        if line == position.line && column >= position.character {
            break;
        }

        if ch == '\n' {
            if line == position.line {
                break;
            }
            line += 1;
            column = 0;
            offset += ch.len_utf8();
            continue;
        }

        if line == position.line {
            let next_column = column + ch.len_utf16() as u32;
            if next_column > position.character {
                break;
            }
            column = next_column;
        }
        offset += ch.len_utf8();
    }

    offset
}

#[cfg(test)]
mod tests {
    use tower_lsp_server::ls_types::Position;

    use super::position_to_offset;

    #[test]
    fn converts_utf16_positions_to_byte_offsets() {
        let source = "a😀b\ncafé";

        assert_eq!(position_to_offset(source, Position::new(0, 0)), 0);
        assert_eq!(position_to_offset(source, Position::new(0, 1)), 1);
        assert_eq!(position_to_offset(source, Position::new(0, 3)), 5);
        assert_eq!(
            position_to_offset(source, Position::new(1, 4)),
            source.len()
        );
    }
}
