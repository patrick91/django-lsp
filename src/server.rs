use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::{
    CompletionOptions, CompletionResponse, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, InitializeParams, InitializeResult, InitializedParams, MessageType,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
};
use tower_lsp_server::{Client, LanguageServer};
use tracing::{debug, info, warn};

use crate::completion::complete_lsp_items;
use crate::config::DjangoLspConfig;
use crate::document_store::DocumentStore;
use crate::error::DjangoLspError;
use crate::index::WorkspaceIndex;

const INDEX_REFRESH_DEBOUNCE: Duration = Duration::from_millis(150);

#[derive(Debug)]
pub struct Backend {
    client: Client,
    state: Arc<ServerState>,
}

#[derive(Debug, Default)]
pub struct ServerState {
    inner: RwLock<ServerSnapshot>,
    refresh: Mutex<RefreshQueue>,
}

#[derive(Debug, Default)]
struct RefreshQueue {
    generation: u64,
    pending_paths: HashSet<PathBuf>,
    task: Option<JoinHandle<()>>,
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

    async fn update_document_and_schedule<F>(&self, path: PathBuf, update: F)
    where
        F: FnOnce(&mut DocumentStore, &Path) + Send,
    {
        let state = Arc::downgrade(&self.state);
        let client = self.client.clone();
        let mut refresh = self.state.refresh.lock().await;
        refresh.generation += 1;
        let generation = refresh.generation;
        refresh.pending_paths.insert(path.clone());

        if let Some(task) = refresh.task.take() {
            task.abort();
        }

        {
            let mut snapshot = self.state.inner.write().await;
            update(&mut snapshot.documents, &path);
        }

        refresh.task = Some(tokio::spawn(async move {
            sleep(INDEX_REFRESH_DEBOUNCE).await;
            let Some(state) = state.upgrade() else {
                return;
            };

            let paths = {
                let refresh = state.refresh.lock().await;
                if refresh.generation != generation {
                    return;
                }
                refresh.pending_paths.iter().cloned().collect::<Vec<_>>()
            };
            let (mut index, documents) = {
                let snapshot = state.inner.read().await;
                (snapshot.index.clone(), snapshot.documents.clone())
            };
            let started = Instant::now();
            let result = tokio::task::spawn_blocking(move || {
                let refreshed_files = index.refresh_paths(&paths, &documents)?;
                Ok::<_, DjangoLspError>((index, paths, refreshed_files))
            })
            .await;

            let (index, paths, refreshed_files) = match result {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    warn!("index refresh failed: {error}");
                    client
                        .log_message(
                            MessageType::ERROR,
                            format!("django-lsp failed to refresh index: {error}"),
                        )
                        .await;
                    return;
                }
                Err(error) => {
                    warn!("index refresh task failed: {error}");
                    client
                        .log_message(
                            MessageType::ERROR,
                            format!("django-lsp index refresh task failed: {error}"),
                        )
                        .await;
                    return;
                }
            };

            let elapsed = started.elapsed();
            let mut refresh = state.refresh.lock().await;
            if refresh.generation != generation {
                debug!(generation, "discarded stale workspace index refresh");
                return;
            }

            {
                let mut snapshot = state.inner.write().await;
                snapshot.index = index;
            }
            for path in &paths {
                refresh.pending_paths.remove(path);
            }
            debug!(
                generation,
                refreshed_files,
                elapsed_ms = elapsed.as_secs_f64() * 1_000.0,
                "workspace index refreshed"
            );
        }));
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
        let started = Instant::now();
        let index = if let Some(root) = workspace_root.clone() {
            let build_config = config.clone();
            match tokio::task::spawn_blocking(move || {
                WorkspaceIndex::build(&root, build_config, &DocumentStore::default())
            })
            .await
            {
                Ok(Ok(index)) => index,
                Ok(Err(error)) => {
                    warn!("initial index build failed: {error}");
                    WorkspaceIndex::empty(workspace_root.clone().unwrap(), config.clone())
                }
                Err(error) => {
                    warn!("initial index build task failed: {error}");
                    WorkspaceIndex::empty(workspace_root.clone().unwrap(), config.clone())
                }
            }
        } else {
            WorkspaceIndex::empty(PathBuf::from("."), config.clone())
        };
        info!(
            analyzed_files = index.analyzed_file_count(),
            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
            "workspace index initialized"
        );

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
        let version = params.text_document.version;
        let text = params.text_document.text;
        self.update_document_and_schedule(path, move |documents, path| {
            documents.open(path.to_path_buf(), version, text);
        })
        .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let Ok(path) = Self::path_from_uri(&params.text_document.uri) else {
            return;
        };
        let Some(change) = params.content_changes.into_iter().next() else {
            return;
        };

        let version = params.text_document.version;
        self.update_document_and_schedule(path, move |documents, path| {
            documents.update(path.to_path_buf(), version, change.text);
        })
        .await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let Ok(path) = Self::path_from_uri(&params.text_document.uri) else {
            return;
        };

        self.update_document_and_schedule(path, |documents, path| {
            documents.close(path);
        })
        .await;
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
