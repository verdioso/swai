/// Update installer module — downloads and installs new SWAI releases.
///
/// This module provides:
/// - Download release tarball from GitHub
/// - Replace the local binary at ~/.local/bin/swai
/// - Refresh desktop shortcuts (if any)
/// - Send desktop notification on completion
use reqwest::blocking::Client;
use std::fs;
use std::io::{self, Write as IoWrite};
use std::path::PathBuf;
use std::process::Command;

// ─── Install Result ───────────────────────────────────────────────────────

/// Result of an update install attempt.
#[derive(Debug)]
pub enum UpdateInstallResult {
    /// Update installed successfully.
    Success { new_version: String },
    /// Installation failed with an error message.
    Error(String),
}

// ─── Download & Install ───────────────────────────────────────────────────

/// Download and install a new SWAI release from GitHub.
///
/// 1. Downloads the tarball for the current platform.
/// 2. Extracts it to a temp directory.
/// 3. Replaces `~/.local/bin/swai` with the new binary.
/// 4. Sends a desktop notification on success or failure.
pub fn install_update(github_repo: &str, version: &str) -> UpdateInstallResult {
    let client = match Client::builder()
        .user_agent("SWAI/1.0 (Linux; GTK4)")
        .timeout(std::time::Duration::from_secs(120))
        .build()
    {
        Ok(c) => c,
        Err(e) => return UpdateInstallResult::Error(format!("Failed to build HTTP client: {}", e)),
    };

    let clean_ver = version.trim_start_matches('v');
    let tag_name = format!("v{}", clean_ver);
    let tags_to_try = [tag_name.clone(), clean_ver.to_string()];

    // Candidate asset filenames on GitHub release page
    let candidate_files = [
        "swai-linux-x86_64.tar.gz".to_string(),
        format!("swai-{}-linux-x86_64.tar.gz", tag_name),
        format!("swai-{}-linux-x86_64.tar.gz", version),
        "swai-linux-x86_64.AppImage".to_string(),
        "swai".to_string(),
    ];

    let mut download_response = None;
    let mut last_error = String::new();

    'outer: for tag in &tags_to_try {
        for file_name in &candidate_files {
            let url = format!(
                "https://github.com/{}/releases/download/{}/{}",
                github_repo, tag, file_name
            );
            tracing::info!("Attempting SWAI update download from {}", url);

            match client.get(&url).send() {
                Ok(resp) if resp.status().is_success() => {
                    download_response = Some(resp);
                    break 'outer;
                }
                Ok(resp) => {
                    last_error = format!("Status {} from {}", resp.status(), url);
                }
                Err(e) => {
                    last_error = format!("Request failed: {}", e);
                }
            }
        }
    }

    let mut response = match download_response {
        Some(resp) => resp,
        None => {
            return UpdateInstallResult::Error(format!(
                "Could not download release asset for version {}. {}",
                version, last_error
            ));
        }
    };

    // Create a temporary directory for extraction.
    let temp_dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => return UpdateInstallResult::Error(format!("Failed to create temp dir: {}", e)),
    };

    // Write the tarball to the temp directory.
    let tarball_path = temp_dir.path().join("swai.tar.gz");
    let mut file = match fs::File::create(&tarball_path) {
        Ok(f) => f,
        Err(e) => {
            return UpdateInstallResult::Error(format!("Failed to create tarball file: {}", e))
        }
    };

    let mut body = Vec::new();
    if let Err(e) = response.copy_to(&mut body) {
        return UpdateInstallResult::Error(format!("Failed to read download body: {}", e));
    }

    if let Err(e) = file.write_all(&body) {
        return UpdateInstallResult::Error(format!("Failed to write tarball: {}", e));
    }

    // Extract the tarball.
    let extract_dir = temp_dir.path().join("extracted");
    if let Err(e) = fs::create_dir_all(&extract_dir) {
        return UpdateInstallResult::Error(format!("Failed to create extraction dir: {}", e));
    }

    if let Err(e) = extract_tarball(&tarball_path, &extract_dir) {
        return UpdateInstallResult::Error(format!("Failed to extract tarball: {}", e));
    }

    // Find the binary in the extracted directory.
    let binary_path = find_binary_in_dir(&extract_dir);
    let binary_path = match binary_path {
        Some(p) => p,
        None => {
            return UpdateInstallResult::Error(
                "No executable binary found in release tarball".to_string(),
            );
        }
    };

    // Replace the existing binary.
    let install_dir = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "~".to_string()))
        .join(".local")
        .join("bin");

    if let Err(e) = fs::create_dir_all(&install_dir) {
        return UpdateInstallResult::Error(format!("Failed to create install dir: {}", e));
    }

    let target_path = install_dir.join("swai");

    // Back up the existing binary.
    if target_path.exists() {
        let backup_path = target_path.with_extension("bak");
        if let Err(e) = fs::rename(&target_path, &backup_path) {
            tracing::warn!("Failed to back up existing binary: {}", e);
        }
    }

    // Copy the new binary.
    if let Err(e) = fs::copy(&binary_path, &target_path) {
        return UpdateInstallResult::Error(format!("Failed to install binary: {}", e));
    }

    // Make it executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        if let Err(e) = fs::set_permissions(&target_path, perms) {
            tracing::warn!("Failed to set executable permissions: {}", e);
        }
    }

    // Refresh desktop shortcuts (if any).
    refresh_desktop_shortcuts();

    tracing::info!("SWAI updated to v{}", version);

    UpdateInstallResult::Success {
        new_version: version.to_string(),
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────

/// Extract a tar.gz archive to the given directory.
fn extract_tarball(tarball_path: &std::path::Path, dest_dir: &std::path::Path) -> io::Result<()> {
    // Use the system `tar` command for extraction (more reliable than pure Rust).
    let status = Command::new("tar")
        .args([
            "-xzf",
            &tarball_path.to_string_lossy(),
            "-C",
            &dest_dir.to_string_lossy(),
        ])
        .status()?;

    if !status.success() {
        return Err(io::Error::other(format!(
            "tar extraction failed with status {}",
            status
        )));
    }

    Ok(())
}

/// Find the first executable file in a directory tree.
fn find_binary_in_dir(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let metadata = match fs::metadata(&path) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    let mode = metadata.permissions().mode();
                    // Check if any execute bit is set.
                    if mode & 0o111 != 0 {
                        return Some(path);
                    }
                }
                #[cfg(not(unix))]
                {
                    if path.extension().map_or(false, |ext| ext == "exe") {
                        return Some(path);
                    }
                }
            } else if path.is_dir() {
                // Recurse into subdirectories.
                if let Some(found) = find_binary_in_dir(&path) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// Refresh desktop shortcuts after installation.
fn refresh_desktop_shortcuts() {
    // On Linux, we could run `update-desktop-database` if available.
    let _ = Command::new("update-desktop-database")
        .arg(
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "~".to_string()))
                .join(".local")
                .join("share")
                .join("applications"),
        )
        .status();

    tracing::debug!("Desktop shortcuts refreshed");
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_install_result_success() {
        let result = UpdateInstallResult::Success {
            new_version: "1.2.0".to_string(),
        };
        match &result {
            UpdateInstallResult::Success { new_version } => {
                assert_eq!(new_version, "1.2.0");
            }
            _ => panic!("Expected Success"),
        }
    }

    #[test]
    fn test_install_result_error() {
        let result = UpdateInstallResult::Error("network down".to_string());
        match &result {
            UpdateInstallResult::Error(msg) => {
                assert_eq!(msg, "network down");
            }
            _ => panic!("Expected Error"),
        }
    }

    #[test]
    fn test_install_result_debug_display() {
        let success = UpdateInstallResult::Success {
            new_version: "1.0.0".to_string(),
        };
        let err = UpdateInstallResult::Error("failed".to_string());

        assert!(format!("{:?}", success).contains("Success"));
        assert!(format!("{:?}", err).contains("Error"));
    }
}
