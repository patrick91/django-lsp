use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use django_lsp::analysis::AnalysisDatabase;
use django_lsp::config::DjangoLspConfig;
use django_lsp::server::{Backend, ServerState};
use serde::Serialize;
use serde_json::{Value, json};
use tower::{Service, ServiceExt};
use tower_lsp_server::LspService;
use tower_lsp_server::jsonrpc::{Request, Response};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const CURSOR_MARKER: &str = "<cursor>";
const MARKDOWN_EXAMPLE_PREFIX: &str = "<!-- django-lsp-example";
const MARKDOWN_EXAMPLE_END: &str = "-->";
const MARKDOWN_OUTPUT_START: &str = "<!-- django-lsp-output:start -->";
const MARKDOWN_OUTPUT_END: &str = "<!-- django-lsp-output:end -->";
const DIAGNOSTIC_EXAMPLE_PREFIX: &str = "<!-- django-lsp-diagnostic";
const DIAGNOSTIC_EXAMPLE_END: &str = "<!-- django-lsp-diagnostic:end -->";
const MDX_EXAMPLE_PREFIX: &str = "{/* django-lsp-example";
const MDX_EXAMPLE_END: &str = "*/}";
const MDX_OUTPUT_START: &str = "{/* django-lsp-output:start */}";
const MDX_OUTPUT_END: &str = "{/* django-lsp-output:end */}";
const DOCS_ROOT: &str = "website/content/docs";
const GENERATED_EXAMPLES: &str = "website/frontend/generated/completions.json";

#[derive(Debug)]
struct ExampleOptions {
    id: String,
    file: PathBuf,
    limit: usize,
}

#[derive(Debug)]
struct DiagnosticExampleOptions {
    file: PathBuf,
    code: String,
    method: String,
    relation_path: String,
}

#[derive(Debug)]
struct RenderedDocument {
    path: PathBuf,
    contents: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeneratedExample {
    id: String,
    fixture: String,
    source: String,
    cursor: GeneratedCursor,
    items: Vec<String>,
    visible_items: usize,
    models_fixture: String,
    models_source: String,
}

#[derive(Debug, Serialize)]
struct GeneratedCursor {
    line: usize,
    character: usize,
}

#[derive(Debug)]
struct RenderedExample {
    menu: String,
    generated: GeneratedExample,
}

#[derive(Clone, Copy, Debug)]
struct ExampleSyntax {
    example_end: &'static str,
    output_start: &'static str,
    output_end: &'static str,
    supports_components: bool,
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
    let (mut documents, examples) = render_documents(root).await?;
    documents.push(RenderedDocument {
        path: root.join(GENERATED_EXAMPLES),
        contents: format!("{}\n", serde_json::to_string_pretty(&examples)?),
    });
    let mut stale = Vec::new();

    for document in documents {
        if check {
            if fs::read_to_string(&document.path).ok().as_deref() != Some(&document.contents) {
                stale.push(document.path);
            }
        } else {
            if let Some(parent) = document.path.parent() {
                fs::create_dir_all(parent)?;
            }
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

async fn render_documents(root: &Path) -> Result<(Vec<RenderedDocument>, Vec<GeneratedExample>)> {
    let docs_root = root.join(DOCS_ROOT);
    let paths = markdown_paths(&docs_root)?;

    let mut documents = Vec::new();
    let mut examples = Vec::new();
    let mut example_ids = HashSet::new();
    let mut diagnostic_examples = 0usize;
    for path in paths {
        let source = fs::read_to_string(&path)?;
        diagnostic_examples += validate_diagnostic_examples(root, &path, &source)?;
        let (contents, document_examples) = render_markdown(root, &path, &source).await?;
        if !document_examples.is_empty() {
            for example in &document_examples {
                if !example_ids.insert(example.id.clone()) {
                    return Err(message(format!(
                        "duplicate django-lsp example id `{}`",
                        example.id
                    )));
                }
            }
            examples.extend(document_examples);
            documents.push(RenderedDocument { path, contents });
        }
    }

    if documents.is_empty() {
        return Err(message(format!(
            "no executable Markdown examples found in {DOCS_ROOT}"
        )));
    }
    if diagnostic_examples == 0 {
        return Err(message(format!(
            "no executable diagnostic examples found in {DOCS_ROOT}"
        )));
    }

    Ok((documents, examples))
}

fn validate_diagnostic_examples(root: &Path, path: &Path, markdown: &str) -> Result<usize> {
    let lines = markdown.lines().collect::<Vec<_>>();
    let mut index = 0usize;
    let mut count = 0usize;
    while index < lines.len() {
        let Some(options) = lines[index].strip_prefix(DIAGNOSTIC_EXAMPLE_PREFIX) else {
            index += 1;
            continue;
        };
        let options = parse_diagnostic_options(options, path, index + 1)?;
        index += 1;
        if !lines
            .get(index)
            .is_some_and(|line| line.trim_start().starts_with("```python"))
        {
            return Err(message(format!(
                "{}:{}: diagnostic example must be followed by a Python code fence",
                path.display(),
                index + 1
            )));
        }
        index += 1;
        let source_start = index;
        while index < lines.len() && !is_closing_fence(lines[index], '`', 3) {
            index += 1;
        }
        if index == lines.len() {
            return Err(message(format!(
                "{}:{}: unclosed diagnostic Python fence",
                path.display(),
                source_start
            )));
        }
        let source = lines[source_start..index].join("\n");
        index += 1;
        if lines.get(index) != Some(&DIAGNOSTIC_EXAMPLE_END) {
            return Err(message(format!(
                "{}:{}: diagnostic example must end with `{DIAGNOSTIC_EXAMPLE_END}`",
                path.display(),
                index + 1
            )));
        }
        validate_diagnostic_example(root, &options, &source).map_err(|error| {
            message(format!("{}:{}: {error}", path.display(), source_start + 1))
        })?;
        count += 1;
        index += 1;
    }
    Ok(count)
}

fn parse_diagnostic_options(
    text: &str,
    path: &Path,
    line: usize,
) -> Result<DiagnosticExampleOptions> {
    let mut file = None;
    let mut code = None;
    let mut method = None;
    let mut relation_path = None;
    for option in text.trim_end_matches("-->").split_whitespace() {
        let (key, value) = option.split_once('=').ok_or_else(|| {
            message(format!(
                "{}:{line}: expected diagnostic option in key=value form",
                path.display()
            ))
        })?;
        match key {
            "file" => file = Some(PathBuf::from(value)),
            "code" => code = Some(value.to_string()),
            "method" => method = Some(value.to_string()),
            "path" => relation_path = Some(value.to_string()),
            _ => {
                return Err(message(format!(
                    "{}:{line}: unknown diagnostic option `{key}`",
                    path.display()
                )));
            }
        }
    }
    let required = |value: Option<String>, name: &str| {
        value.ok_or_else(|| {
            message(format!(
                "{}:{line}: diagnostic examples require a {name} option",
                path.display()
            ))
        })
    };
    Ok(DiagnosticExampleOptions {
        file: file.ok_or_else(|| {
            message(format!(
                "{}:{line}: diagnostic examples require a file option",
                path.display()
            ))
        })?,
        code: required(code, "code")?,
        method: required(method, "method")?,
        relation_path: required(relation_path, "path")?,
    })
}

fn validate_diagnostic_example(
    root: &Path,
    options: &DiagnosticExampleOptions,
    source: &str,
) -> Result<()> {
    let fixture_root = root.join("tests/fixtures/django_project").canonicalize()?;
    let document_path = fixture_root.join(&options.file).canonicalize()?;
    if !document_path.starts_with(&fixture_root) {
        return Err(message(
            "diagnostic example file must be inside the Django fixture",
        ));
    }
    let mut database = AnalysisDatabase::build(&fixture_root, DjangoLspConfig::default())?;
    database.sync_path(document_path.clone(), Some(source.to_string()))?;
    let diagnostics = database
        .diagnostics_for_path(&document_path)
        .ok_or_else(|| message("diagnostic example file was not analyzed"))?;
    if diagnostics.iter().any(|diagnostic| {
        diagnostic.code == options.code
            && diagnostic.method == options.method
            && diagnostic.relation_path == options.relation_path
    }) {
        Ok(())
    } else {
        Err(message(format!(
            "expected {} {}(\"{}\"), got {diagnostics:#?}",
            options.code, options.method, options.relation_path
        )))
    }
}

fn markdown_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let mut directories = vec![root.to_path_buf()];
    let mut paths = Vec::new();

    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                directories.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "md" || extension == "mdx")
            {
                paths.push(path);
            }
        }
    }

    paths.sort();
    Ok(paths)
}

async fn render_markdown(
    root: &Path,
    path: &Path,
    markdown: &str,
) -> Result<(String, Vec<GeneratedExample>)> {
    let lines = markdown.lines().collect::<Vec<_>>();
    let mut rendered = String::new();
    let mut line_index = 0;
    let mut markdown_fence = None;
    let mut examples = Vec::new();

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

        let Some((option_text, syntax)) = example_start(line) else {
            rendered.push_str(line);
            rendered.push('\n');
            line_index += 1;
            continue;
        };

        let options = parse_options(option_text, path, line_index + 1)?;
        rendered.push_str(line);
        rendered.push('\n');
        line_index += 1;
        let source_start = line_index;
        while line_index < lines.len() && lines[line_index] != syntax.example_end {
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
        rendered.push_str(syntax.example_end);
        rendered.push('\n');
        line_index += 1;
        if lines.get(line_index) != Some(&syntax.output_start) {
            return Err(message(format!(
                "{}:{}: django-lsp example must be followed by `{}`",
                path.display(),
                line_index + 1,
                syntax.output_start
            )));
        }
        rendered.push_str(syntax.output_start);
        rendered.push('\n');
        line_index += 1;
        while line_index < lines.len() && lines[line_index] != syntax.output_end {
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
        if syntax.supports_components {
            rendered.push_str(&format!(
                concat!(
                    "<div class=\"completion-example\">\n",
                    "<AutocompleteDemo example=\"{}\" compact></AutocompleteDemo>\n",
                    "</div>\n"
                ),
                example.generated.id
            ));
        } else {
            rendered.push_str("```text\n");
            rendered.push_str(&example.menu);
            rendered.push_str("\n```\n");
        }
        rendered.push_str(syntax.output_end);
        rendered.push('\n');
        examples.push(example.generated);
        line_index += 1;
    }

    Ok((rendered, examples))
}

fn example_start(line: &str) -> Option<(&str, ExampleSyntax)> {
    if let Some(options) = line.strip_prefix(MDX_EXAMPLE_PREFIX) {
        return Some((
            options,
            ExampleSyntax {
                example_end: MDX_EXAMPLE_END,
                output_start: MDX_OUTPUT_START,
                output_end: MDX_OUTPUT_END,
                supports_components: true,
            },
        ));
    }

    line.strip_prefix(MARKDOWN_EXAMPLE_PREFIX).map(|options| {
        (
            options,
            ExampleSyntax {
                example_end: MARKDOWN_EXAMPLE_END,
                output_start: MARKDOWN_OUTPUT_START,
                output_end: MARKDOWN_OUTPUT_END,
                supports_components: true,
            },
        )
    })
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
    let mut id = None;
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
            "id" => {
                if value.is_empty()
                    || !value.chars().all(|character| {
                        character.is_ascii_lowercase()
                            || character.is_ascii_digit()
                            || character == '-'
                    })
                {
                    return Err(message(format!(
                        "{}:{line}: id must contain lowercase ASCII letters, numbers, or hyphens",
                        path.display()
                    )));
                }
                id = Some(value.to_owned());
            }
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
        id: id.ok_or_else(|| {
            message(format!(
                "{}:{line}: django-lsp examples require an id option",
                path.display()
            ))
        })?,
        file: file.ok_or_else(|| {
            message(format!(
                "{}:{line}: django-lsp examples require a file option",
                path.display()
            ))
        })?,
        limit,
    })
}

async fn render_example(
    root: &Path,
    options: &ExampleOptions,
    source: &str,
) -> Result<RenderedExample> {
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

    let menu = render_completion_menu(&source, cursor, &labels, options.limit);
    let visible_items = labels.len().min(options.limit);

    let models_fixture = options
        .file
        .parent()
        .map(|directory| directory.join("models.py"))
        .unwrap_or_else(|| PathBuf::from("models.py"));
    let models_source = fs::read_to_string(fixture_root.join(&models_fixture))
        .map_err(|error| {
            message(format!(
                "failed to read {}: {error}",
                models_fixture.display()
            ))
        })?
        .trim_end()
        .to_string();

    Ok(RenderedExample {
        menu,
        generated: GeneratedExample {
            id: options.id.clone(),
            fixture: options.file.display().to_string(),
            source: source.clone(),
            cursor: cursor_position(&source, cursor),
            items: labels,
            visible_items,
            models_fixture: models_fixture.display().to_string(),
            models_source,
        },
    })
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
    let cursor = cursor_position(source, offset);
    json!({"line": cursor.line, "character": cursor.character})
}

fn cursor_position(source: &str, offset: usize) -> GeneratedCursor {
    let prefix = &source[..offset];
    GeneratedCursor {
        line: prefix.bytes().filter(|byte| *byte == b'\n').count(),
        character: prefix
            .rsplit_once('\n')
            .map_or(prefix, |(_, line)| line)
            .encode_utf16()
            .count(),
    }
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
