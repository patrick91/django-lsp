use std::path::{Path, PathBuf};
use std::{ffi::OsString, process::ExitCode, sync::Arc};

use django_lsp::analysis::AnalysisDatabase;
use django_lsp::config::DjangoLspConfig;
use django_lsp::server::{Backend, ServerState};
use serde::Serialize;
use tokio::io::{stdin, stdout};
use tower_lsp_server::{LspService, Server};
use tracing_subscriber::EnvFilter;

const HELP: &str = "django-lsp - Django ORM language server and query checker

Usage:
  django-lsp
  django-lsp check [OPTIONS] [PATH ...]
  django-lsp [OPTIONS]

Commands:
  check [PATH ...]  Check Python files for repeated ORM relation queries

Check options:
  --format <FORMAT>  Output format: text, json, or github [default: text]

Options:
  -h, --help     Print help
  -V, --version  Print version

With no options, django-lsp communicates with an editor over standard input and output.";

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Serve,
    Check(CheckOptions),
    Help,
    Version,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CheckOptions {
    paths: Vec<PathBuf>,
    format: OutputFormat,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum OutputFormat {
    #[default]
    Text,
    Json,
    Github,
}

impl OutputFormat {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            "github" => Ok(Self::Github),
            _ => Err(format!(
                "invalid format `{value}`; expected text, json, or github"
            )),
        }
    }
}

#[derive(Debug, Serialize)]
struct CheckDiagnostic {
    path: String,
    line: usize,
    column: usize,
    end_line: usize,
    end_column: usize,
    severity: &'static str,
    code: &'static str,
    message: String,
    suggestion: CheckSuggestion,
}

#[derive(Debug, Serialize)]
struct CheckSuggestion {
    method: &'static str,
    relation: String,
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let args = args.into_iter().collect::<Vec<_>>();

    match args.as_slice() {
        [] => Ok(Action::Serve),
        [arg] if arg == "-h" || arg == "--help" => Ok(Action::Help),
        [arg] if arg == "-V" || arg == "--version" => Ok(Action::Version),
        [command, check_args @ ..] if command == "check" => parse_check_args(check_args),
        [arg] => Err(format!("unexpected argument: {}", arg.to_string_lossy())),
        _ => Err("unexpected arguments".to_string()),
    }
}

fn parse_check_args(args: &[OsString]) -> Result<Action, String> {
    let mut options = CheckOptions::default();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        let value = argument.to_string_lossy();
        match value.as_ref() {
            "-h" | "--help" => return Ok(Action::Help),
            "--" => {
                options
                    .paths
                    .extend(args[index + 1..].iter().map(PathBuf::from));
                break;
            }
            "--format" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "`--format` requires a value".to_string())?;
                options.format = OutputFormat::parse(&value.to_string_lossy())?;
            }
            value if value.starts_with("--format=") => {
                options.format = OutputFormat::parse(&value["--format=".len()..])?;
            }
            value if value.starts_with('-') => {
                return Err(format!("unexpected check option: {value}"));
            }
            _ => options.paths.push(PathBuf::from(argument)),
        }
        index += 1;
    }
    Ok(Action::Check(options))
}

#[tokio::main]
async fn main() -> ExitCode {
    match parse_args(std::env::args_os().skip(1)) {
        Ok(Action::Help) => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Ok(Action::Version) => {
            println!("django-lsp {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(Action::Check(options)) => return run_check(&options),
        Ok(Action::Serve) => {}
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("Try 'django-lsp --help' for more information.");
            return ExitCode::from(2);
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let (service, socket) =
        LspService::new(|client| Backend::new(client, Arc::new(ServerState::default())));
    Server::new(stdin(), stdout(), socket).serve(service).await;

    ExitCode::SUCCESS
}

fn run_check(options: &CheckOptions) -> ExitCode {
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(error) => {
            eprintln!("error: failed to determine the current directory: {error}");
            return ExitCode::from(2);
        }
    };
    let config = match DjangoLspConfig::load(&workspace) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };
    let database = match AnalysisDatabase::build(&workspace, config) {
        Ok(database) => database,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };

    let requested_paths = match normalize_requested_paths(&workspace, &options.paths) {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };
    let mut findings = Vec::new();
    for path in database.paths() {
        if !requested_paths.is_empty()
            && !requested_paths
                .iter()
                .any(|requested| path == *requested || path.starts_with(requested))
        {
            continue;
        }
        let Some(source) = database.source_for_path(&path) else {
            continue;
        };
        let Some(diagnostics) = database.diagnostics_for_path(&path) else {
            continue;
        };
        for diagnostic in diagnostics {
            let (line, column) = line_column(source, diagnostic.range.start().to_usize());
            let (end_line, end_column) = line_column(source, diagnostic.range.end().to_usize());
            let display_path = path.strip_prefix(&workspace).unwrap_or(&path);
            findings.push(CheckDiagnostic {
                path: display_path.to_string_lossy().replace('\\', "/"),
                line,
                column,
                end_line,
                end_column,
                severity: "warning",
                code: diagnostic.code,
                message: diagnostic.message.clone(),
                suggestion: CheckSuggestion {
                    method: diagnostic.method,
                    relation: diagnostic.relation_path.clone(),
                },
            });
        }
    }

    if let Err(error) = print_findings(&findings, options.format) {
        eprintln!("error: failed to format diagnostics: {error}");
        return ExitCode::from(2);
    }

    if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn print_findings(findings: &[CheckDiagnostic], format: OutputFormat) -> serde_json::Result<()> {
    match format {
        OutputFormat::Text => {
            for finding in findings {
                println!(
                    "{}:{}:{}: warning {}: {}",
                    finding.path, finding.line, finding.column, finding.code, finding.message
                );
            }
        }
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(findings)?),
        OutputFormat::Github => {
            for finding in findings {
                println!(
                    "::warning file={},line={},col={},endLine={},endColumn={},title={}::{}",
                    escape_github_property(&finding.path),
                    finding.line,
                    finding.column,
                    finding.end_line,
                    finding.end_column,
                    escape_github_property(finding.code),
                    escape_github_message(&finding.message),
                );
            }
        }
    }
    Ok(())
}

fn escape_github_message(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn escape_github_property(value: &str) -> String {
    escape_github_message(value)
        .replace(':', "%3A")
        .replace(',', "%2C")
}

fn normalize_requested_paths(workspace: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    paths
        .iter()
        .map(|path| {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                workspace.join(path)
            };
            path.canonicalize()
                .map_err(|error| format!("failed to resolve `{}`: {error}", path.display()))
        })
        .collect()
}

fn line_column(source: &str, offset: usize) -> (usize, usize) {
    let prefix = source.get(..offset).unwrap_or(source);
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, line)| line)
        .chars()
        .count()
        + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Action, CheckOptions, OutputFormat, line_column, parse_args};

    #[test]
    fn no_arguments_starts_the_server() {
        assert_eq!(parse_args([]).unwrap(), Action::Serve);
    }

    #[test]
    fn parses_help_and_version_options() {
        for option in ["-h", "--help"] {
            assert_eq!(parse_args([option.into()]).unwrap(), Action::Help);
            assert_eq!(
                parse_args(["check".into(), option.into()]).unwrap(),
                Action::Help
            );
        }

        for option in ["-V", "--version"] {
            assert_eq!(parse_args([option.into()]).unwrap(), Action::Version);
        }
    }

    #[test]
    fn parses_check_paths() {
        assert_eq!(
            parse_args(["check".into(), "blog".into(), "users/views.py".into()]).unwrap(),
            Action::Check(CheckOptions {
                paths: vec![PathBuf::from("blog"), PathBuf::from("users/views.py")],
                format: OutputFormat::Text,
            })
        );
    }

    #[test]
    fn parses_check_output_formats() {
        assert_eq!(
            parse_args([
                "check".into(),
                "--format".into(),
                "json".into(),
                "blog".into(),
            ])
            .unwrap(),
            Action::Check(CheckOptions {
                paths: vec![PathBuf::from("blog")],
                format: OutputFormat::Json,
            })
        );
        assert_eq!(
            parse_args(["check".into(), "--format=github".into()]).unwrap(),
            Action::Check(CheckOptions {
                paths: Vec::new(),
                format: OutputFormat::Github,
            })
        );
        assert!(parse_args(["check".into(), "--format".into()]).is_err());
        assert!(parse_args(["check".into(), "--format=sarif".into()]).is_err());
    }

    #[test]
    fn rejects_arguments_that_would_corrupt_the_lsp_transport() {
        assert!(parse_args(["--stdio".into()]).is_err());
        assert!(parse_args(["--help".into(), "extra".into()]).is_err());
    }

    #[test]
    fn reports_one_based_locations() {
        assert_eq!(line_column("one\ntwo", 5), (2, 2));
    }
}
