use {
    std::fs,
    zed::{
        Architecture, Command, DownloadedFileType, GithubReleaseOptions, LanguageServerId,
        LanguageServerInstallationStatus, Os, Worktree, serde_json,
    },
    zed_extension_api::{self as zed, Result, settings::LspSettings},
};

const NAME: &str = "ry";

struct Extension {
    cached_binary_path: Option<String>,
}

#[derive(Clone)]
struct Binary {
    path: String,
    args: Option<Vec<String>>,
}

impl Extension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_binary(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Binary> {
        let mut args = None;

        // 1. try get binary path or args from settings
        if let Some(settings) = LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|lsp_settings| lsp_settings.binary)
        {
            args = settings.arguments;
            if let Some(path) = settings.path {
                return Ok(Binary { path, args });
            }
        }

        // 2. try get from path
        if let Some(path) = worktree.which(NAME) {
            return Ok(Binary { path, args });
        }

        // 3. try get from github
        if let Some(path) = &self.cached_binary_path
            && fs::metadata(path).is_ok_and(|stat| stat.is_file())
        {
            return Ok(Binary {
                path: path.clone(),
                args,
            });
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let release = zed::latest_github_release(
            "felix-andreas/ry",
            GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let (platform, arch) = zed::current_platform();
        let asset_name = format!(
            "{NAME}-{arch}-{os}.{ext}",
            arch = match arch {
                Architecture::Aarch64 => "aarch64",
                Architecture::X86 => "x86",
                Architecture::X8664 => "x86_64",
            },
            os = match platform {
                Os::Mac => "apple-darwin",
                Os::Linux => "unknown-linux-musl",
                Os::Windows => "pc-windows-gnu",
            },
            ext = match platform {
                Os::Mac | Os::Linux => "tar.gz",
                Os::Windows => "zip",
            },
        );

        let asset = release
            .assets
            .iter()
            .find(|a| a.name == asset_name)
            .ok_or_else(|| format!("no asset found matching {:?}", asset_name))?;

        let version_dir = format!("{NAME}-{}", release.version);
        let path = match platform {
            Os::Mac | Os::Linux => format!("{version_dir}/{NAME}"),
            Os::Windows => format!("{version_dir}/{NAME}.exe"),
        };

        if !fs::metadata(&path).is_ok_and(|stat| stat.is_file()) {
            zed::set_language_server_installation_status(
                language_server_id,
                &LanguageServerInstallationStatus::Downloading,
            );

            zed::download_file(
                &asset.download_url,
                &version_dir,
                match platform {
                    Os::Mac | Os::Linux => DownloadedFileType::GzipTar,
                    Os::Windows => DownloadedFileType::Zip,
                },
            )
            .map_err(|err| format!("failed to download file: {err}"))?;

            // remove old versions
            let entries = fs::read_dir(".")
                .map_err(|err| format!("failed to list working directory {err}"))?;
            for entry in entries {
                let entry = entry.map_err(|err| format!("failed to load directory entry {err}"))?;
                if entry.file_name().to_str() != Some(&version_dir) {
                    fs::remove_dir_all(entry.path()).ok();
                }
            }
        }
        self.cached_binary_path = Some(path.clone());

        Ok(Binary { path, args })
    }
}

impl zed::Extension for Extension {
    fn new() -> Extension {
        Extension::new()
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let binary = self.language_server_binary(language_server_id, worktree)?;
        Ok(Command {
            command: binary.path,
            args: binary
                .args
                .unwrap_or_else(|| vec!["server".to_string(), "--stdio".to_string()]),
            env: Default::default(),
        })
    }

    fn language_server_workspace_configuration(
        &mut self,
        server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<serde_json::Value>> {
        let settings = LspSettings::for_worktree(server_id.as_ref(), worktree)
            .ok()
            .and_then(|lsp_settings| lsp_settings.settings.clone());

        Ok(settings)
    }
}

zed::register_extension!(Extension);
