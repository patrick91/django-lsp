use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use django_lsp::server::{Backend, ServerState};
use serde_json::{Value, json};
use tower::{Service, ServiceExt};
use tower_lsp_server::LspService;
use tower_lsp_server::jsonrpc::{Request, Response};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const CURSOR_MARKER: &str = "<cursor>";
const EXAMPLE_PREFIX: &str = "<!-- django-lsp-example";
const EXAMPLE_END: &str = "-->";
const OUTPUT_START: &str = "<!-- django-lsp-output:start -->";
const OUTPUT_END: &str = "<!-- django-lsp-output:end -->";

#[derive(Debug)]
struct ExampleOptions {
    file: PathBuf,
    limit: usize,
}

#[derive(Debug)]
struct RenderedDocument {
    path: PathBuf,
    contents: String,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("render-docs: {error}");
        process::exit(1);
    }
}

async fn run() -> Result<()> {
    let check = match env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => false,
        [argument] if argument == "--check" => true,
        _ => return Err(message("usage: render-docs [--check]")),
    };

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let documents = render_documents(root).await?;
    let mut stale = Vec::new();

    for document in documents {
        let current = fs::read_to_string(&document.path)?;
        if check {
            if current != document.contents {
                stale.push(document.path);
            }
        } else {
            fs::write(&document.path, document.contents)?;
            println!("rendered {}", relative_path(root, &document.path));
        }
    }

    if stale.is_empty() {
        if check {
            println!("generated documentation is current");
        }
        return Ok(());
    }

    let paths = stale
        .iter()
        .map(|path| relative_path(root, path))
        .collect::<Vec<_>>()
        .join(", ");
    Err(message(format!(
        "generated documentation is stale: {paths}; run `cargo run --bin render-docs`"
    )))
}

async fn render_documents(root: &Path) -> Result<Vec<RenderedDocument>> {
    let docs_root = root.join("docs");
    let mut paths = fs::read_dir(&docs_root)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .collect::<Vec<_>>();
    paths.sort();

    let mut documents = Vec::new();
    for path in paths {
        let source = fs::read_to_string(&path)?;
        let (contents, example_count) = render_markdown(root, &path, &source).await?;
        if example_count > 0 {
            documents.push(RenderedDocument { path, contents });
        }
    }

    if documents.is_empty() {
        return Err(message("no executable Markdown examples found in docs"));
    }

    Ok(documents)
}

async fn render_markdown(root: &Path, path: &Path, markdown: &str) -> Result<(String, usize)> {
    let lines = markdown.lines().collect::<Vec<_>>();
    let mut rendered = String::new();
    let mut line_index = 0;
    let mut markdown_fence = None;
    let mut example_count = 0;

    while line_index < lines.len() {
        let line = lines[line_index];
        if let Some((marker, minimum_length)) = markdown_fence {
            rendered.push_str(line);
            rendered.push('\n');
            line_index += 1;
            if is_closing_fence(line, marker, minimum_length) {
                markdown_fence = None;
            }
            continue;
        }
        if let Some(fence) = opening_fence(line) {
            rendered.push_str(line);
            rendered.push('\n');
            line_index += 1;
            markdown_fence = Some(fence);
            continue;
        }

        let Some(option_text) = line.strip_prefix(EXAMPLE_PREFIX) else {
            rendered.push_str(line);
            rendered.push('\n');
            line_index += 1;
            continue;
        };

        let options = parse_options(option_text, path, line_index + 1)?;
        example_count += 1;
        rendered.push_str(line);
        rendered.push('\n');
        line_index += 1;
        let source_start = line_index;
        while line_index < lines.len() && lines[line_index] != EXAMPLE_END {
            rendered.push_str(lines[line_index]);
            rendered.push('\n');
            line_index += 1;
        }
        if line_index == lines.len() {
            return Err(message(format!(
                "{}:{}: unclosed django-lsp example",
                path.display(),
                source_start
            )));
        }

        let source = lines[source_start..line_index].join("\n");
        rendered.push_str(EXAMPLE_END);
        rendered.push('\n');
        line_index += 1;
        if lines.get(line_index) != Some(&OUTPUT_START) {
            return Err(message(format!(
                "{}:{}: django-lsp example must be followed by `{OUTPUT_START}`",
                path.display(),
                line_index + 1
            )));
        }
        rendered.push_str(OUTPUT_START);
        rendered.push('\n');
        line_index += 1;
        while line_index < lines.len() && lines[line_index] != OUTPUT_END {
            line_index += 1;
        }
        if line_index == lines.len() {
            return Err(message(format!(
                "{}:{}: unclosed django-lsp output",
                path.display(),
                source_start
            )));
        }

        let example = render_example(root, &options, &source)
            .await
            .map_err(|error| {
                message(format!("{}:{}: {error}", path.display(), source_start + 1))
            })?;
        rendered.push_str("```text\n");
        rendered.push_str(&example);
        rendered.push_str("\n```\n");
        rendered.push_str(OUTPUT_END);
        rendered.push('\n');
        line_index += 1;
    }

    Ok((rendered, example_count))
}

fn opening_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let length = trimmed
        .chars()
        .take_while(|character| *character == marker)
        .count();
    (length >= 3).then_some((marker, length))
}

fn is_closing_fence(line: &str, marker: char, minimum_length: usize) -> bool {
    let trimmed = line.trim_start();
    let length = trimmed
        .chars()
        .take_while(|character| *character == marker)
        .count();
    length >= minimum_length && trimmed[length..].trim().is_empty()
}

fn parse_options(text: &str, path: &Path, line: usize) -> Result<ExampleOptions> {
    let mut file = None;
    let mut limit = 8;

    for option in text.split_whitespace() {
        let (key, value) = option.split_once('=').ok_or_else(|| {
            message(format!(
                "{}:{line}: expected django-lsp option in key=value form",
                path.display()
            ))
        })?;
        match key {
            "file" => file = Some(PathBuf::from(value)),
            "limit" => {
                limit = value.parse::<usize>().map_err(|_| {
                    message(format!(
                        "{}:{line}: limit must be a positive integer",
                        path.display()
                    ))
                })?;
                if limit == 0 {
                    return Err(message(format!(
                        "{}:{line}: limit must be a positive integer",
                        path.display()
                    )));
                }
            }
            _ => {
                return Err(message(format!(
                    "{}:{line}: unknown django-lsp option `{key}`",
                    path.display()
                )));
            }
        }
    }

    Ok(ExampleOptions {
        file: file.ok_or_else(|| {
            message(format!(
                "{}:{line}: django-lsp examples require a file option",
                path.display()
            ))
        })?,
        limit,
    })
}

async fn render_example(root: &Path, options: &ExampleOptions, source: &str) -> Result<String> {
    let marker_count = source.matches(CURSOR_MARKER).count();
    if marker_count != 1 {
        return Err(message(format!(
            "expected exactly one {CURSOR_MARKER} marker, found {marker_count}"
        )));
    }

    let cursor = source.find(CURSOR_MARKER).unwrap();
    let source = source.replacen(CURSOR_MARKER, "", 1);
    let fixture_root = root.join("tests/fixtures/django_project").canonicalize()?;
    let document_path = fixture_root.join(&options.file).canonicalize()?;
    if !document_path.starts_with(&fixture_root) {
        return Err(message("example file must be inside the Django fixture"));
    }

    let labels = completion_labels(&fixture_root, &document_path, &source, cursor).await?;
    if labels.is_empty() {
        return Err(message("the language server returned no completions"));
    }

    Ok(render_completion_menu(
        &source,
        cursor,
        &labels,
        options.limit,
    ))
}

async fn completion_labels(
    fixture_root: &Path,
    document_path: &Path,
    source: &str,
    cursor: usize,
) -> Result<Vec<String>> {
    let state = Arc::new(ServerState::default());
    let service_state = state.clone();
    let (mut service, _socket) = LspService::new(move |client| Backend::new(client, service_state));
    let root_uri = file_uri(fixture_root)?;
    let document_uri = file_uri(document_path)?;

    let initialize = Request::build("initialize")
        .params(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {},
        }))
        .id(1)
        .finish();
    let response = send(&mut service, initialize)
        .await?
        .ok_or_else(|| message("initialize returned no response"))?;
    if !response.is_ok() {
        return Err(message(format!("initialize failed: {response:?}")));
    }

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
    send(&mut service, did_open).await?;

    let completion = Request::build("textDocument/completion")
        .params(json!({
            "textDocument": {"uri": document_uri},
            "position": position_at(source, cursor),
        }))
        .id(2)
        .finish();
    let response = send(&mut service, completion)
        .await?
        .ok_or_else(|| message("completion returned no response"))?;
    if !response.is_ok() {
        return Err(message(format!("completion failed: {response:?}")));
    }

    let items = response
        .result()
        .and_then(Value::as_array)
        .ok_or_else(|| message("completion response did not contain an item array"))?;
    items
        .iter()
        .map(|item| {
            item["label"]
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| message("completion item did not contain a label"))
        })
        .collect()
}

async fn send(service: &mut LspService<Backend>, request: Request) -> Result<Option<Response>> {
    Ok(service.ready().await?.call(request).await?)
}

fn position_at(source: &str, offset: usize) -> Value {
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let character = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, line)| line)
        .encode_utf16()
        .count();
    json!({"line": line, "character": character})
}

fn render_completion_menu(source: &str, cursor: usize, labels: &[String], limit: usize) -> String {
    let cursor_line = source[..cursor]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    let cursor_column = source[..cursor]
        .rsplit_once('\n')
        .map_or(&source[..cursor], |(_, line)| line)
        .chars()
        .count();
    let mut lines = source.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
    let visible = labels.len().min(limit);
    let mut menu = labels
        .iter()
        .take(visible)
        .enumerate()
        .map(|(index, label)| {
            let is_last = index + 1 == visible && visible == labels.len();
            let branch = if is_last { "└─" } else { "├─" };
            format!("{}{branch} {label}", " ".repeat(cursor_column))
        })
        .collect::<Vec<_>>();
    if labels.len() > visible {
        menu.push(format!(
            "{}└─ … {} more",
            " ".repeat(cursor_column),
            labels.len() - visible
        ));
    }
    lines.splice(cursor_line + 1..cursor_line + 1, menu);
    lines.join("\n")
}

fn file_uri(path: &Path) -> Result<String> {
    let path = path.canonicalize()?;
    Ok(format!("file://{}", path.display()))
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn message(message: impl Into<String>) -> Box<dyn Error + Send + Sync> {
    io::Error::other(message.into()).into()
}
