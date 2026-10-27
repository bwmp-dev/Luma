use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::errors::{LumaError, Result};

/*
 * External editors for remote file editing. The list is detected here and the
 * frontend only ever names an entry by id, so nothing it sends can choose what
 * runs. Every editor is started directly with the working copy as its last
 * argument: no shell, and never the OS "open with default app" association,
 * which for a downloaded script would run it rather than open it.
 */

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EditorInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct DetectedEditor {
    pub id: &'static str,
    pub name: &'static str,
    program: PathBuf,
    leading_args: Vec<OsString>,
}

impl DetectedEditor {
    pub fn info(&self) -> EditorInfo {
        EditorInfo {
            id: self.id.into(),
            name: self.name.into(),
        }
    }

    /// Open `file` in the editor and return without waiting for it to exit:
    /// many editors hand the file to an already-running window and quit, so
    /// the process lifetime says nothing about when editing ends.
    pub fn launch(&self, file: &Path) -> Result<()> {
        let mut child = Command::new(&self.program)
            .args(&self.leading_args)
            .arg(file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                LumaError::InvalidInput(format!("could not start {}: {error}", self.name))
            })?;
        // Reap the child so a long session does not accumulate zombies.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

pub fn find(id: &str) -> Option<DetectedEditor> {
    detect().into_iter().find(|editor| editor.id == id)
}

/// Editors installed on this machine, in the order the picker lists them.
pub fn detect() -> Vec<DetectedEditor> {
    let mut editors = Vec::new();
    for candidate in candidates() {
        if editors
            .iter()
            .any(|existing: &DetectedEditor| existing.id == candidate.id)
        {
            continue;
        }
        if let Some(editor) = candidate.resolve() {
            editors.push(editor);
        }
    }
    editors
}

struct Candidate {
    id: &'static str,
    name: &'static str,
    location: Location,
}

enum Location {
    /// Absolute paths to try in order.
    #[cfg_attr(not(windows), allow(dead_code))]
    Paths(Vec<PathBuf>),
    /// Program names looked up on PATH.
    #[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
    OnPath(&'static [&'static str]),
    /// macOS application bundles, opened with `open -a <bundle>`.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    AppBundle(Vec<PathBuf>),
}

impl Candidate {
    fn resolve(self) -> Option<DetectedEditor> {
        let (program, leading_args) = match self.location {
            Location::Paths(paths) => (paths.into_iter().find(|path| path.is_file())?, vec![]),
            Location::OnPath(names) => (names.iter().find_map(|name| find_on_path(name))?, vec![]),
            Location::AppBundle(bundles) => {
                let bundle = bundles.into_iter().find(|path| path.is_dir())?;
                let open = PathBuf::from("/usr/bin/open");
                if !open.is_file() {
                    return None;
                }
                // `-a` names the application explicitly, so Launch Services
                // never falls back to the file type's default handler.
                (open, vec![OsString::from("-a"), bundle.into_os_string()])
            }
        };
        Some(DetectedEditor {
            id: self.id,
            name: self.name,
            program,
            leading_args,
        })
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(windows)]
fn candidates() -> Vec<Candidate> {
    let env_dir = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let local = env_dir("LOCALAPPDATA");
    let program_files: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|name| env_dir(name))
        .collect();
    let under = |roots: &[PathBuf], relative: &str| -> Vec<PathBuf> {
        roots.iter().map(|root| root.join(relative)).collect()
    };
    let local_roots: Vec<PathBuf> = local.into_iter().map(|dir| dir.join("Programs")).collect();
    let user_and_machine = |relative: &str| {
        let mut paths = under(&local_roots, relative);
        paths.extend(under(&program_files, relative));
        paths
    };
    let system32 = env_dir("SystemRoot")
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32");

    vec![
        Candidate {
            id: "vscode",
            name: "Visual Studio Code",
            location: Location::Paths(user_and_machine(r"Microsoft VS Code\Code.exe")),
        },
        Candidate {
            id: "vscode-insiders",
            name: "VS Code Insiders",
            location: Location::Paths(user_and_machine(
                r"Microsoft VS Code Insiders\Code - Insiders.exe",
            )),
        },
        Candidate {
            id: "cursor",
            name: "Cursor",
            location: Location::Paths(user_and_machine(r"cursor\Cursor.exe")),
        },
        Candidate {
            id: "vscodium",
            name: "VSCodium",
            location: Location::Paths(user_and_machine(r"VSCodium\VSCodium.exe")),
        },
        Candidate {
            id: "zed",
            name: "Zed",
            location: Location::Paths(user_and_machine(r"Zed\Zed.exe")),
        },
        Candidate {
            id: "sublime",
            name: "Sublime Text",
            location: Location::Paths(under(&program_files, r"Sublime Text\sublime_text.exe")),
        },
        Candidate {
            id: "notepad-plus-plus",
            name: "Notepad++",
            location: Location::Paths(under(&program_files, r"Notepad++\notepad++.exe")),
        },
        Candidate {
            id: "notepad",
            name: "Notepad",
            location: Location::Paths(vec![system32.join("notepad.exe")]),
        },
    ]
}

#[cfg(target_os = "macos")]
fn candidates() -> Vec<Candidate> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = crate::platform::home_dir() {
        roots.push(home.join("Applications"));
    }
    let bundle =
        |name: &str| -> Vec<PathBuf> { roots.iter().map(|root| root.join(name)).collect() };
    vec![
        Candidate {
            id: "vscode",
            name: "Visual Studio Code",
            location: Location::AppBundle(bundle("Visual Studio Code.app")),
        },
        Candidate {
            id: "vscode-insiders",
            name: "VS Code Insiders",
            location: Location::AppBundle(bundle("Visual Studio Code - Insiders.app")),
        },
        Candidate {
            id: "cursor",
            name: "Cursor",
            location: Location::AppBundle(bundle("Cursor.app")),
        },
        Candidate {
            id: "vscodium",
            name: "VSCodium",
            location: Location::AppBundle(bundle("VSCodium.app")),
        },
        Candidate {
            id: "zed",
            name: "Zed",
            location: Location::AppBundle(bundle("Zed.app")),
        },
        Candidate {
            id: "sublime",
            name: "Sublime Text",
            location: Location::AppBundle(bundle("Sublime Text.app")),
        },
        Candidate {
            id: "bbedit",
            name: "BBEdit",
            location: Location::AppBundle(bundle("BBEdit.app")),
        },
        Candidate {
            id: "textedit",
            name: "TextEdit",
            location: Location::AppBundle(vec![
                PathBuf::from("/System/Applications/TextEdit.app"),
                PathBuf::from("/Applications/TextEdit.app"),
            ]),
        },
    ]
}

#[cfg(all(unix, not(target_os = "macos")))]
fn candidates() -> Vec<Candidate> {
    let on_path = |id, name, programs| Candidate {
        id,
        name,
        location: Location::OnPath(programs),
    };
    vec![
        on_path("vscode", "Visual Studio Code", &["code"]),
        on_path("vscode-insiders", "VS Code Insiders", &["code-insiders"]),
        on_path("cursor", "Cursor", &["cursor"]),
        on_path("vscodium", "VSCodium", &["codium"]),
        on_path("zed", "Zed", &["zed", "zeditor"]),
        on_path("sublime", "Sublime Text", &["subl"]),
        on_path("kate", "Kate", &["kate"]),
        on_path("gnome-text-editor", "Text Editor", &["gnome-text-editor"]),
        on_path("gedit", "gedit", &["gedit"]),
        on_path("kwrite", "KWrite", &["kwrite"]),
        on_path("mousepad", "Mousepad", &["mousepad"]),
        on_path("xed", "Xed", &["xed"]),
        on_path("pluma", "Pluma", &["pluma"]),
        on_path("geany", "Geany", &["geany"]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detected_ids_are_unique_and_resolve_to_existing_programs() {
        let editors = detect();
        let mut ids: Vec<_> = editors.iter().map(|editor| editor.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), editors.len());
        for editor in &editors {
            assert!(editor.program.is_file(), "{} has no program", editor.id);
            assert!(find(editor.id).is_some());
        }
        assert!(find("../../bin/sh").is_none());
        assert!(find("").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn notepad_is_always_offered_on_windows() {
        assert!(find("notepad").is_some());
    }
}
