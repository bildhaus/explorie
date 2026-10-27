//! Passive dependency hints, including while an integration is disabled.
//! Never start an executable or read connection credentials to populate settings.

use std::path::{Path, PathBuf};

pub(crate) fn detect_dependency(id: &str) -> Option<String> {
    Some(match id {
        "git" => git_status(),
        "obsidian" => obsidian_status(),
        "syncthing" => syncthing_status(),
        _ => return None,
    })
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn path_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    std::env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(&name))
        .find(|path| is_executable(path))
}

fn git_status() -> String {
    if let Some(path) = path_executable("git") {
        #[cfg(target_os = "macos")]
        if path.canonicalize().ok().as_deref() == Some(Path::new("/usr/bin/git")) {
            // Apple's launcher exists even without an installed developer toolchain.
            return "macOS Git launcher detected · Availability checked when enabled".into();
        }
        let _ = path;
        return "Git executable detected on PATH".into();
    }
    #[cfg(target_os = "macos")]
    if [
        "/opt/homebrew/bin/git",
        "/usr/local/bin/git",
        "/Library/Developer/CommandLineTools/usr/bin/git",
        "/Applications/Xcode.app/Contents/Developer/usr/bin/git",
    ]
    .iter()
    .any(|path| is_executable(Path::new(path)))
    {
        return "Git executable detected · Add Git to PATH to use the integration".into();
    }
    "Git executable not detected on PATH".into()
}

fn obsidian_status() -> String {
    #[cfg(windows)]
    {
        use winreg::{RegKey, enums::HKEY_CLASSES_ROOT};
        let registered = RegKey::predef(HKEY_CLASSES_ROOT)
            .open_subkey(r"obsidian\shell\open\command")
            .and_then(|key| key.get_value::<String, _>(""))
            .is_ok_and(|command| !command.trim().is_empty());
        if registered {
            return "Obsidian link handler registered".into();
        }
        if ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .any(|root| {
                root.join("Obsidian/Obsidian.exe").is_file()
                    || root.join("Programs/Obsidian/Obsidian.exe").is_file()
            })
        {
            return "Obsidian app detected · Link handler not detected".into();
        }
    }
    #[cfg(target_os = "macos")]
    if std::iter::once(PathBuf::from("/Applications/Obsidian.app"))
        .chain(dirs::home_dir().map(|home| home.join("Applications/Obsidian.app")))
        .any(|app| is_executable(&app.join("Contents/MacOS/Obsidian")))
    {
        return "Obsidian app detected".into();
    }
    "Obsidian app not detected in standard locations".into()
}

fn syncthing_config_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(root) = std::env::var_os("LOCALAPPDATA") {
        paths.push(PathBuf::from(root).join("Syncthing/config.xml"));
    }
    if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
        paths.push(PathBuf::from(root).join("syncthing/config.xml"));
    }
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join("Library/Application Support/Syncthing/config.xml"));
        paths.push(home.join(".local/state/syncthing/config.xml"));
        paths.push(home.join(".config/syncthing/config.xml"));
    }
    paths
}

fn syncthing_status() -> String {
    if syncthing_config_candidates()
        .iter()
        .any(|path| path.is_file())
    {
        "Syncthing configuration detected".into()
    } else if path_executable("syncthing").is_some() {
        "Syncthing executable detected · Configuration not detected".into()
    } else {
        "Syncthing configuration not detected in standard locations".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_plugins_have_no_dependency_hint() {
        assert_eq!(detect_dependency("third-party"), None);
    }

    #[test]
    fn executable_detection_requires_a_file_and_unix_execute_permission() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!is_executable(temp.path()));
        let path = temp.path().join("tool");
        assert!(!is_executable(&path));
        std::fs::write(&path, "never execute this fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(!is_executable(&path));
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(is_executable(&path));
    }
}
