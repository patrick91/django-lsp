use std::{ffi::OsString, process::ExitCode, sync::Arc};

use django_lsp::server::{Backend, ServerState};
use tokio::io::{stdin, stdout};
use tower_lsp_server::{LspService, Server};
use tracing_subscriber::EnvFilter;

const HELP: &str = "django-lsp - Django ORM completion language server

Usage: django-lsp [OPTIONS]

Options:
  -h, --help     Print help
  -V, --version  Print version

With no options, django-lsp communicates with an editor over standard input and output.";

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Serve,
    Help,
    Version,
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let args = args.into_iter().collect::<Vec<_>>();

    match args.as_slice() {
        [] => Ok(Action::Serve),
        [arg] if arg == "-h" || arg == "--help" => Ok(Action::Help),
        [arg] if arg == "-V" || arg == "--version" => Ok(Action::Version),
        [arg] => Err(format!("unexpected argument: {}", arg.to_string_lossy())),
        _ => Err("django-lsp accepts at most one option".to_string()),
    }
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

#[cfg(test)]
mod tests {
    use super::{Action, parse_args};

    #[test]
    fn no_arguments_starts_the_server() {
        assert_eq!(parse_args([]).unwrap(), Action::Serve);
    }

    #[test]
    fn parses_help_and_version_options() {
        for option in ["-h", "--help"] {
            assert_eq!(parse_args([option.into()]).unwrap(), Action::Help);
        }

        for option in ["-V", "--version"] {
            assert_eq!(parse_args([option.into()]).unwrap(), Action::Version);
        }
    }

    #[test]
    fn rejects_arguments_that_would_corrupt_the_lsp_transport() {
        assert!(parse_args(["--stdio".into()]).is_err());
        assert!(parse_args(["--help".into(), "extra".into()]).is_err());
    }
}
