//! Listing behavior that Finder users rely on: resilience to unreadable
//! items, hidden flags, packages, links and cloud placeholders.

use explorie_core::{FileEntry, is_cloud_placeholder, list_dir};
use std::fs;
use tempfile::tempdir;

fn entry<'a>(entries: &'a [FileEntry], name: &str) -> &'a FileEntry {
    entries
        .iter()
        .find(|entry| entry.path.file_name().is_some_and(|file| file == name))
        .unwrap_or_else(|| panic!("{name} should be listed"))
}

#[cfg(unix)]
#[test]
fn items_whose_metadata_cannot_be_read_are_still_listed_with_a_warning() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempdir().unwrap();
    let locked = temp_dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("report.txt"), b"report").unwrap();
    fs::create_dir(locked.join("drafts")).unwrap();
    // Readable but not searchable: names can be listed, but stat fails.
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o444)).unwrap();
    let stat_blocked = fs::symlink_metadata(locked.join("report.txt")).is_err();

    let listing = explorie_core::list_dir_with_warnings(&locked, true);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    if !stat_blocked {
        // Elevated test runners can stat through mode-444 directories.
        return;
    }

    let listing = listing.unwrap();
    assert_eq!(listing.entries.len(), 2);
    let report = entry(&listing.entries, "report.txt");
    assert!(!report.is_dir);
    assert_eq!(report.size, 0);
    assert!(entry(&listing.entries, "drafts").is_dir);
    assert_eq!(listing.warnings.len(), 1, "{:?}", listing.warnings);
    assert!(
        listing.warnings[0]
            .starts_with("Details for 2 items could not be read: drafts, report.txt"),
        "{:?}",
        listing.warnings
    );
}

#[test]
fn only_dotfiles_and_platform_hidden_items_are_hidden() {
    let temp_dir = tempdir().unwrap();
    fs::write(temp_dir.path().join(".env"), b"secret").unwrap();
    fs::write(temp_dir.path().join("visible.txt"), b"shown").unwrap();

    let entries = list_dir(temp_dir.path()).unwrap();
    assert!(entry(&entries, ".env").hidden);
    assert!(!entry(&entries, "visible.txt").hidden);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_hidden_flag_hides_items_like_finder() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let temp_dir = tempdir().unwrap();
    let library = temp_dir.path().join("Library");
    fs::create_dir(&library).unwrap();
    let c_path = CString::new(library.as_os_str().as_bytes()).unwrap();
    // SAFETY: c_path is a valid NUL-terminated path; this is `chflags hidden`.
    assert_eq!(
        unsafe { libc::chflags(c_path.as_ptr(), libc::UF_HIDDEN) },
        0
    );

    let entries = list_dir(temp_dir.path()).unwrap();
    let library = entry(&entries, "Library");
    assert!(library.hidden);
    assert!(library.is_dir);
}

#[cfg(target_os = "macos")]
fn set_finder_bundle_bit(path: &std::path::Path) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let mut finder_info = [0_u8; 32];
    finder_info[8] = 0x20; // kHasBundle (0x2000), big-endian finderFlags
    // SAFETY: The path and attribute name are NUL-terminated and the value
    // pointer/length describe the local buffer.
    let result = unsafe {
        libc::setxattr(
            c_path.as_ptr(),
            c"com.apple.FinderInfo".as_ptr(),
            finder_info.as_ptr().cast(),
            finder_info.len(),
            0,
            0,
        )
    };
    assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_packages_are_flagged_but_remain_directories() {
    let temp_dir = tempdir().unwrap();
    let root = temp_dir.path();
    for name in [
        "Tool.app",
        "Letter.RTFD",
        "Budget.numbers",
        "plain",
        "Custom.thing",
    ] {
        fs::create_dir(root.join(name)).unwrap();
    }
    fs::write(root.join("Tool.app").join("Info.plist"), b"<plist/>").unwrap();
    fs::write(root.join("notes.pages"), b"a flat file is not a package").unwrap();
    set_finder_bundle_bit(&root.join("Custom.thing"));

    let entries = explorie_core::list_dir_with_sizes(root, true).unwrap();
    for name in ["Tool.app", "Letter.RTFD", "Budget.numbers", "Custom.thing"] {
        let package = entry(&entries, name);
        assert!(package.is_package, "{name} should be a package");
        assert!(package.is_dir, "{name} should stay a directory");
    }
    assert_eq!(entry(&entries, "Tool.app").size, 8);
    assert!(!entry(&entries, "plain").is_package);
    assert!(!entry(&entries, "notes.pages").is_package);
}

#[cfg(target_os = "macos")]
#[test]
fn system_applications_are_packages() {
    let entries = list_dir(std::path::Path::new("/System/Applications")).unwrap();
    let text_edit = entry(&entries, "TextEdit.app");
    assert!(text_edit.is_package);
    assert!(text_edit.is_dir);
}

#[cfg(unix)]
#[test]
fn links_report_their_target_kind_and_folder_sizes_never_follow_them() {
    use std::os::unix::fs::symlink;

    let temp_dir = tempdir().unwrap();
    let root = temp_dir.path().join("root");
    let outside = temp_dir.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("large.bin"), vec![0_u8; 4096]).unwrap();

    let project = root.join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("main.rs"), b"fn main() {}").unwrap();
    symlink(&outside, project.join("vendor")).unwrap();
    symlink(&outside, root.join("folder-link")).unwrap();
    symlink(outside.join("large.bin"), root.join("file-link")).unwrap();
    symlink(root.join("missing"), root.join("broken-link")).unwrap();

    let entries = explorie_core::list_dir_with_sizes(&root, true).unwrap();
    let folder_link = entry(&entries, "folder-link");
    assert!(folder_link.is_symlink);
    assert!(folder_link.link_target_is_dir);
    assert!(!folder_link.is_dir);
    assert!(folder_link.size < 4096, "links report their own size");
    assert!(!entry(&entries, "file-link").link_target_is_dir);
    assert!(!entry(&entries, "broken-link").link_target_is_dir);

    let project = entry(&entries, "project");
    assert!(!project.link_target_is_dir);
    assert_eq!(project.size, 12, "the vendor link must not be traversed");
}

#[test]
fn regular_files_are_not_cloud_placeholders() {
    let temp_dir = tempdir().unwrap();
    let path = temp_dir.path().join("local.txt");
    fs::write(&path, b"on disk").unwrap();

    assert!(!is_cloud_placeholder(&fs::symlink_metadata(&path).unwrap()));
    assert!(!entry(&list_dir(temp_dir.path()).unwrap(), "local.txt").is_cloud_placeholder);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_links_to_packages_are_packages_and_links_to_folders_are_not() {
    use std::os::unix::fs::symlink;

    let temp_dir = tempdir().unwrap();
    let root = temp_dir.path();
    fs::create_dir(root.join("Tool.app")).unwrap();
    fs::create_dir(root.join("Custom.thing")).unwrap();
    fs::create_dir(root.join("plain")).unwrap();
    set_finder_bundle_bit(&root.join("Custom.thing"));
    symlink(root.join("Tool.app"), root.join("tool-link")).unwrap();
    symlink(root.join("Custom.thing"), root.join("custom-link")).unwrap();
    symlink(root.join("plain"), root.join("plain-link")).unwrap();

    let entries = list_dir(root).unwrap();
    for name in ["tool-link", "custom-link"] {
        let link = entry(&entries, name);
        assert!(link.is_package, "{name} opens like its package");
        assert!(link.link_target_is_dir);
        assert!(!link.is_dir);
    }
    let plain_link = entry(&entries, "plain-link");
    assert!(!plain_link.is_package);
    assert!(plain_link.link_target_is_dir);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_listings_carry_finder_tags_for_items_with_extended_attributes() {
    use explorie_core::{FinderTag, write_finder_tags};

    let temp_dir = tempdir().unwrap();
    let tagged = temp_dir.path().join("tagged.txt");
    let plain = temp_dir.path().join("plain.txt");
    fs::write(&tagged, b"tagged").unwrap();
    fs::write(&plain, b"plain").unwrap();
    write_finder_tags(&tagged, &["Important\n6".to_string(), "Work".to_string()]).unwrap();

    let entries = list_dir(temp_dir.path()).unwrap();
    let listed = entry(&entries, "tagged.txt");
    assert!(listed.has_xattrs);
    assert_eq!(
        listed.tags,
        vec![
            FinderTag {
                name: "Important".to_string(),
                color: 6
            },
            FinderTag {
                name: "Work".to_string(),
                color: 0
            },
        ]
    );
    assert!(entry(&entries, "plain.txt").tags.is_empty());

    // Single-path reads (watcher patches, Spotlight hits) see the same tags.
    assert_eq!(
        explorie_core::entry_for_path(&tagged).unwrap().tags,
        listed.tags
    );
    let serialized = serde_json::to_value(entry(&entries, "plain.txt")).unwrap();
    assert!(serialized.get("tags").is_none(), "empty tags are omitted");
}

#[test]
fn file_entries_without_tags_still_deserialize() {
    let json = serde_json::json!({
        "id": "00000000-0000-0000-0000-000000000000",
        "path": "/tmp/legacy.txt",
        "size": 1,
        "modified": {"secs_since_epoch": 0, "nanos_since_epoch": 0},
        "hidden": false,
        "is_dir": false,
        "custom": {}
    });
    let entry: FileEntry = serde_json::from_value(json).unwrap();
    assert!(entry.tags.is_empty());
}
