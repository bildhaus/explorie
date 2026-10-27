//! `7zz l -slt -ba` output is attacker-influenced: entry names, attributes, and
//! sizes come straight from the archive being browsed.
#![no_main]

use explorie_core::fuzzing::{parse_7zip_listing, validate_archive_entry_path};
use libfuzzer_sys::fuzz_target;
use std::collections::HashSet;
use std::path::{Component, Path};

// Multi-entry containers and single-stream compressors take different branches.
const ARCHIVES: [&str; 4] = ["archive.7z", "archive.iso", "payload.tar.gz", "image.xz"];

fuzz_target!(|data: &[u8]| {
    let Some((&selector, listing)) = data.split_first() else {
        return;
    };
    // The backend rejects non-UTF-8 listings before parsing them.
    let Ok(text) = std::str::from_utf8(listing) else {
        return;
    };
    let archive = Path::new(ARCHIVES[usize::from(selector) % ARCHIVES.len()]);
    let max_entries = usize::from(selector >> 2) + 1;
    let Ok(entries) = parse_7zip_listing(text, archive, max_entries) else {
        return;
    };

    assert!(entries.len() <= max_entries, "entry limit exceeded");
    let mut seen = HashSet::new();
    for entry in &entries {
        assert!(
            !entry.path.contains(':') && !entry.path.starts_with(['/', '\\']),
            "absolute or drive-relative path accepted: {:?}",
            entry.path
        );
        let path = entry.path.replace('\\', "/");
        validate_archive_entry_path(Path::new(&path)).expect("accepted entry must be a safe path");
        let normalized = Path::new(&path)
            .components()
            .filter_map(|component| match component {
                Component::Normal(name) => Some(name.to_string_lossy()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/");
        assert!(
            seen.insert(normalized.to_lowercase()),
            "case-insensitive duplicate accepted: {:?}",
            entry.path
        );
    }
});
