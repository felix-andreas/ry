//! Where a GNU R installation lives when neither `R_HOME` nor `R RHOME` names one.
//!
//! ry's runtime reads base R from GNU R (`system::r_home`) and `ry R` embeds GNU R
//! (`repl::libr`), and both search the same places, so the list lives here once; `repl`
//! includes this file by path because it does not link the interpreter. The order is the
//! platform's usual installers first: the distribution package on Linux, CRAN's framework and
//! then Homebrew on macOS, and on Windows the installer's registry key and then its default
//! directories.

use std::path::PathBuf;

/// The installations to try, in order. An entry is a candidate only: the caller checks that it
/// holds what it needs.
pub fn candidate_r_homes() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    let fixed: &[&str] = &[
        // CRAN's installer. `R.home()` reports this path, and the framework's versioned
        // directories sit behind it.
        "/Library/Frameworks/R.framework/Resources",
        // Homebrew on Apple silicon and on Intel. The `opt` link follows upgrades.
        "/opt/homebrew/opt/r/lib/R",
        "/usr/local/opt/r/lib/R",
        "/opt/R/lib/R",
    ];
    #[cfg(all(unix, not(target_os = "macos")))]
    let fixed: &[&str] = &[
        "/usr/lib/R",
        "/usr/local/lib/R",
        "/opt/R/lib/R",
        "/usr/lib64/R",
    ];
    #[cfg(windows)]
    let fixed: &[&str] = &[];
    #[cfg(not(any(unix, windows)))]
    let fixed: &[&str] = &[];
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut candidates: Vec<PathBuf> = fixed.iter().map(PathBuf::from).collect();
    #[cfg(windows)]
    {
        candidates.extend(windows::registry_install_paths());
        candidates.extend(windows::default_install_paths());
    }
    candidates
}

#[cfg(windows)]
mod windows {
    use std::{path::PathBuf, process::Command};

    /// The R installer records `InstallPath` under `R-core\R` (and `R-core\R64` for older
    /// 64-bit installs), machine-wide or for the current user. `reg query` reads it without
    /// a registry binding, and its output line is `InstallPath    REG_SZ    C:\...`.
    pub fn registry_install_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for root in ["HKLM", "HKCU"] {
            for key in ["R", "R64"] {
                let Ok(output) = Command::new("reg")
                    .args([
                        "query",
                        &format!(r"{root}\SOFTWARE\R-core\{key}"),
                        "/v",
                        "InstallPath",
                    ])
                    .output()
                else {
                    continue;
                };
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines() {
                    if let Some((_, value)) = line.split_once("REG_SZ") {
                        let value = value.trim();
                        if !value.is_empty() {
                            paths.push(PathBuf::from(value));
                        }
                    }
                }
            }
        }
        paths
    }

    /// `R-x.y.z` directories under the machine-wide and per-user install roots, newest first.
    pub fn default_install_paths() -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for variable in ["ProgramFiles", "ProgramW6432"] {
            if let Some(root) = std::env::var_os(variable) {
                roots.push(PathBuf::from(root).join("R"));
            }
        }
        if let Some(root) = std::env::var_os("LOCALAPPDATA") {
            roots.push(PathBuf::from(root).join("Programs").join("R"));
        }
        let mut found: Vec<(Vec<u32>, PathBuf)> = Vec::new();
        for root in roots {
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(version) = name.strip_prefix("R-") {
                    let parts = version
                        .split('.')
                        .filter_map(|part| part.parse().ok())
                        .collect();
                    found.push((parts, entry.path()));
                }
            }
        }
        found.sort_by(|left, right| right.0.cmp(&left.0));
        found.dedup_by(|left, right| left.1 == right.1);
        found.into_iter().map(|(_, path)| path).collect()
    }
}
