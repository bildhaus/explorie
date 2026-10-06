//! Copy, move and rename on a real exFAT volume, which has neither an
//! exclusive rename (`renamex_np(RENAME_EXCL)` fails with ENOTSUP) nor hard
//! links. Run with:
//! `cargo test -p explorie-core --test exfat_volume -- --ignored`
#![cfg(target_os = "macos")]

use explorie_core::{
    ConflictPolicy, FileOperationKind, FileOperationRequest, perform_file_operation,
    rename_noreplace,
};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

struct AttachedImage {
    mount_point: PathBuf,
}

impl AttachedImage {
    fn create(directory: &Path) -> Self {
        let image = directory.join("explorie-exfat.dmg");
        let mount_point = directory.join("volume");
        fs::create_dir(&mount_point).unwrap();
        hdiutil(&[
            "create".as_ref(),
            "-size".as_ref(),
            "20m".as_ref(),
            "-fs".as_ref(),
            "ExFAT".as_ref(),
            "-volname".as_ref(),
            "EXPLORIE".as_ref(),
            image.as_os_str(),
        ]);
        hdiutil(&[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-mountpoint".as_ref(),
            mount_point.as_os_str(),
            image.as_os_str(),
        ]);
        Self { mount_point }
    }
}

impl Drop for AttachedImage {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil")
            .args(["detach", "-force"])
            .arg(&self.mount_point)
            .output();
    }
}

fn hdiutil(args: &[&std::ffi::OsStr]) {
    let output = Command::new("hdiutil").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "hdiutil {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn operate(
    kind: FileOperationKind,
    source: &Path,
    destination: &Path,
    conflict_policy: ConflictPolicy,
) -> io::Result<Vec<PathBuf>> {
    perform_file_operation(
        FileOperationRequest {
            kind,
            sources: vec![source.to_path_buf()],
            destination: Some(destination.to_path_buf()),
            conflict_policy,
        },
        &AtomicBool::new(false),
        |_| {},
    )
    .map(|result| result.targets)
}

fn explorie_leftovers(directory: &Path) -> Vec<String> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".explorie-"))
        .collect()
}

#[test]
#[ignore = "creates and attaches an exFAT disk image with hdiutil"]
fn copy_move_and_rename_work_on_exfat() {
    let temp = tempfile::tempdir().unwrap();
    let image = AttachedImage::create(temp.path());
    let volume = image.mount_point.as_path();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::write(source.join("nested/data.txt"), b"content").unwrap();
    std::os::unix::fs::symlink("data.txt", source.join("nested/link")).unwrap();
    let item = temp.path().join("item.txt");
    fs::write(&item, b"new").unwrap();

    // Copy onto the volume; conflicts are detected and renamed, never replaced.
    operate(
        FileOperationKind::Copy,
        &source,
        volume,
        ConflictPolicy::Error,
    )
    .unwrap();
    assert_eq!(
        fs::read(volume.join("source/nested/data.txt")).unwrap(),
        b"content"
    );
    let conflict = operate(
        FileOperationKind::Copy,
        &source,
        volume,
        ConflictPolicy::Error,
    )
    .unwrap_err();
    assert_eq!(conflict.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        operate(
            FileOperationKind::Copy,
            &source,
            volume,
            ConflictPolicy::Rename
        )
        .unwrap(),
        vec![volume.join("source 2")]
    );
    fs::write(volume.join("item.txt"), b"old").unwrap();
    operate(
        FileOperationKind::Copy,
        &item,
        volume,
        ConflictPolicy::Replace,
    )
    .unwrap();
    assert_eq!(fs::read(volume.join("item.txt")).unwrap(), b"new");

    // Same-volume moves are renames on the volume.
    let moved = volume.join("moved");
    fs::create_dir(&moved).unwrap();
    operate(
        FileOperationKind::Move,
        &volume.join("source"),
        &moved,
        ConflictPolicy::Error,
    )
    .unwrap();
    operate(
        FileOperationKind::Move,
        &volume.join("item.txt"),
        &moved,
        ConflictPolicy::Error,
    )
    .unwrap();
    assert_eq!(
        fs::read(moved.join("source/nested/data.txt")).unwrap(),
        b"content"
    );
    assert_eq!(fs::read(moved.join("item.txt")).unwrap(), b"new");
    assert!(!volume.join("source").exists());

    // Renames refuse to replace existing files and folders.
    fs::write(moved.join("other.txt"), b"other").unwrap();
    let error = rename_noreplace(&moved.join("item.txt"), &moved.join("other.txt")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    fs::create_dir(moved.join("empty")).unwrap();
    let error = rename_noreplace(&moved.join("source"), &moved.join("empty")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(moved.join("other.txt")).unwrap(), b"other");
    rename_noreplace(&moved.join("item.txt"), &moved.join("renamed.txt")).unwrap();
    rename_noreplace(&moved.join("source"), &moved.join("renamed-folder")).unwrap();
    assert_eq!(fs::read(moved.join("renamed.txt")).unwrap(), b"new");
    assert!(moved.join("renamed-folder/nested/data.txt").is_file());

    // A cross-volume move copies, verifies and commits onto the volume.
    operate(
        FileOperationKind::Move,
        &source,
        volume,
        ConflictPolicy::Error,
    )
    .unwrap();
    assert!(!source.exists());
    assert_eq!(
        fs::read(volume.join("source/nested/data.txt")).unwrap(),
        b"content"
    );
    assert_eq!(
        fs::read_link(volume.join("source/nested/link")).unwrap(),
        Path::new("data.txt")
    );

    assert!(explorie_leftovers(volume).is_empty());
    assert!(explorie_leftovers(&moved).is_empty());
}
