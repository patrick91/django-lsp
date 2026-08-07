use std::fs;

use zed::settings::LspSettings;
use zed_extension_api::{
    self as zed, Architecture, DownloadedFileType, GithubReleaseOptions, LanguageServerId,
    LanguageServerInstallationStatus, Os, Result,
};

const REPOSITORY: &str = "patrick91/django-lsp";

struct DjangoLspExtension {
    cached_binary_path: Option<String>,
}

impl zed::Extension for DjangoLspExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binary_path = match &self.cached_binary_path {
            Some(path) => path.clone(),
            None => self.resolve_binary(language_server_id, worktree)?,
        };

        Ok(zed::Command {
            command: binary_path,
            args: Vec::new(),
            env: worktree.shell_env(),
        })
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        let settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|settings| settings.settings.clone())
            .unwrap_or_default();
        Ok(Some(settings))
    }
}

impl DjangoLspExtension {
    fn resolve_binary(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<String> {
        if let Some(path) = worktree.which("django-lsp") {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let release = zed::latest_github_release(
            REPOSITORY,
            GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;
        let (os, architecture) = zed::current_platform();
        let asset_name = binary_asset_name(os, architecture)?;
        let version_directory = format!("bin/{}", release.version);
        let binary_path = format!("{version_directory}/{asset_name}");

        if fs::metadata(&binary_path).is_ok() {
            zed::make_file_executable(&binary_path)?;
            self.cleanup_old_versions(&release.version);
            self.cached_binary_path = Some(binary_path.clone());
            return Ok(binary_path);
        }

        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| {
                format!(
                    "release {} does not contain {asset_name}; install django-lsp from PyPI or use an extension-ready release",
                    release.version
                )
            })?;

        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::Downloading,
        );
        fs::create_dir_all(&version_directory)
            .map_err(|error| format!("failed to create {version_directory}: {error}"))?;
        zed::download_file(
            &asset.download_url,
            &binary_path,
            DownloadedFileType::Uncompressed,
        )
        .map_err(|error| format!("failed to download {asset_name}: {error}"))?;
        zed::make_file_executable(&binary_path)?;
        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::None,
        );

        self.cleanup_old_versions(&release.version);
        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }

    fn cleanup_old_versions(&self, current_version: &str) {
        let Ok(entries) = fs::read_dir("bin") else {
            return;
        };

        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() || entry.file_name() == current_version {
                continue;
            }
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn binary_asset_name(os: Os, architecture: Architecture) -> Result<&'static str> {
    match (os, architecture) {
        (Os::Mac, Architecture::Aarch64) => Ok("django-lsp-aarch64-apple-darwin"),
        (Os::Mac, Architecture::X8664) => Ok("django-lsp-x86_64-apple-darwin"),
        (Os::Linux, Architecture::Aarch64) => Ok("django-lsp-aarch64-unknown-linux-gnu"),
        (Os::Linux, Architecture::X8664) => Ok("django-lsp-x86_64-unknown-linux-gnu"),
        (Os::Windows, Architecture::X8664) => Ok("django-lsp-x86_64-pc-windows-msvc.exe"),
        (os, architecture) => Err(format!(
            "django-lsp does not publish a binary for {os:?} {architecture:?}"
        )),
    }
}

zed::register_extension!(DjangoLspExtension);
