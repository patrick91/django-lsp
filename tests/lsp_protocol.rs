use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use django_lsp::server::{Backend, ServerState};
use serde_json::{Value, json};
use tower::{Service, ServiceExt};
use tower_lsp_server::LspService;
use tower_lsp_server::jsonrpc::{Request, Response};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/django_project")
}

fn file_uri(path: &Path) -> String {
    let path = path.canonicalize().unwrap();
    format!("file://{}", path.display())
}

fn position_after(source: &str, needle: &str) -> Value {
    let offset = source.find(needle).unwrap() + needle.len();
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let character = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, line)| line)
        .encode_utf16()
        .count();

    json!({"line": line, "character": character})
}

async fn send(service: &mut LspService<Backend>, request: Request) -> Option<Response> {
    service.ready().await.unwrap().call(request).await.unwrap()
}

fn completion_labels(response: &Response) -> Vec<&str> {
    response
        .result()
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["label"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn completes_a_django_project_over_json_rpc() {
    let root = fixture_root();
    let root_uri = file_uri(&root);
    let state = Arc::new(ServerState::default());
    let service_state = state.clone();
    let (mut service, _socket) = LspService::new(move |client| Backend::new(client, service_state));

    let initialize = Request::build("initialize")
        .params(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {},
        }))
        .id(1)
        .finish();
    let response = send(&mut service, initialize).await.unwrap();
    assert!(response.is_ok());
    assert!(response.result().unwrap()["capabilities"]["completionProvider"].is_object());

    let initialized = Request::build("initialized").params(json!({})).finish();
    assert!(send(&mut service, initialized).await.is_none());

    let views_path = root.join("blog/views.py");
    let views_uri = file_uri(&views_path);
    let views_source = fs::read_to_string(&views_path).unwrap();
    let did_open = Request::build("textDocument/didOpen")
        .params(json!({
            "textDocument": {
                "uri": views_uri,
                "languageId": "python",
                "version": 1,
                "text": views_source,
            }
        }))
        .finish();
    assert!(send(&mut service, did_open).await.is_none());

    let author_completion = Request::build("textDocument/completion")
        .params(json!({
            "textDocument": {"uri": views_uri},
            "position": position_after(&views_source, "author__te"),
        }))
        .id(2)
        .finish();
    let response = send(&mut service, author_completion).await.unwrap();
    let labels = completion_labels(&response);
    assert!(labels.contains(&"author__team"));
    let team = response
        .result()
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["label"] == "author__team")
        .unwrap();
    assert_eq!(team["textEdit"]["newText"], "team");

    let user_completion = Request::build("textDocument/completion")
        .params(json!({
            "textDocument": {"uri": views_uri},
            "position": position_after(&views_source, "installer__time"),
        }))
        .id(3)
        .finish();
    let response = send(&mut service, user_completion).await.unwrap();
    assert!(completion_labels(&response).contains(&"installer__timezone"));

    let models_path = root.join("blog/models.py");
    let models_uri = file_uri(&models_path);
    let models_source = fs::read_to_string(&models_path).unwrap();
    let did_open = Request::build("textDocument/didOpen")
        .params(json!({
            "textDocument": {
                "uri": models_uri,
                "languageId": "python",
                "version": 1,
                "text": models_source,
            }
        }))
        .finish();
    assert!(send(&mut service, did_open).await.is_none());

    let changed_models = models_source.replacen(
        "    title = models.CharField(max_length=255)",
        "    title = models.CharField(max_length=255)\n    summary = models.TextField()",
        1,
    );
    let did_change = Request::build("textDocument/didChange")
        .params(json!({
            "textDocument": {"uri": models_uri, "version": 2},
            "contentChanges": [{"text": changed_models}],
        }))
        .finish();
    assert!(send(&mut service, did_change).await.is_none());

    let changed_views = views_source.replace("author__te", "summ");
    let did_change = Request::build("textDocument/didChange")
        .params(json!({
            "textDocument": {"uri": views_uri, "version": 2},
            "contentChanges": [{"text": changed_views}],
        }))
        .finish();
    assert!(send(&mut service, did_change).await.is_none());

    let unsaved_completion = Request::build("textDocument/completion")
        .params(json!({
            "textDocument": {"uri": views_uri},
            "position": position_after(&changed_views, "summ"),
        }))
        .id(4)
        .finish();
    let response = send(&mut service, unsaved_completion).await.unwrap();
    assert!(completion_labels(&response).contains(&"summary"));

    let did_close = Request::build("textDocument/didClose")
        .params(json!({"textDocument": {"uri": models_uri}}))
        .finish();
    assert!(send(&mut service, did_close).await.is_none());

    let reverted_completion = Request::build("textDocument/completion")
        .params(json!({
            "textDocument": {"uri": views_uri},
            "position": position_after(&changed_views, "summ"),
        }))
        .id(5)
        .finish();
    let response = send(&mut service, reverted_completion).await.unwrap();
    assert_eq!(response.result(), Some(&Value::Null));
}

fn frame(message: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(message).unwrap();
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend(body);
    framed
}

fn read_frame(reader: &mut impl BufRead) -> Value {
    let mut content_length = None;

    loop {
        let mut header = String::new();
        assert_ne!(reader.read_line(&mut header).unwrap(), 0);
        if header == "\r\n" {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length: ") {
            content_length = Some(value.trim().parse::<usize>().unwrap());
        }
    }

    let mut body = vec![0; content_length.unwrap()];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn serves_framed_json_rpc_over_stdio() {
    let root_uri = file_uri(&fixture_root());
    let mut child = Command::new(env!("CARGO_BIN_EXE_django-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {},
        },
    });
    stdin.write_all(&frame(&initialize)).unwrap();
    stdin.flush().unwrap();

    let initialize = read_frame(&mut stdout);
    assert_eq!(initialize["id"], 1);
    assert!(initialize["result"]["capabilities"]["completionProvider"].is_object());

    let shutdown = json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"});
    stdin.write_all(&frame(&shutdown)).unwrap();
    stdin.flush().unwrap();

    let shutdown = read_frame(&mut stdout);
    assert_eq!(shutdown, json!({"jsonrpc": "2.0", "id": 2, "result": null}));

    let exit = json!({"jsonrpc": "2.0", "method": "exit"});
    stdin.write_all(&frame(&exit)).unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    let status = child.wait().unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(status.success(), "language server failed: {stderr}");
}
