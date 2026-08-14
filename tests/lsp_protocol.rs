use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use django_lsp::server::{Backend, ServerState};
use futures_util::StreamExt;
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
    let (mut service, mut socket) =
        LspService::new(move |client| Backend::new(client, service_state));
    tokio::spawn(async move { while socket.next().await.is_some() {} });

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
    assert_eq!(
        response.result().unwrap()["capabilities"]["codeActionProvider"],
        json!({"codeActionKinds": ["quickfix"]})
    );
    assert!(response.result().unwrap()["capabilities"]["completionProvider"].is_object());
    assert_eq!(
        response.result().unwrap()["capabilities"]["completionProvider"]["triggerCharacters"],
        json!(["_", "\"", "'"])
    );
    assert_eq!(
        response.result().unwrap()["capabilities"]["diagnosticProvider"],
        json!({
            "identifier": "django-lsp",
            "interFileDependencies": true,
            "workspaceDiagnostics": true,
        })
    );

    let initialized = Request::build("initialized").params(json!({})).finish();
    assert!(send(&mut service, initialized).await.is_none());

    let views_path = root.join("blog/views.py");
    let views_uri = file_uri(&views_path);
    let views_source = fs::read_to_string(&views_path).unwrap();
    let did_open = Request::build("textDocument/didOpen")
        .params(json!({
            "textDocument": {
                "uri": views_uri.clone(),
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

    let select_related_completion = Request::build("textDocument/completion")
        .params(json!({
            "textDocument": {"uri": views_uri},
            "position": position_after(&views_source, "\"author__te"),
        }))
        .id(4)
        .finish();
    let response = send(&mut service, select_related_completion).await.unwrap();
    let team = response
        .result()
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["label"] == "author__team")
        .unwrap();
    assert_eq!(team["textEdit"]["newText"], "team");

    let prefetch_related_completion = Request::build("textDocument/completion")
        .params(json!({
            "textDocument": {"uri": views_uri},
            "position": position_after(&views_source, "\"bl"),
        }))
        .id(5)
        .finish();
    let response = send(&mut service, prefetch_related_completion)
        .await
        .unwrap();
    let labels = completion_labels(&response);
    assert!(labels.contains(&"blogs"));
    assert!(!labels.contains(&"blogs__tags"));

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

    let latest_models = models_source.replacen(
        "    title = models.CharField(max_length=255)",
        "    title = models.CharField(max_length=255)\n    description = models.TextField()",
        1,
    );
    let did_change = Request::build("textDocument/didChange")
        .params(json!({
            "textDocument": {"uri": models_uri, "version": 3},
            "contentChanges": [{"text": latest_models}],
        }))
        .finish();
    assert!(send(&mut service, did_change).await.is_none());

    let changed_views = views_source.replace("author__te", "desc");
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
            "position": position_after(&changed_views, "desc"),
        }))
        .id(6)
        .finish();
    let response = send(&mut service, unsaved_completion).await.unwrap();
    assert!(completion_labels(&response).contains(&"description"));
    assert!(!completion_labels(&response).contains(&"summary"));

    let did_close = Request::build("textDocument/didClose")
        .params(json!({"textDocument": {"uri": models_uri}}))
        .finish();
    assert!(send(&mut service, did_close).await.is_none());
    let reverted_completion = Request::build("textDocument/completion")
        .params(json!({
            "textDocument": {"uri": views_uri},
            "position": position_after(&changed_views, "desc"),
        }))
        .id(7)
        .finish();
    let response = send(&mut service, reverted_completion).await.unwrap();
    assert_eq!(response.result(), Some(&Value::Null));
}

#[tokio::test]
async fn pulls_diagnostics_for_unopened_workspace_documents() {
    let directory = tempfile::tempdir().unwrap();
    let blog = directory.path().join("blog");
    fs::create_dir(&blog).unwrap();
    fs::write(blog.join("__init__.py"), "").unwrap();
    fs::write(
        blog.join("models.py"),
        concat!(
            "from django.db import models\n",
            "\n",
            "class Author(models.Model):\n",
            "    email = models.EmailField()\n",
            "\n",
            "class Blog(models.Model):\n",
            "    author = models.ForeignKey(Author, on_delete=models.CASCADE)\n",
            "    editor = models.ForeignKey(Author, on_delete=models.CASCADE, related_name=\"edited_blogs\")\n",
        ),
    )
    .unwrap();
    let diagnostic_source = concat!(
        "from .models import Blog\n",
        "\n",
        "for blog in Blog.objects.all():\n",
        "    print(blog.author.email, blog.editor.email)\n",
    );
    let first_path = blog.join("first.py");
    let second_path = blog.join("second.py");
    fs::write(&first_path, diagnostic_source).unwrap();
    fs::write(&second_path, diagnostic_source).unwrap();

    let root_uri = file_uri(directory.path());
    let first_uri = file_uri(&first_path);
    let second_uri = file_uri(&second_path);
    let state = Arc::new(ServerState::default());
    let service_state = state.clone();
    let (mut service, mut socket) =
        LspService::new(move |client| Backend::new(client, service_state));

    let initialize = Request::build("initialize")
        .params(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {"diagnostic": {}},
                "workspace": {"diagnostics": {"refreshSupport": false}},
            },
        }))
        .id(1)
        .finish();
    let response = send(&mut service, initialize).await.unwrap();
    assert!(response.is_ok());

    let initialized = Request::build("initialized").params(json!({})).finish();
    assert!(send(&mut service, initialized).await.is_none());
    let log_message = socket.next().await.unwrap();
    assert_eq!(log_message.method(), "window/logMessage");

    let workspace_diagnostic = Request::build("workspace/diagnostic")
        .params(json!({
            "identifier": "django-lsp",
            "previousResultIds": [],
            "partialResultToken": "workspace/diagnostic/django-lsp/1",
        }))
        .id(2)
        .finish();
    let response = send(&mut service, workspace_diagnostic).await.unwrap();
    let items = response.result().unwrap()["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(
        items
            .iter()
            .map(|item| item["uri"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [&first_uri, &second_uri]
    );
    for item in items {
        assert_eq!(item["kind"], "full");
        assert_eq!(item["items"][0]["code"], "DJ001");
        assert!(item["resultId"].is_string());
    }

    let document_diagnostic = Request::build("textDocument/diagnostic")
        .params(json!({
            "textDocument": {"uri": first_uri},
            "identifier": "django-lsp",
            "previousResultId": null,
        }))
        .id(3)
        .finish();
    let response = send(&mut service, document_diagnostic).await.unwrap();
    assert_eq!(response.result().unwrap()["kind"], "full");
    assert_eq!(response.result().unwrap()["items"][0]["code"], "DJ001");
    assert_eq!(
        response.result().unwrap()["items"][0]["data"],
        json!({
            "method": "select_related",
            "relation": "author",
            "fixable": true,
        })
    );
    let diagnostic = response.result().unwrap()["items"][0].clone();
    let document_result_id = response.result().unwrap()["resultId"].clone();

    let code_action = Request::build("textDocument/codeAction")
        .params(json!({
            "textDocument": {"uri": first_uri},
            "range": diagnostic["range"],
            "context": {"diagnostics": [diagnostic]},
        }))
        .id(4)
        .finish();
    let response = send(&mut service, code_action).await.unwrap();
    let actions = response.result().unwrap().as_array().unwrap();
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[0]["title"], "Add select_related(\"author\")");
    assert_eq!(actions[0]["kind"], "quickfix");
    assert_eq!(actions[0]["isPreferred"], true);
    assert_eq!(
        actions[0]["edit"]["changes"][&first_uri][0]["newText"],
        ".select_related(\"author\")"
    );
    assert_eq!(
        actions[0]["edit"]["changes"][&first_uri][0]["range"]["start"],
        position_after(diagnostic_source, "Blog.objects.all()")
    );
    assert_eq!(
        actions[1]["title"],
        "Add all missing related loading for this QuerySet"
    );
    assert_eq!(
        actions[1]["edit"]["changes"][&first_uri][0]["newText"],
        ".select_related(\"author\", \"editor\")"
    );
    assert_eq!(actions[1]["diagnostics"].as_array().unwrap().len(), 2);

    let document_diagnostic = Request::build("textDocument/diagnostic")
        .params(json!({
            "textDocument": {"uri": first_uri},
            "identifier": "django-lsp",
            "previousResultId": document_result_id,
        }))
        .id(5)
        .finish();
    let response = send(&mut service, document_diagnostic).await.unwrap();
    assert_eq!(response.result().unwrap()["kind"], "unchanged");

    let previous_result_ids = items
        .iter()
        .map(|item| {
            json!({
                "uri": item["uri"],
                "value": item["resultId"],
            })
        })
        .collect::<Vec<_>>();
    let workspace_diagnostic = Request::build("workspace/diagnostic")
        .params(json!({
            "identifier": "django-lsp",
            "previousResultIds": previous_result_ids,
        }))
        .id(6)
        .finish();
    let response = send(&mut service, workspace_diagnostic).await.unwrap();
    let items = response.result().unwrap()["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|item| item["kind"] == "unchanged"));

    let fixed_source = diagnostic_source.replace(
        "Blog.objects.all()",
        "Blog.objects.select_related(\"author\", \"editor\")",
    );
    let did_open = Request::build("textDocument/didOpen")
        .params(json!({
            "textDocument": {
                "uri": first_uri,
                "languageId": "python",
                "version": 1,
                "text": fixed_source,
            }
        }))
        .finish();
    assert!(send(&mut service, did_open).await.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(25), socket.next())
            .await
            .is_err(),
        "pull-capable clients should not also receive pushed diagnostics"
    );

    let previous_result_ids = items
        .iter()
        .map(|item| {
            json!({
                "uri": item["uri"],
                "value": item["resultId"],
            })
        })
        .collect::<Vec<_>>();
    let workspace_diagnostic = Request::build("workspace/diagnostic")
        .params(json!({
            "identifier": "django-lsp",
            "previousResultIds": previous_result_ids,
        }))
        .id(7)
        .finish();
    let response = send(&mut service, workspace_diagnostic).await.unwrap();
    let items = response.result().unwrap()["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let fixed = items.iter().find(|item| item["uri"] == first_uri).unwrap();
    assert_eq!(fixed["kind"], "full");
    assert_eq!(fixed["items"], json!([]));
    let unchanged = items.iter().find(|item| item["uri"] == second_uri).unwrap();
    assert_eq!(unchanged["kind"], "unchanged");
}

#[tokio::test]
#[ignore = "manual real-project responsiveness benchmark"]
async fn benchmarks_real_workspace_responsiveness() {
    let root = PathBuf::from(
        std::env::var("DJANGO_LSP_BENCHMARK_ROOT")
            .expect("set DJANGO_LSP_BENCHMARK_ROOT to a Django project"),
    );
    let document = std::env::var("DJANGO_LSP_BENCHMARK_DOCUMENT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("manage.py"));
    let source = fs::read_to_string(&document).unwrap();
    let root_uri = file_uri(&root);
    let document_uri = file_uri(&document);
    let state = Arc::new(ServerState::default());
    let service_state = state.clone();
    let (mut service, mut socket) =
        LspService::new(move |client| Backend::new(client, service_state));
    tokio::spawn(async move { while socket.next().await.is_some() {} });

    let started = Instant::now();
    let initialize = Request::build("initialize")
        .params(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {},
        }))
        .id(1)
        .finish();
    assert!(send(&mut service, initialize).await.unwrap().is_ok());
    let initialize_elapsed = started.elapsed();
    assert!(
        send(
            &mut service,
            Request::build("initialized").params(json!({})).finish()
        )
        .await
        .is_none()
    );

    let started = Instant::now();
    let did_open = Request::build("textDocument/didOpen")
        .params(json!({
            "textDocument": {
                "uri": document_uri,
                "languageId": "python",
                "version": 1,
                "text": source,
            }
        }))
        .finish();
    assert!(send(&mut service, did_open).await.is_none());
    let completion = Request::build("textDocument/completion")
        .params(json!({
            "textDocument": {"uri": document_uri},
            "position": {"line": 0, "character": 0},
        }))
        .id(2)
        .finish();
    assert!(send(&mut service, completion).await.unwrap().is_ok());
    let open_barrier_elapsed = started.elapsed();

    let started = Instant::now();
    let did_change = Request::build("textDocument/didChange")
        .params(json!({
            "textDocument": {"uri": document_uri, "version": 2},
            "contentChanges": [{"text": format!("{source}\n")}],
        }))
        .finish();
    assert!(send(&mut service, did_change).await.is_none());
    let completion = Request::build("textDocument/completion")
        .params(json!({
            "textDocument": {"uri": document_uri},
            "position": {"line": 0, "character": 0},
        }))
        .id(3)
        .finish();
    assert!(send(&mut service, completion).await.unwrap().is_ok());
    let change_barrier_elapsed = started.elapsed();

    eprintln!(
        "initialize_ms={:.2} open_barrier_ms={:.2} change_barrier_ms={:.2}",
        initialize_elapsed.as_secs_f64() * 1_000.0,
        open_barrier_elapsed.as_secs_f64() * 1_000.0,
        change_barrier_elapsed.as_secs_f64() * 1_000.0,
    );
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

    let initialized = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});
    stdin.write_all(&frame(&initialized)).unwrap();
    stdin.flush().unwrap();
    let log_message = read_frame(&mut stdout);
    assert_eq!(log_message["method"], "window/logMessage");

    let views_path = fixture_root().join("blog/views.py");
    let views_uri = file_uri(&views_path);
    let diagnostic_source = concat!(
        "from .models import Blog\n",
        "for blog in Blog.objects.all():\n",
        "    print(blog.author.email)\n",
    );
    let did_open = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": views_uri,
                "languageId": "python",
                "version": 1,
                "text": diagnostic_source,
            }
        },
    });
    stdin.write_all(&frame(&did_open)).unwrap();
    stdin.flush().unwrap();

    let diagnostics = read_frame(&mut stdout);
    assert_eq!(diagnostics["method"], "textDocument/publishDiagnostics");
    assert_eq!(diagnostics["params"]["version"], 1);
    assert_eq!(diagnostics["params"]["diagnostics"][0]["code"], "DJ001");
    assert_eq!(
        diagnostics["params"]["diagnostics"][0]["range"],
        json!({
            "start": {"line": 2, "character": 15},
            "end": {"line": 2, "character": 21},
        })
    );

    let fixed_source = diagnostic_source.replace(
        "Blog.objects.all()",
        "Blog.objects.select_related(\"author\")",
    );
    let did_change = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": {"uri": views_uri, "version": 2},
            "contentChanges": [{"text": fixed_source}],
        },
    });
    stdin.write_all(&frame(&did_change)).unwrap();
    stdin.flush().unwrap();
    let diagnostics = read_frame(&mut stdout);
    assert_eq!(diagnostics["method"], "textDocument/publishDiagnostics");
    assert_eq!(diagnostics["params"]["version"], 2);
    assert_eq!(diagnostics["params"]["diagnostics"], json!([]));

    let did_close = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didClose",
        "params": {"textDocument": {"uri": views_uri}},
    });
    stdin.write_all(&frame(&did_close)).unwrap();
    stdin.flush().unwrap();
    let diagnostics = read_frame(&mut stdout);
    assert_eq!(diagnostics["method"], "textDocument/publishDiagnostics");
    assert_eq!(diagnostics["params"]["diagnostics"], json!([]));

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
