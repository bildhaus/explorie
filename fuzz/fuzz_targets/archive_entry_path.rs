//! Archive entry names decide where extracted bytes land (zip-slip).
#![no_main]

use explorie_core::fuzzing::{safe_extraction_path, validate_archive_entry_path};
use libfuzzer_sys::fuzz_target;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join("explorie-fuzz-extraction-root");
        std::fs::create_dir_all(&root).expect("create fuzz extraction root");
        // Canonical, so the link-ancestor check only ever sees real directories.
        root.canonicalize()
            .expect("canonicalize fuzz extraction root")
    })
}

#[cfg(unix)]
fn entry_path(data: &[u8]) -> Option<&Path> {
    use std::os::unix::ffi::OsStrExt;
    Some(Path::new(std::ffi::OsStr::from_bytes(data)))
}

#[cfg(not(unix))]
fn entry_path(data: &[u8]) -> Option<&Path> {
    std::str::from_utf8(data).ok().map(Path::new)
}

fuzz_target!(|data: &[u8]| {
    let Some(entry) = entry_path(data) else {
        return;
    };
    let validated = validate_archive_entry_path(entry).is_ok();
    // Rejections are always fine (including I/O errors on absurd names).
    let Ok(destination) = safe_extraction_path(root(), entry) else {
        return;
    };
    assert!(
        validated,
        "extraction accepted a name validation rejects: {entry:?}"
    );
    let relative = destination
        .strip_prefix(root())
        .unwrap_or_else(|_| panic!("{entry:?} escaped the extraction root: {destination:?}"));
    assert!(
        relative
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)),
        "{entry:?} resolved to a non-normal component: {relative:?}"
    );
    assert!(
        relative
            .components()
            .any(|component| matches!(component, Component::Normal(_))),
        "{entry:?} resolved to the extraction root itself"
    );
});
