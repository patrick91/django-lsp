use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::{
    CompletionOptions, CompletionResponse, Diagnostic, DiagnosticOptions,
    DiagnosticServerCapabilities, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DocumentDiagnosticParams,
    DocumentDiagnosticReport, DocumentDiagnosticReportResult, FullDocumentDiagnosticReport,
    InitializeParams, InitializeResult, InitializedParams, MessageType, NumberOrString, Position,
    Range, RelatedFullDocumentDiagnosticReport, RelatedUnchangedDocumentDiagnosticReport,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
    UnchangedDocumentDiagnosticReport, Uri, WorkspaceDiagnosticParams, WorkspaceDiagnosticReport,
    WorkspaceDiagnosticReportResult, WorkspaceDocumentDiagnosticReport,
    WorkspaceFullDocumentDiagnosticReport, WorkspaceUnchangedDocumentDiagnosticReport,
};
use tower_lsp_server::{Client, LanguageServer};
use tracing::{info, warn};

use crate::analysis::AnalysisDatabase;
use crate::completion::complete_lsp_items_from_analysis;
use crate::config::DjangoLspConfig;
use crate::diagnostic::OrmDiagnostic;
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
    uses_pull_diagnostics: bool,
    supports_diagnostic_refresh: bool,
}

impl Backend {
    pub fn new(client: Client, state: Arc<ServerState>) -> Self {
        Self { client, state }
    }

    async fn sync_document(
        &self,
        path: PathBuf,
        contents: Option<String>,
        uri: Uri,
        version: i32,
        refresh_workspace: bool,
    ) {
        let (result, uses_pull_diagnostics, supports_diagnostic_refresh) = {
            let mut snapshot = self.state.inner.lock().await;
            let result = snapshot
                .database
                .sync_path(path.clone(), contents)
                .map(|_| lsp_diagnostics_for_path(&snapshot.database, &path).unwrap_or_default());
            (
                result,
                snapshot.uses_pull_diagnostics,
                snapshot.supports_diagnostic_refresh,
            )
        };
        match result {
            Ok(diagnostics) => {
                if uses_pull_diagnostics {
                    if refresh_workspace && supports_diagnostic_refresh {
                        self.refresh_workspace_diagnostics().await;
                    }
                } else {
                    self.client
                        .publish_diagnostics(uri, diagnostics, Some(version))
                        .await;
                }
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

    async fn restore_document_from_disk(&self, path: PathBuf, uri: Uri) {
        let (result, uses_pull_diagnostics, supports_diagnostic_refresh) = {
            let mut snapshot = self.state.inner.lock().await;
            let result = snapshot.database.sync_path_from_disk(path);
            (
                result,
                snapshot.uses_pull_diagnostics,
                snapshot.supports_diagnostic_refresh,
            )
        };
        if let Err(error) = result {
            warn!("analysis input restore failed: {error}");
            self.client
                .log_message(
                    MessageType::ERROR,
                    format!("django-lsp failed to restore its analysis inputs: {error}"),
                )
                .await;
        }

        if uses_pull_diagnostics {
            if supports_diagnostic_refresh {
                self.refresh_workspace_diagnostics().await;
            }
        } else {
            self.client.publish_diagnostics(uri, Vec::new(), None).await;
        }
    }

    async fn refresh_workspace_diagnostics(&self) {
        if let Err(error) = self.client.workspace_diagnostic_refresh().await {
            warn!("workspace diagnostic refresh failed: {error}");
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

    fn path_from_uri(uri: &Uri) -> std::result::Result<PathBuf, DjangoLspError> {
        uri.to_file_path()
            .map(|path| path.into_owned())
            .ok_or_else(|| DjangoLspError::InvalidFileUri(uri.to_string()))
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let workspace_root = Self::workspace_root_from_initialize(&params);
        let uses_pull_diagnostics = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|capabilities| capabilities.diagnostic.as_ref())
            .is_some();
        let supports_diagnostic_refresh = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|capabilities| capabilities.diagnostics.as_ref())
            .and_then(|capabilities| capabilities.refresh_support)
            .unwrap_or(false);
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
        snapshot.uses_pull_diagnostics = uses_pull_diagnostics;
        snapshot.supports_diagnostic_refresh = supports_diagnostic_refresh;

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
                diagnostic_provider: Some(DiagnosticServerCapabilities::Options(
                    DiagnosticOptions {
                        identifier: Some("django-lsp".to_string()),
                        inter_file_dependencies: true,
                        workspace_diagnostics: true,
                        ..DiagnosticOptions::default()
                    },
                )),
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
        self.sync_document(path, Some(text), uri, params.text_document.version, false)
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

        self.sync_document(
            path,
            Some(change.text),
            uri,
            params.text_document.version,
            true,
        )
        .await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        let Ok(path) = Self::path_from_uri(&uri) else {
            return;
        };

        self.restore_document_from_disk(path, uri).await;
    }

    async fn diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> Result<DocumentDiagnosticReportResult> {
        let Ok(path) = Self::path_from_uri(&params.text_document.uri) else {
            return Ok(full_document_diagnostic_report(Vec::new(), None).into());
        };
        let snapshot = self.state.inner.lock().await;
        let Some((diagnostics, result_id)) =
            diagnostics_and_result_id_for_path(&snapshot.database, &path)
        else {
            return Ok(full_document_diagnostic_report(Vec::new(), None).into());
        };

        if let (Some(result_id), Some(previous_result_id)) =
            (&result_id, &params.previous_result_id)
            && result_id == previous_result_id
        {
            return Ok(DocumentDiagnosticReport::Unchanged(
                RelatedUnchangedDocumentDiagnosticReport {
                    related_documents: None,
                    unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport {
                        result_id: result_id.clone(),
                    },
                },
            )
            .into());
        }

        Ok(full_document_diagnostic_report(diagnostics, result_id).into())
    }

    async fn workspace_diagnostic(
        &self,
        params: WorkspaceDiagnosticParams,
    ) -> Result<WorkspaceDiagnosticReportResult> {
        let mut previous_result_ids = params
            .previous_result_ids
            .into_iter()
            .map(|previous| (previous.uri, previous.value))
            .collect::<HashMap<_, _>>();
        let snapshot = self.state.inner.lock().await;
        let mut items = Vec::new();

        for path in snapshot.database.paths() {
            let Some(uri) = Uri::from_file_path(&path) else {
                warn!(
                    "could not convert diagnostic path to URI: {}",
                    path.display()
                );
                continue;
            };
            let Some((diagnostics, result_id)) =
                diagnostics_and_result_id_for_path(&snapshot.database, &path)
            else {
                continue;
            };
            let previous_result_id = previous_result_ids.remove(&uri);

            if diagnostics.is_empty() {
                if previous_result_id.is_some() {
                    items.push(workspace_full_document_diagnostic_report(
                        uri,
                        Vec::new(),
                        None,
                    ));
                }
                continue;
            }

            if let (Some(result_id), Some(previous_result_id)) = (&result_id, &previous_result_id)
                && result_id == previous_result_id
            {
                items.push(WorkspaceDocumentDiagnosticReport::Unchanged(
                    WorkspaceUnchangedDocumentDiagnosticReport {
                        uri,
                        version: None,
                        unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport {
                            result_id: result_id.clone(),
                        },
                    },
                ));
            } else {
                items.push(workspace_full_document_diagnostic_report(
                    uri,
                    diagnostics,
                    result_id,
                ));
            }
        }

        let mut removed_documents = previous_result_ids.into_keys().collect::<Vec<_>>();
        removed_documents.sort();
        items.extend(
            removed_documents
                .into_iter()
                .map(|uri| workspace_full_document_diagnostic_report(uri, Vec::new(), None)),
        );

        Ok(WorkspaceDiagnosticReportResult::Report(
            WorkspaceDiagnosticReport { items },
        ))
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

fn lsp_diagnostics_for_path(database: &AnalysisDatabase, path: &Path) -> Option<Vec<Diagnostic>> {
    diagnostics_and_result_id_for_path(database, path).map(|(diagnostics, _)| diagnostics)
}

fn diagnostics_and_result_id_for_path(
    database: &AnalysisDatabase,
    path: &Path,
) -> Option<(Vec<Diagnostic>, Option<String>)> {
    let source = database.source_for_path(path)?;
    let diagnostics = database.diagnostics_for_path(path)?;
    let result_id = diagnostic_result_id(diagnostics);
    let diagnostics = diagnostics
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
        .collect();
    Some((diagnostics, result_id))
}

fn diagnostic_result_id(diagnostics: &[OrmDiagnostic]) -> Option<String> {
    if diagnostics.is_empty() {
        return None;
    }

    let mut hasher = DefaultHasher::new();
    for diagnostic in diagnostics {
        diagnostic.code.hash(&mut hasher);
        diagnostic.range.start().to_usize().hash(&mut hasher);
        diagnostic.range.end().to_usize().hash(&mut hasher);
        diagnostic.message.hash(&mut hasher);
        diagnostic.method.hash(&mut hasher);
        diagnostic.relation_path.hash(&mut hasher);
    }
    Some(format!("{:016x}", hasher.finish()))
}

fn full_document_diagnostic_report(
    diagnostics: Vec<Diagnostic>,
    result_id: Option<String>,
) -> DocumentDiagnosticReport {
    DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
        related_documents: None,
        full_document_diagnostic_report: FullDocumentDiagnosticReport {
            result_id,
            items: diagnostics,
        },
    })
}

fn workspace_full_document_diagnostic_report(
    uri: Uri,
    diagnostics: Vec<Diagnostic>,
    result_id: Option<String>,
) -> WorkspaceDocumentDiagnosticReport {
    WorkspaceDocumentDiagnosticReport::Full(WorkspaceFullDocumentDiagnosticReport {
        uri,
        version: None,
        full_document_diagnostic_report: FullDocumentDiagnosticReport {
            result_id,
            items: diagnostics,
        },
    })
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
