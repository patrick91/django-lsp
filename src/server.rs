use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::{
    CompletionOptions, CompletionResponse, Diagnostic, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializeResult, InitializedParams, MessageType, NumberOrString, Position,
    Range, ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
};
use tower_lsp_server::{Client, LanguageServer};
use tracing::{info, warn};

use crate::analysis::AnalysisDatabase;
use crate::completion::complete_lsp_items_from_analysis;
use crate::config::DjangoLspConfig;
use crate::error::DjangoLspError;

#[derive(Debug)]
pub struct Backend {
    client: Client,
    state: Arc<ServerState>,
}

#[derive(Debug, Default)]
pub struct ServerState {
    inner: Mutex<ServerSnapshot>,
}

#[derive(Debug, Default)]
struct ServerSnapshot {
    workspace_root: Option<PathBuf>,
    config: DjangoLspConfig,
    database: AnalysisDatabase,
}

impl Backend {
    pub fn new(client: Client, state: Arc<ServerState>) -> Self {
        Self { client, state }
    }

    async fn sync_document(
        &self,
        path: PathBuf,
        contents: Option<String>,
        uri: tower_lsp_server::ls_types::Uri,
        version: i32,
    ) {
        let result = {
            let mut snapshot = self.state.inner.lock().await;
            snapshot
                .database
                .sync_path(path.clone(), contents)
                .map(|_| {
                    let source = snapshot.database.source_for_path(&path).unwrap_or_default();
                    snapshot.database.diagnostics_for_path(&path).map_or_else(
                        Vec::new,
                        |diagnostics| {
                            diagnostics
                                .iter()
                                .map(|diagnostic| Diagnostic {
                                    range: offsets_to_range(
                                        source,
                                        diagnostic.range.start().to_usize(),
                                        diagnostic.range.end().to_usize(),
                                    ),
                                    severity: Some(DiagnosticSeverity::WARNING),
                                    code: Some(NumberOrString::String(diagnostic.code.to_string())),
                                    source: Some("django-lsp".to_string()),
                                    message: diagnostic.message.clone(),
                                    ..Diagnostic::default()
                                })
                                .collect()
                        },
                    )
                })
        };
        match result {
            Ok(diagnostics) => {
                self.client
                    .publish_diagnostics(uri, diagnostics, Some(version))
                    .await;
            }
            Err(error) => {
                warn!("analysis input update failed: {error}");
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("django-lsp failed to update its analysis inputs: {error}"),
                    )
                    .await;
            }
        }
    }

    async fn restore_document_from_disk(&self, path: PathBuf) {
        let result = self
            .state
            .inner
            .lock()
            .await
            .database
            .sync_path_from_disk(path);
        if let Err(error) = result {
            warn!("analysis input restore failed: {error}");
            self.client
                .log_message(
                    MessageType::ERROR,
                    format!("django-lsp failed to restore its analysis inputs: {error}"),
                )
                .await;
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
        let started = Instant::now();
        let database = if let Some(root) = workspace_root.clone() {
            let build_config = config.clone();
            match tokio::task::spawn_blocking(move || AnalysisDatabase::build(&root, build_config))
                .await
            {
                Ok(Ok(database)) => database,
                Ok(Err(error)) => {
                    warn!("initial analysis database build failed: {error}");
                    AnalysisDatabase::empty(workspace_root.clone().unwrap(), config.clone())
                }
                Err(error) => {
                    warn!("initial analysis database task failed: {error}");
                    AnalysisDatabase::empty(workspace_root.clone().unwrap(), config.clone())
                }
            }
        } else {
            AnalysisDatabase::empty(PathBuf::from("."), config.clone())
        };
        info!(
            analyzed_files = database.analyzed_file_count(),
            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
            "analysis database initialized"
        );

        let mut snapshot = self.state.inner.lock().await;
        snapshot.workspace_root = workspace_root;
        snapshot.config = config;
        snapshot.database = database;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        "_".to_string(),
                        "\"".to_string(),
                        "'".to_string(),
                    ]),
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
        let uri = params.text_document.uri;
        let Ok(path) = Self::path_from_uri(&uri) else {
            return;
        };
        let text = params.text_document.text;
        self.sync_document(path, Some(text), uri, params.text_document.version)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let Ok(path) = Self::path_from_uri(&uri) else {
            return;
        };
        let Some(change) = params.content_changes.into_iter().next() else {
            return;
        };

        self.sync_document(path, Some(change.text), uri, params.text_document.version)
            .await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        let Ok(path) = Self::path_from_uri(&uri) else {
            return;
        };

        self.restore_document_from_disk(path).await;
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn completion(
        &self,
        params: tower_lsp_server::ls_types::CompletionParams,
    ) -> Result<Option<CompletionResponse>> {
        let Ok(path) = Self::path_from_uri(&params.text_document_position.text_document.uri) else {
            return Ok(None);
        };

        let snapshot = self.state.inner.lock().await;
        let Some(source) = snapshot.database.source_for_path(&path) else {
            return Ok(None);
        };
        let Some(analysis) = snapshot.database.analysis_for_path(&path) else {
            return Ok(None);
        };
        let cursor = position_to_offset(source, params.text_document_position.position);
        let items =
            complete_lsp_items_from_analysis(snapshot.database.index(), analysis, source, cursor);
        if items.is_empty() {
            Ok(None)
        } else {
            Ok(Some(CompletionResponse::Array(items)))
        }
    }
}

fn position_to_offset(source: &str, position: Position) -> usize {
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

fn offsets_to_range(source: &str, start: usize, end: usize) -> Range {
    Range {
        start: offset_to_position(source, start),
        end: offset_to_position(source, end),
    }
}

fn offset_to_position(source: &str, offset: usize) -> Position {
    let mut line = 0u32;
    let mut column = 0u32;
    let mut seen = 0usize;

    for ch in source.chars() {
        if seen >= offset {
            break;
        }
        let len = ch.len_utf8();
        if seen + len > offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 0;
        } else {
            column += ch.len_utf16() as u32;
        }
        seen += len;
    }

    Position::new(line, column)
}

#[cfg(test)]
mod tests {
    use tower_lsp_server::ls_types::Position;

    use super::{offset_to_position, position_to_offset};

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

    #[test]
    fn converts_byte_offsets_to_utf16_positions() {
        let source = "a😀b\ncafé";

        assert_eq!(offset_to_position(source, 0), Position::new(0, 0));
        assert_eq!(offset_to_position(source, 5), Position::new(0, 3));
        assert_eq!(
            offset_to_position(source, source.len()),
            Position::new(1, 4)
        );
    }
}
