use std::sync::Arc;

use django_lsp::server::{Backend, ServerState};
use tokio::io::{stdin, stdout};
use tower_lsp::{LspService, Server};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let (service, socket) = LspService::new(|client| Backend::new(client, Arc::new(ServerState::default())));
    Server::new(stdin(), stdout(), socket).serve(service).await;
}
