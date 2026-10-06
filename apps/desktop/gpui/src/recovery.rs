use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MARKER_FILE: &str = "runtime-dirty-v1.json";

#[derive(Clone, Debug)]
pub struct RecoveryMarker {
    path: PathBuf,
    previous_session_unclean: bool,
}

impl RecoveryMarker {
    pub fn begin(config_dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(config_dir)?;
        let path = config_dir.join(MARKER_FILE);
        let previous_session_unclean = path.is_file();
        let started_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let marker = serde_json::json!({
            "version": 1,
            "pid": std::process::id(),
            "startedAtUnixMs": started_at_unix_ms,
        });
        fs::write(
            &path,
            serde_json::to_vec_pretty(&marker).expect("runtime marker is serializable"),
        )?;
        Ok(Self {
            path,
            previous_session_unclean,
        })
    }

    pub fn previous_session_unclean(&self) -> bool {
        self.previous_session_unclean
    }

    pub fn clear(&self) -> io::Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn marker_distinguishes_unclean_restart_from_clean_shutdown() {
        let root = std::env::temp_dir().join(format!("explorie-recovery-{}", Uuid::new_v4()));
        let first = RecoveryMarker::begin(&root).unwrap();
        assert!(!first.previous_session_unclean());
        assert!(root.join(MARKER_FILE).is_file());

        let restarted = RecoveryMarker::begin(&root).unwrap();
        assert!(restarted.previous_session_unclean());
        restarted.clear().unwrap();
        assert!(!root.join(MARKER_FILE).exists());

        let clean = RecoveryMarker::begin(&root).unwrap();
        assert!(!clean.previous_session_unclean());
        clean.clear().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
