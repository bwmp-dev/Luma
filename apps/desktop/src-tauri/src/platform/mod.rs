use std::path::PathBuf;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
use serde::Serialize;

pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let var = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let var = std::env::var_os("HOME");
    var.map(PathBuf::from).filter(|path| path.is_dir())
}

// Dialogs return native paths on desktop and `file://` URLs on iOS. Convert
// the latter before filesystem validation or access while leaving ordinary
// paths unchanged.
pub(crate) fn picker_path(value: &str) -> Option<PathBuf> {
    let is_file_url = value
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file:"));
    if !is_file_url {
        return Some(PathBuf::from(value));
    }
    if value.get(5..7) != Some("//") {
        return None;
    }

    let url = tauri::Url::parse(value).ok()?;
    if url.scheme() != "file" || url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    url.to_file_path().ok()
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedShell {
    pub id: String,
    pub name: String,
    pub path: String,
    pub args: Vec<String>,
}

#[cfg(windows)]
fn find_in_path(executable: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(executable);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn shell(id: &str, name: &str, path: PathBuf, args: &[&str]) -> DetectedShell {
    DetectedShell {
        id: id.into(),
        name: name.into(),
        path: path.to_string_lossy().into_owned(),
        args: args.iter().map(|a| a.to_string()).collect(),
    }
}

/// Detect shells available on this machine, ordered by preference. The first
/// entry is the platform default when the user has not chosen one.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn detect_shells() -> Vec<DetectedShell> {
    let mut shells = Vec::new();

    #[cfg(windows)]
    {
        let system32 = std::env::var_os("SystemRoot")
            .map(|root| PathBuf::from(root).join("System32"))
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32"));

        if let Some(pwsh) = find_in_path("pwsh.exe") {
            shells.push(shell("pwsh", "PowerShell", pwsh, &["-NoLogo"]));
        }
        let windows_powershell = system32.join(r"WindowsPowerShell\v1.0\powershell.exe");
        if windows_powershell.is_file() {
            shells.push(shell(
                "powershell",
                "Windows PowerShell",
                windows_powershell,
                &["-NoLogo"],
            ));
        }
        let cmd = std::env::var_os("ComSpec")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .unwrap_or_else(|| system32.join("cmd.exe"));
        if cmd.is_file() {
            shells.push(shell("cmd", "Command Prompt", cmd, &[]));
        }
        let wsl = system32.join("wsl.exe");
        if wsl.is_file() {
            shells.push(shell("wsl", "WSL", wsl, &[]));
        }
        for git_bash in [
            PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"),
            PathBuf::from(r"C:\Program Files (x86)\Git\bin\bash.exe"),
        ] {
            if git_bash.is_file() {
                shells.push(shell("git-bash", "Git Bash", git_bash, &["-i", "-l"]));
                break;
            }
        }
    }

    #[cfg(not(windows))]
    {
        // The user's login shell first.
        if let Ok(login_shell) = std::env::var("SHELL") {
            let path = PathBuf::from(&login_shell);
            if path.is_file() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Shell".into());
                shells.push(shell("login", &format!("Default ({name})"), path, &["-l"]));
            }
        }
        for (id, name, candidates) in [
            ("bash", "Bash", vec!["/bin/bash", "/usr/bin/bash"]),
            ("zsh", "Zsh", vec!["/bin/zsh", "/usr/bin/zsh"]),
            (
                "fish",
                "Fish",
                vec![
                    "/usr/bin/fish",
                    "/usr/local/bin/fish",
                    "/opt/homebrew/bin/fish",
                ],
            ),
        ] {
            if let Some(path) = candidates
                .into_iter()
                .map(PathBuf::from)
                .find(|p| p.is_file())
            {
                let duplicate = shells
                    .iter()
                    .any(|s: &DetectedShell| s.path == path.to_string_lossy());
                if !duplicate {
                    shells.push(shell(id, name, path, &["-l"]));
                }
            }
        }
    }

    shells
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_paths_decode_file_urls() {
        let path = std::env::temp_dir().join("Luma picker file.txt");
        let url = tauri::Url::from_file_path(&path).unwrap();
        assert_eq!(picker_path(url.as_str()), Some(path));
    }

    #[test]
    fn picker_paths_reject_invalid_file_urls() {
        assert_eq!(picker_path("file:relative/path"), None);
        assert_eq!(picker_path("file:///tmp/file?query"), None);
    }

    #[test]
    fn picker_paths_preserve_native_paths() {
        let path = if cfg!(windows) {
            r"C:\Users\alice\file.txt"
        } else {
            "/home/alice/file.txt"
        };
        assert_eq!(picker_path(path), Some(PathBuf::from(path)));
    }

    #[test]
    fn detects_at_least_one_shell() {
        let shells = detect_shells();
        assert!(!shells.is_empty(), "no shells detected");
        for s in &shells {
            assert!(
                std::path::Path::new(&s.path).is_file(),
                "detected shell path missing: {}",
                s.path
            );
        }
    }
}
