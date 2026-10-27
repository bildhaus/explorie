//! Entry points for the `fuzz/` cargo-fuzz targets; not a supported API.
//!
//! Each wrapper hands untrusted input to the same private parser or check the
//! application uses, so the fuzzers exercise production code, not a copy.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

pub use crate::archive::ArchiveEntry;

/// Parse `7zz l -slt -ba` output the way the bundled 7-Zip backend does.
pub fn parse_7zip_listing(
    text: &str,
    archive: &Path,
    max_entries: usize,
) -> io::Result<Vec<ArchiveEntry>> {
    crate::archive::sevenzip::parse_listing_entries(text, archive, max_entries)
}

/// Validate an archive entry name (traversal, roots, reserved names).
pub fn validate_archive_entry_path(entry_path: &Path) -> io::Result<()> {
    crate::archive::validate_archive_entry_path(entry_path)
}

/// Resolve an entry under `root`, rejecting zip-slip escapes and link ancestors.
pub fn safe_extraction_path(root: &Path, entry_path: &Path) -> io::Result<PathBuf> {
    crate::archive::ensure_safe_extraction_path(root, entry_path)
}

/// Parse `.explorie.json` content and apply the typed custom-field checks
/// (dates, URLs, closed enums) that guard every metadata write.
pub fn parse_explorie_document(
    content: &str,
) -> io::Result<HashMap<String, HashMap<String, serde_json::Value>>> {
    let document: HashMap<String, HashMap<String, serde_json::Value>> =
        serde_json::from_str(content)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    crate::validate_explorie_document(&document)?;
    Ok(document)
}
