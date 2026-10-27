//! # explorie Core
//!
//! Core library for the explorie file manager, providing file system operations
//! and metadata management.
//!
//! ## Features
//!
//! - **Directory Listing**: Fast, parallel directory listing with metadata
//! - **Folder Size Calculation**: Recursive directory size with caching
//! - **Custom Metadata**: Read/write `.explorie.json` files for custom fields
//! - **Archive Operations**: Create and extract ZIP/TAR archives
//! - **Platform Support**: Windows and macOS specific features (junctions, xattrs)
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use explorie_core::{list_dir, list_dir_with_sizes, dir_size};
//! use std::path::Path;
//!
//! // List directory contents
//! let entries = list_dir(Path::new("/path/to/dir")).unwrap();
//!
//! // List with folder sizes
//! let entries = list_dir_with_sizes(Path::new("/path/to/dir"), true).unwrap();
//!
//! // Get total size of a directory
//! let size = dir_size(Path::new("/path/to/dir")).unwrap();
//! ```
//!
//! ## Custom Fields
//!
//! Files can have custom metadata stored in `.explorie.json`:
//!
//! ```json
//! {
//!   "document.pdf": {
//!     "status": "Done",
//!     "priority": "High",
//!     "tags": ["work", "important"]
//!   }
//! }
//! ```
//!
//! Use [`update_custom_fields`] to modify custom fields programmatically.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::SystemTime;

use rayon::prelude::*;
use uuid::Uuid;
use walkdir::WalkDir;

pub mod archive;
pub use archive::*;
mod file_operations;
pub use file_operations::*;
mod finder_tags;
pub use finder_tags::{
    FINDER_TAG_COLOR_COUNT, FinderTag, MAX_LISTED_FINDER_TAGS, read_finder_tags, write_finder_tags,
};
#[doc(hidden)]
pub mod fuzzing;

/// Represents a file or directory entry in explorie.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub id: Uuid,
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
    pub hidden: bool,
    pub is_dir: bool,
    pub custom: HashMap<String, serde_json::Value>, // from .explorie.json
    /// True if this entry is a symbolic link
    #[serde(default)]
    pub is_symlink: bool,
    /// True if this is a Windows junction point or reparse point
    #[serde(default)]
    pub is_junction: bool,
    /// The target path if this is a symlink or junction
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
    /// True if file has extended attributes (macOS xattrs, Windows ADS)
    #[serde(default)]
    pub has_xattrs: bool,
    /// True for macOS package directories (apps, bundles, Photos libraries,
    /// document packages) that Finder presents as a single item. `is_dir`
    /// stays true for them. Links whose target is a package report true as
    /// well (alongside `link_target_is_dir`).
    #[serde(default)]
    pub is_package: bool,
    /// True if this is a symlink or junction whose target resolves to a
    /// directory. Broken links report false.
    #[serde(default)]
    pub link_target_is_dir: bool,
    /// True if the file's contents live in the cloud and reading them would
    /// trigger a download (iCloud dataless files, OneDrive placeholders).
    #[serde(default)]
    pub is_cloud_placeholder: bool,
    /// Finder tags (macOS), read with the listing for items that have
    /// extended attributes. Empty elsewhere.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<FinderTag>,
}

/// A directory listing together with non-fatal problems met while producing it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DirListing {
    pub entries: Vec<FileEntry>,
    /// Human-readable descriptions of problems that did not stop the listing,
    /// such as an unreadable `.explorie.json`, items whose details could not
    /// be read, or folder sizes that are incomplete.
    #[serde(default)]
    pub warnings: Vec<String>,
}

const CUSTOM_FIELDS_CACHE_LIMIT: usize = 64;

type CustomFieldsDocument = HashMap<String, HashMap<String, serde_json::Value>>;

struct CustomFieldsCacheEntry {
    modified: Option<SystemTime>,
    fields: Arc<CustomFieldsDocument>,
}

struct CustomFieldsCache {
    entries: HashMap<PathBuf, CustomFieldsCacheEntry>,
    order: VecDeque<PathBuf>,
}

static CUSTOM_FIELDS_CACHE: OnceLock<RwLock<CustomFieldsCache>> = OnceLock::new();
// ponytail: one process-wide lock; use per-directory locks only after measured contention.
static CUSTOM_FIELDS_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn custom_fields_cache() -> &'static RwLock<CustomFieldsCache> {
    CUSTOM_FIELDS_CACHE.get_or_init(|| {
        RwLock::new(CustomFieldsCache {
            entries: HashMap::new(),
            order: VecDeque::new(),
        })
    })
}

fn custom_fields_write_lock() -> &'static Mutex<()> {
    CUSTOM_FIELDS_WRITE_LOCK.get_or_init(|| Mutex::new(()))
}

fn invalidate_custom_fields_cache(path: &Path) {
    let mut cache = custom_fields_cache()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.entries.remove(path);
    cache.order.retain(|cached| cached != path);
}

/// Load custom fields from .explorie.json file in a directory.
///
/// Invalid metadata is returned to the caller instead of being treated as an
/// empty schema; listings downgrade it to a warning, while writers refuse to
/// touch the file so the next update cannot erase it. The parsed document is
/// shared through the cache rather than cloned for every listing.
fn load_custom_fields(dir_path: &Path) -> io::Result<Arc<CustomFieldsDocument>> {
    let explorie_json_path = dir_path.join(".explorie.json");
    if !explorie_json_path.exists() {
        invalidate_custom_fields_cache(&explorie_json_path);
        return Ok(Arc::default());
    }

    let modified = fs::metadata(&explorie_json_path)
        .ok()
        .and_then(|metadata| metadata.modified().ok());

    if let Ok(cache) = custom_fields_cache().read()
        && let Some(entry) = cache.entries.get(&explorie_json_path)
        && modified.is_some()
        && entry.modified == modified
    {
        return Ok(Arc::clone(&entry.fields));
    }

    let content = fs::read_to_string(&explorie_json_path)?;
    let parsed: Arc<CustomFieldsDocument> = Arc::new(
        serde_json::from_str(&content)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
    );

    if let Ok(mut cache) = custom_fields_cache().write() {
        if cache.entries.len() >= CUSTOM_FIELDS_CACHE_LIMIT
            && let Some(oldest) = cache.order.pop_front()
        {
            cache.entries.remove(&oldest);
        }
        cache.order.retain(|path| path != &explorie_json_path);
        cache.order.push_back(explorie_json_path.clone());
        cache.entries.insert(
            explorie_json_path.clone(),
            CustomFieldsCacheEntry {
                modified,
                fields: Arc::clone(&parsed),
            },
        );
    }

    Ok(parsed)
}

/// Load custom fields for a listing, turning an unreadable or malformed
/// `.explorie.json` into "no custom fields" plus a warning.
fn load_custom_fields_lenient(
    dir_path: &Path,
    warnings: &mut Vec<String>,
) -> Arc<CustomFieldsDocument> {
    load_custom_fields(dir_path).unwrap_or_else(|error| {
        tracing::warn!(
            path = %dir_path.display(),
            %error,
            "ignoring unreadable custom metadata"
        );
        warnings.push(format!(
            "Custom fields from {} were ignored: {error}",
            dir_path.join(".explorie.json").display()
        ));
        Arc::default()
    })
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source: Vec<u16> = source
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

fn write_custom_fields_atomic(
    dir_path: &Path,
    fields: &HashMap<String, HashMap<String, serde_json::Value>>,
) -> io::Result<()> {
    let destination = dir_path.join(".explorie.json");
    let temporary = dir_path.join(format!(".explorie.{}.tmp", Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(fields)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        atomic_replace(&temporary, &destination)?;
        invalidate_custom_fields_cache(&destination);

        #[cfg(unix)]
        fs::File::open(dir_path)?.sync_all()?;

        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Check if a file has extended attributes (macOS xattrs, Windows ADS).
/// This is a best-effort check that returns false if unable to determine.
fn has_extended_attributes(_path: &Path, _metadata: &fs::Metadata) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        // Use listxattr to check if there are any xattrs
        let c_path = match CString::new(_path.as_os_str().as_bytes()) {
            Ok(p) => p,
            Err(_) => return false,
        };

        // listxattr returns the size of the buffer needed, or -1 on error
        // A size > 0 means there are extended attributes
        // XATTR_NOFOLLOW = 0x0001 - don't follow symlinks
        let size = unsafe {
            libc::listxattr(
                c_path.as_ptr(),
                std::ptr::null_mut(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        size > 0
    }

    #[cfg(windows)]
    {
        // On Windows, check for Alternate Data Streams by looking for ':'
        // in the path after the drive letter, or by attempting to enumerate streams.
        // For simplicity, we check if the file has the FILE_ATTRIBUTE_SPARSE_FILE
        // or has any named streams beyond the main $DATA stream.
        // A full implementation would use FindFirstStreamW/FindNextStreamW.

        use std::os::windows::fs::MetadataExt;

        // Check for sparse file attribute as a proxy (often used with ADS)
        const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;
        // This is a simplified check - true ADS detection requires Win32 API
        (_metadata.file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE) != 0
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        // On Linux, use listxattr
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_path = match CString::new(_path.as_os_str().as_bytes()) {
            Ok(p) => p,
            Err(_) => return false,
        };

        let size = unsafe { libc::llistxattr(c_path.as_ptr(), std::ptr::null_mut(), 0) };
        size > 0
    }
}

/// Calculate the total size of a directory or file in bytes.
///
/// For files, returns the file size. For directories, recursively calculates
/// the total size of all files within (excluding symlinks).
///
/// Uses parallel iteration via rayon for performance on large directories.
///
/// # Arguments
///
/// * `path` - Path to the file or directory
///
/// # Returns
///
/// Total size in bytes, or an IO error if the path cannot be read.
///
/// # Example
///
/// ```rust,no_run
/// use explorie_core::dir_size;
/// use std::path::Path;
///
/// let size = dir_size(Path::new("/home/user/Documents")).unwrap();
/// println!("Total size: {} bytes", size);
/// ```
pub fn dir_size(path: &Path) -> io::Result<u64> {
    let root_metadata = fs::symlink_metadata(path)?;
    if is_link_metadata(&root_metadata) {
        return Ok(0);
    }
    if root_metadata.is_file() {
        return Ok(root_metadata.len());
    }

    walk_dir_size(path, Err)
}

/// Sum the sizes of regular files below `path` without following links.
///
/// Every entry that cannot be read is handed to `on_error`, which either stops
/// the walk by returning the error or skips the entry by returning `Ok`.
fn walk_dir_size(
    path: &Path,
    mut on_error: impl FnMut(io::Error) -> io::Result<()>,
) -> io::Result<u64> {
    let mut total: u64 = 0;
    let mut entries = WalkDir::new(path).follow_links(false).into_iter();
    while let Some(entry) = entries.next() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let kind = error
                    .io_error()
                    .map_or(io::ErrorKind::Other, io::Error::kind);
                on_error(io::Error::new(kind, error))?;
                continue;
            }
        };
        let metadata = match entry.path().symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                on_error(error)?;
                continue;
            }
        };
        if is_link_metadata(&metadata) {
            if metadata.is_dir() {
                entries.skip_current_dir();
            }
        } else if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }

    Ok(total)
}

/// Folder size for listings: unreadable contents (for example `~/.Trash`
/// without Full Disk Access) are skipped instead of failing the listing.
/// Returns the size of everything readable and whether that total is complete.
fn dir_size_partial(path: &Path) -> (u64, bool) {
    let mut complete = true;
    let total = walk_dir_size(path, |error| {
        // Items deleted mid-walk no longer contribute to the folder's size.
        if error.kind() != io::ErrorKind::NotFound {
            complete = false;
        }
        Ok(())
    })
    .unwrap_or(0);
    (total, complete)
}

/// Return the recursive entry count and byte size without following links.
pub fn dir_info(path: &Path) -> io::Result<(u64, u64)> {
    let metadata = fs::symlink_metadata(path)?;
    if is_link_metadata(&metadata) || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory info requires a real directory",
        ));
    }
    fs::read_dir(path)?;

    let mut count = 0;
    let mut size = 0;
    let mut entries = WalkDir::new(path).follow_links(false).into_iter();
    let _ = entries.next();
    while let Some(entry) = entries.next() {
        let entry = entry.map_err(io::Error::other)?;
        count += 1;
        let metadata = entry.path().symlink_metadata()?;
        if is_link_metadata(&metadata) {
            if metadata.is_dir() {
                entries.skip_current_dir();
            }
        } else if metadata.is_file() {
            size += metadata.len();
        }
    }
    Ok((count, size))
}

/// Symbolic links, plus junctions (mount points) on Windows. There, std reads
/// the reparse tag and reports a link only for name-surrogate tags such as
/// IO_REPARSE_TAG_SYMLINK and IO_REPARSE_TAG_MOUNT_POINT. Other reparse points
/// (OneDrive and other cloud-file placeholders, deduplicated or WOF-compressed
/// files) are ordinary files and folders and must not be refused as links.
fn is_link_metadata(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// List all files and directories in the given path.
///
/// Returns a vector of [`FileEntry`] structs with metadata for each item.
/// Automatically loads custom fields from `.explorie.json` if present.
///
/// # Arguments
///
/// * `path` - Directory path to list
/// * `calc_dir_size` - If true, recursively calculate folder sizes (slower but more informative)
///
/// # Returns
///
/// Vector of file entries, or an IO error if the directory itself cannot be
/// read. Problems with individual entries never fail the listing; use
/// [`list_dir_with_warnings`] to learn about them.
///
/// # Features
///
/// - Parallel iteration for fast listing of large directories
/// - Detects symlinks and Windows junction points
/// - Checks for extended attributes (macOS xattrs)
/// - Loads custom metadata from `.explorie.json`
/// - Handles hidden files (dotfiles, the macOS hidden flag and the Windows
///   hidden attribute)
/// - Flags macOS packages and cloud placeholders
///
/// # Example
///
/// ```rust,no_run
/// use explorie_core::list_dir_with_sizes;
/// use std::path::Path;
///
/// // List without folder sizes (fast)
/// let entries = list_dir_with_sizes(Path::new("/path"), false).unwrap();
///
/// // List with folder sizes (slower, calculates sizes)
/// let entries = list_dir_with_sizes(Path::new("/path"), true).unwrap();
///
/// for entry in entries {
///     println!("{}: {} bytes", entry.path.display(), entry.size);
/// }
/// ```
pub fn list_dir_with_sizes(path: &Path, calc_dir_size: bool) -> io::Result<Vec<FileEntry>> {
    list_dir_with_warnings(path, calc_dir_size).map(|listing| listing.entries)
}

/// List a directory like [`list_dir_with_sizes`] and report non-fatal problems.
///
/// Only failing to read the directory itself is an error. Entries that vanish
/// while the listing runs are skipped; entries whose metadata cannot be read
/// are still listed with what the directory knows about them; a malformed or
/// unreadable `.explorie.json` means "no custom fields"; and folders whose
/// contents cannot all be read get the size of their readable part. Each of
/// these problems is described in [`DirListing::warnings`].
pub fn list_dir_with_warnings(path: &Path, calc_dir_size: bool) -> io::Result<DirListing> {
    let mut unnamed_failures = 0;
    let dir_entries: Vec<fs::DirEntry> = fs::read_dir(path)?
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(error) => {
                if error.kind() != io::ErrorKind::NotFound {
                    unnamed_failures += 1;
                }
                None
            }
        })
        .collect();
    let mut warnings = Vec::new();
    let custom_fields = load_custom_fields_lenient(path, &mut warnings);
    let mut listing = list_entries(dir_entries, &custom_fields, calc_dir_size, unnamed_failures);
    warnings.append(&mut listing.warnings);
    listing.warnings = warnings;
    Ok(listing)
}

/// Build the listing entry for one path without calculating folder sizes.
///
/// Custom fields come from the parent's `.explorie.json` when it is readable.
pub fn entry_for_path(path: &Path) -> io::Result<FileEntry> {
    let metadata = fs::symlink_metadata(path)?;
    let file_name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned();
    let custom = path
        .parent()
        .and_then(|parent| load_custom_fields(parent).ok())
        .and_then(|fields| fields.get(&file_name).cloned())
        .unwrap_or_default();
    Ok(build_file_entry(path.to_path_buf(), &file_name, &metadata, custom, false).0)
}

enum EntryIssue {
    Unreadable(String),
    SizeIncomplete(String),
}

fn list_entries(
    dir_entries: Vec<fs::DirEntry>,
    custom_fields: &CustomFieldsDocument,
    calc_dir_size: bool,
    unnamed_failures: usize,
) -> DirListing {
    let outcomes: Vec<(Option<FileEntry>, Option<EntryIssue>)> = dir_entries
        .into_par_iter()
        .map(|entry| listed_entry(&entry, custom_fields, calc_dir_size))
        .collect();

    let mut entries = Vec::with_capacity(outcomes.len());
    let mut unreadable = Vec::new();
    let mut incomplete = Vec::new();
    for (entry, issue) in outcomes {
        entries.extend(entry);
        match issue {
            Some(EntryIssue::Unreadable(name)) => unreadable.push(name),
            Some(EntryIssue::SizeIncomplete(name)) => incomplete.push(name),
            None => {}
        }
    }

    let mut warnings = Vec::new();
    let failures = unreadable.len() + unnamed_failures;
    if failures > 0 {
        let mut message = format!(
            "Details for {failures} item{} could not be read",
            plural(failures)
        );
        if !unreadable.is_empty() {
            message.push_str(": ");
            message.push_str(&describe_names(&mut unreadable));
        }
        warnings.push(message);
    }
    if !incomplete.is_empty() {
        warnings.push(format!(
            "Sizes are incomplete for {} folder{} with unreadable contents: {}",
            incomplete.len(),
            plural(incomplete.len()),
            describe_names(&mut incomplete)
        ));
    }

    DirListing { entries, warnings }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn describe_names(names: &mut [String]) -> String {
    const SHOWN: usize = 3;
    names.sort();
    let mut description = names
        .iter()
        .take(SHOWN)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        description.push_str(&format!(" and {} more", names.len() - SHOWN));
    }
    description
}

fn listed_entry(
    entry: &fs::DirEntry,
    custom_fields: &CustomFieldsDocument,
    calc_dir_size: bool,
) -> (Option<FileEntry>, Option<EntryIssue>) {
    let file_name = entry.file_name();
    let file_name = file_name.to_string_lossy();

    // Skip the .explorie.json file itself from listings
    if file_name == ".explorie.json" {
        return (None, None);
    }

    let path = entry.path();
    let custom = custom_fields
        .get(file_name.as_ref())
        .cloned()
        .unwrap_or_default();

    // Get symlink metadata (doesn't follow links)
    match path.symlink_metadata() {
        Ok(metadata) => {
            let (file_entry, size_complete) =
                build_file_entry(path, &file_name, &metadata, custom, calc_dir_size);
            let issue =
                (!size_complete).then(|| EntryIssue::SizeIncomplete(file_name.into_owned()));
            (Some(file_entry), issue)
        }
        // Deleted between read_dir and stat: it is simply gone.
        Err(error) if error.kind() == io::ErrorKind::NotFound => (None, None),
        Err(_) => {
            let file_type = match entry.file_type() {
                Ok(file_type) => Some(file_type),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return (None, None),
                Err(_) => None,
            };
            let file_entry = unreadable_file_entry(path, &file_name, file_type, custom);
            (
                Some(file_entry),
                Some(EntryIssue::Unreadable(file_name.into_owned())),
            )
        }
    }
}

fn entry_id(path: &Path) -> Uuid {
    let path_key = path.to_string_lossy().replace('\\', "/");
    Uuid::new_v5(&Uuid::NAMESPACE_URL, path_key.as_bytes())
}

/// Build an entry from link metadata. The second value is false when a
/// requested folder size is only the readable part of the folder.
fn build_file_entry(
    path_buf: PathBuf,
    file_name: &str,
    symlink_meta: &fs::Metadata,
    custom: HashMap<String, serde_json::Value>,
    calc_dir_size: bool,
) -> (FileEntry, bool) {
    let is_symlink = symlink_meta.file_type().is_symlink();

    // Check for Windows junction points / reparse points
    #[cfg(windows)]
    let is_junction = {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        (symlink_meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 && !is_symlink
    };
    #[cfg(not(windows))]
    let is_junction = false;

    // Get link target for symlinks and junctions
    let link_target = if is_symlink || is_junction {
        fs::read_link(&path_buf)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    } else {
        None
    };
    // Resolve the target only to classify it; broken links report false.
    let link_target_is_dir =
        (is_symlink || is_junction) && fs::metadata(&path_buf).is_ok_and(|target| target.is_dir());

    // Check for extended attributes
    let has_xattrs = has_extended_attributes(&path_buf, symlink_meta);

    // Use link metadata throughout so dangling links remain listable and
    // directory links are never traversed as real directories.
    let metadata = symlink_meta;
    let file_type = metadata.file_type();
    let is_dir = !is_symlink && !is_junction && file_type.is_dir();

    // Calculate size - don't follow symlinks/junctions for size calculation
    let mut size_complete = true;
    let size = if is_symlink || is_junction {
        // For links, just use the link's own size
        metadata.len()
    } else if file_type.is_file() {
        metadata.len()
    } else if is_dir && calc_dir_size {
        let (size, complete) = dir_size_partial(&path_buf);
        size_complete = complete;
        size
    } else {
        0
    };
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let is_package = if is_dir {
        is_package_dir(&path_buf, has_xattrs)
    } else {
        link_target_is_dir && link_target_is_package(&path_buf)
    };
    let tags = if has_xattrs {
        finder_tags::listed_finder_tags(&path_buf)
    } else {
        Vec::new()
    };

    let entry = FileEntry {
        id: entry_id(&path_buf),
        size,
        modified,
        hidden: is_dot_hidden(file_name) || has_hidden_flag(metadata),
        is_dir,
        custom,
        is_symlink,
        is_junction,
        link_target,
        has_xattrs,
        is_package,
        link_target_is_dir,
        is_cloud_placeholder: is_cloud_placeholder(metadata),
        tags,
        path: path_buf,
    };
    (entry, size_complete)
}

/// An entry whose metadata could not be read: keep it visible with what the
/// directory entry itself reports.
fn unreadable_file_entry(
    path: PathBuf,
    file_name: &str,
    file_type: Option<fs::FileType>,
    custom: HashMap<String, serde_json::Value>,
) -> FileEntry {
    let is_symlink = file_type.is_some_and(|file_type| file_type.is_symlink());
    let is_dir = file_type.is_some_and(|file_type| file_type.is_dir());
    FileEntry {
        id: entry_id(&path),
        size: 0,
        modified: SystemTime::UNIX_EPOCH,
        hidden: is_dot_hidden(file_name),
        is_dir,
        custom,
        is_symlink,
        is_junction: false,
        link_target: None,
        has_xattrs: false,
        is_package: is_dir && is_package_dir(&path, false),
        link_target_is_dir: false,
        is_cloud_placeholder: false,
        tags: Vec::new(),
        path,
    }
}

fn is_dot_hidden(file_name: &str) -> bool {
    file_name.starts_with('.') && file_name != "." && file_name != ".."
}

/// The platform's own "hidden" marker: the macOS `UF_HIDDEN` flag (set on
/// `~/Library`, `/usr`, ...) or the Windows hidden attribute.
fn has_hidden_flag(metadata: &fs::Metadata) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        metadata.st_flags() & libc::UF_HIDDEN != 0
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = metadata;
        false
    }
}

/// Whether reading this item's contents would first download it from a cloud
/// provider: macOS dataless files (evicted iCloud Drive or File Provider
/// items) and Windows cloud-files placeholders (OneDrive "online-only").
pub fn is_cloud_placeholder(metadata: &fs::Metadata) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        const SF_DATALESS: u32 = 0x4000_0000;
        metadata.st_flags() & SF_DATALESS != 0
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
        const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x4_0000;
        const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
        metadata.file_attributes()
            & (FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_RECALL_ON_OPEN
                | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
            != 0
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = metadata;
        false
    }
}

/// Directory extensions that macOS presents as single-item packages.
#[cfg(target_os = "macos")]
const PACKAGE_EXTENSIONS: &[&str] = &[
    // Applications, app extensions and plug-ins
    "aaxplugin",
    "app",
    "appex",
    "bundle",
    "component",
    "dext",
    "driverext",
    "framework",
    "kext",
    "mdimporter",
    "plugin",
    "prefpane",
    "qlgenerator",
    "saver",
    "service",
    "systemextension",
    "vst",
    "vst3",
    "wdgt",
    "xpc",
    // Media libraries
    "aplibrary",
    "fcpbundle",
    "imovielibrary",
    "migratedphotolibrary",
    "musiclibrary",
    "photoboothlibrary",
    "photolibrary",
    "photoslibrary",
    "theater",
    "tvlibrary",
    // Document packages
    "action",
    "band",
    "docset",
    "download",
    "dsym",
    "fcpxmld",
    "key",
    "logicx",
    "mlpackage",
    "mpkg",
    "numbers",
    "pages",
    "pkg",
    "playground",
    "rcproject",
    "rtfd",
    "scptd",
    "sparsebundle",
    "workflow",
    "xcappdata",
    "xcarchive",
    "xcodeproj",
    "xcworkspace",
];

/// Whether a real directory is a package. Known extensions are checked first;
/// otherwise the Finder "has bundle" bit in `com.apple.FinderInfo` decides,
/// which is only read when the directory has extended attributes at all.
fn is_package_dir(path: &Path, has_xattrs: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        let known_extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                PACKAGE_EXTENSIONS
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(extension))
            });
        known_extension || (has_xattrs && has_finder_bundle_bit(path))
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, has_xattrs);
        false
    }
}

/// Whether a link that resolves to a directory points at a package, so the
/// link opens like the package instead of being browsed as a folder.
fn link_target_is_package(link: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let has_package_extension = |path: &Path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    PACKAGE_EXTENSIONS
                        .iter()
                        .any(|known| known.eq_ignore_ascii_case(extension))
                })
        };
        if has_package_extension(link) {
            return true;
        }
        fs::canonicalize(link)
            .is_ok_and(|target| has_package_extension(&target) || has_finder_bundle_bit(&target))
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = link;
        false
    }
}

#[cfg(target_os = "macos")]
fn has_finder_bundle_bit(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // FolderInfo.finderFlags is a big-endian u16 at offset 8; kHasBundle = 0x2000.
    const K_HAS_BUNDLE: u16 = 0x2000;
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut finder_info = [0_u8; 32];
    // SAFETY: Both strings are NUL-terminated and the buffer length matches
    // the buffer passed; getxattr writes at most that many bytes.
    let size = unsafe {
        libc::getxattr(
            c_path.as_ptr(),
            c"com.apple.FinderInfo".as_ptr(),
            finder_info.as_mut_ptr().cast(),
            finder_info.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    size >= 10 && u16::from_be_bytes([finder_info[8], finder_info[9]]) & K_HAS_BUNDLE != 0
}

/// Re-read specific children of `dir` exactly as [`list_dir_with_sizes`]
/// lists them, without enumerating the rest of the directory.
///
/// Each result pairs the requested path with its fresh entry, or `None` when
/// the path no longer exists or is not listed (such as `.explorie.json`).
/// Any other error fails the whole batch, as it would fail a listing.
pub fn stat_entries(
    dir: &Path,
    paths: &[PathBuf],
    calc_dir_size: bool,
) -> io::Result<Vec<(PathBuf, Option<FileEntry>)>> {
    let custom_fields = load_custom_fields(dir)?;
    paths
        .par_iter()
        .map(|path| {
            let file_name = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy();
            if file_name == ".explorie.json" {
                return Ok((path.clone(), None));
            }
            let metadata = match path.symlink_metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok((path.clone(), None));
                }
                Err(error) => return Err(error),
            };
            let custom = custom_fields
                .get(file_name.as_ref())
                .cloned()
                .unwrap_or_default();
            let (entry, _) =
                build_file_entry(path.clone(), &file_name, &metadata, custom, calc_dir_size);
            Ok((path.clone(), Some(entry)))
        })
        .collect()
}

/// List directory contents without calculating folder sizes.
///
/// This is a convenience wrapper around [`list_dir_with_sizes`] with
/// `calc_dir_size` set to false. Faster for large directories when
/// folder sizes aren't needed.
///
/// # Example
///
/// ```rust,no_run
/// use explorie_core::list_dir;
/// use std::path::Path;
///
/// let entries = list_dir(Path::new("/path/to/dir")).unwrap();
/// ```
pub fn list_dir(path: &Path) -> io::Result<Vec<FileEntry>> {
    list_dir_with_sizes(path, false)
}

const EXPLORIE_SCHEMA_ENTRY: &str = "$schema";
const STATUS_VALUES: &[&str] = &["Todo", "In Progress", "Done", "Blocked", "Pending Review"];
const PRIORITY_VALUES: &[&str] = &["Low", "Medium", "High", "Urgent"];
const TYPE_VALUES: &[&str] = &["Document", "Image", "Video", "Code", "Data", "Archive"];
const CATEGORY_VALUES: &[&str] = &["Work", "Personal", "Project", "Reference", "Template"];

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
            if leap { 29 } else { 28 }
        }
        _ => 0,
    }
}

fn is_iso_date(value: &str) -> bool {
    if value.len() != 10 {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    if !bytes.iter().enumerate().all(|(index, byte)| {
        if index == 4 || index == 7 {
            true
        } else {
            byte.is_ascii_digit()
        }
    }) {
        return false;
    }
    let Ok(year) = value[0..4].parse::<i32>() else {
        return false;
    };
    let Ok(month) = value[5..7].parse::<u32>() else {
        return false;
    };
    let Ok(day) = value[8..10].parse::<u32>() else {
        return false;
    };
    (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month)
}

fn is_http_url(value: &str) -> bool {
    let rest = match value.split_once("://") {
        Some((scheme, rest))
            if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") =>
        {
            rest
        }
        _ => return false,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return false;
    }
    let hostport = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    if hostport.starts_with('[') {
        return hostport.contains(']') && hostport.len() > 2;
    }
    let host = match hostport.rsplit_once(':') {
        Some((host, port)) => {
            if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
                return false;
            }
            host
        }
        None => hostport,
    };
    !host.is_empty()
        && !host.starts_with('.')
        && !host.ends_with('.')
        && !host.contains("..")
        && host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-'))
}

fn typed_field_error(field: &str, message: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("Invalid custom field \"{field}\": {message}"),
    )
}

fn require_enum(name: &str, value: &serde_json::Value, allowed: &[&str]) -> io::Result<()> {
    match value.as_str() {
        Some(candidate)
            if allowed
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(candidate)) =>
        {
            Ok(())
        }
        _ => Err(typed_field_error(
            name,
            &format!("expected one of {}", allowed.join(", ")),
        )),
    }
}

fn validate_typed_field(name: &str, value: &serde_json::Value) -> io::Result<()> {
    match name {
        "dueDate" => match value.as_str() {
            Some(candidate) if is_iso_date(candidate) => Ok(()),
            _ => Err(typed_field_error(name, "expected ISO date (YYYY-MM-DD)")),
        },
        "url" => match value.as_str() {
            Some(candidate) if is_http_url(candidate) => Ok(()),
            _ => Err(typed_field_error(name, "expected http or https URL")),
        },
        "status" => require_enum(name, value, STATUS_VALUES),
        "priority" => require_enum(name, value, PRIORITY_VALUES),
        "type" => require_enum(name, value, TYPE_VALUES),
        "category" => require_enum(name, value, CATEGORY_VALUES),
        _ => Ok(()),
    }
}

fn validate_explorie_fields(
    file_name: &str,
    fields: &HashMap<String, serde_json::Value>,
) -> io::Result<()> {
    if file_name == EXPLORIE_SCHEMA_ENTRY {
        return Ok(());
    }
    for (name, value) in fields {
        validate_typed_field(name, value)?;
    }
    Ok(())
}

fn validate_explorie_document(
    document: &HashMap<String, HashMap<String, serde_json::Value>>,
) -> io::Result<()> {
    for (file_name, fields) in document {
        validate_explorie_fields(file_name, fields)?;
    }
    Ok(())
}

fn read_explorie_document(
    dir_path: &Path,
) -> io::Result<HashMap<String, HashMap<String, serde_json::Value>>> {
    let explorie_json_path = dir_path.join(".explorie.json");
    if !explorie_json_path.exists() {
        return Ok(HashMap::new());
    }
    let content = fs::read_to_string(&explorie_json_path)?;
    serde_json::from_str(&content).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid {}: {error}", explorie_json_path.display()),
        )
    })
}

/// Create or overwrite a `.explorie.json` file with custom field definitions.
///
/// This replaces the entire contents of the `.explorie.json` file. To update
/// individual file fields, use [`update_custom_fields`] instead.
///
/// A malformed existing file is reported and left untouched, including when
/// `fields` is empty. Date, URL, and closed enum values are checked before write.
///
/// # Arguments
///
/// * `dir_path` - Directory where `.explorie.json` will be created
/// * `fields` - Map of filename -> field map
///
/// # Example
///
/// ```rust,no_run
/// use explorie_core::create_explorie_schema;
/// use std::collections::HashMap;
/// use std::path::Path;
/// use serde_json::json;
///
/// let mut fields = HashMap::new();
/// let mut file_fields = HashMap::new();
/// file_fields.insert("status".to_string(), json!("Done"));
/// fields.insert("document.pdf".to_string(), file_fields);
///
/// create_explorie_schema(Path::new("/path/to/dir"), fields).unwrap();
/// ```
pub fn create_explorie_schema(
    dir_path: &Path,
    fields: HashMap<String, HashMap<String, serde_json::Value>>,
) -> io::Result<()> {
    let _guard = custom_fields_write_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    read_explorie_document(dir_path)?;
    validate_explorie_document(&fields)?;
    write_custom_fields_atomic(dir_path, &fields)
}

/// Update custom fields for a specific file in `.explorie.json`.
///
/// Merges the provided fields into the existing `.explorie.json` file,
/// creating the file if it doesn't exist. Only the specified file's
/// fields are modified; other entries are preserved.
///
/// # Arguments
///
/// * `dir_path` - Directory containing the `.explorie.json` file
/// * `file_name` - Name of the file to update fields for (not full path)
/// * `custom_fields` - Map of field names to values
///
/// # Example
///
/// ```rust,no_run
/// use explorie_core::update_custom_fields;
/// use std::collections::HashMap;
/// use std::path::Path;
/// use serde_json::json;
///
/// let mut fields = HashMap::new();
/// fields.insert("status".to_string(), json!("In Progress"));
/// fields.insert("priority".to_string(), json!("High"));
/// fields.insert("tags".to_string(), json!(["important", "work"]));
///
/// update_custom_fields(
///     Path::new("/path/to/dir"),
///     "document.pdf",
///     fields
/// ).unwrap();
/// ```
pub fn update_custom_fields(
    dir_path: &Path,
    file_name: &str,
    custom_fields: HashMap<String, serde_json::Value>,
) -> io::Result<()> {
    let _guard = custom_fields_write_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let mut schema = read_explorie_document(dir_path)?;
    validate_explorie_fields(file_name, &custom_fields)?;
    schema.insert(file_name.to_string(), custom_fields);

    write_custom_fields_atomic(dir_path, &schema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn serialize_file_entry() {
        let entry = FileEntry {
            id: Uuid::new_v4(),
            path: PathBuf::from("/tmp/foo.txt"),
            size: 1234,
            modified: SystemTime::now(),
            hidden: false,
            is_dir: false,
            custom: {
                let mut map = HashMap::new();
                map.insert("tag".to_string(), json!("important"));
                map
            },
            is_symlink: false,
            is_junction: false,
            link_target: None,
            has_xattrs: false,
            is_package: false,
            link_target_is_dir: false,
            is_cloud_placeholder: false,
            tags: Vec::new(),
        };
        let _ = serde_json::to_string(&entry).unwrap();
    }

    #[test]
    fn serialize_file_entry_symlink() {
        let entry = FileEntry {
            id: Uuid::new_v4(),
            path: PathBuf::from("/tmp/link"),
            size: 0,
            modified: SystemTime::now(),
            hidden: false,
            is_dir: false,
            custom: HashMap::new(),
            is_symlink: true,
            is_junction: false,
            link_target: Some("/tmp/target".to_string()),
            has_xattrs: false,
            is_package: false,
            link_target_is_dir: false,
            is_cloud_placeholder: false,
            tags: Vec::new(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("is_symlink"));
        assert!(json.contains("link_target"));
    }

    #[test]
    fn stat_entries_match_the_listing_and_report_missing_paths() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        fs::write(root.join("note.txt"), "hello").unwrap();
        fs::write(root.join(".hidden"), "secret").unwrap();
        fs::create_dir(root.join("folder")).unwrap();
        fs::write(root.join("folder").join("nested.bin"), vec![0_u8; 64]).unwrap();
        fs::write(
            root.join(".explorie.json"),
            r#"{"note.txt": {"status": "Done"}}"#,
        )
        .unwrap();

        let listed = list_dir_with_sizes(root, true).unwrap();
        let requested: Vec<PathBuf> =
            ["note.txt", ".hidden", "folder", "missing", ".explorie.json"]
                .into_iter()
                .map(|name| root.join(name))
                .collect();
        let stats = stat_entries(root, &requested, true).unwrap();
        assert_eq!(stats.len(), requested.len());
        for (path, entry) in &stats {
            let listed = listed.iter().find(|listed| &listed.path == path);
            match (entry, listed) {
                (Some(entry), Some(listed)) => {
                    assert_eq!(
                        serde_json::to_value(entry).unwrap(),
                        serde_json::to_value(listed).unwrap()
                    );
                }
                (None, None) => {}
                other => panic!("{} differs from the listing: {other:?}", path.display()),
            }
        }
        let folder = stats[2].1.as_ref().unwrap();
        assert!(folder.is_dir);
        assert_eq!(folder.size, 64);
        assert_eq!(stats[0].1.as_ref().unwrap().custom["status"], "Done");
        assert!(stats[1].1.as_ref().unwrap().hidden);
        assert!(stats[3].1.is_none() && stats[4].1.is_none());
    }

    #[test]
    fn test_list_dir_current() {
        let result = list_dir(Path::new("."));
        assert!(result.is_ok());
    }

    #[test]
    fn test_dir_size_deeply_nested() {
        let temp_dir = TempDir::new().unwrap();
        let mut current = temp_dir.path().to_path_buf();
        let mut total: u64 = 0;

        for i in 0..6 {
            current = current.join(format!("level_{i}"));
            fs::create_dir(&current).unwrap();
            let file_path = current.join("data.bin");
            let payload = vec![i as u8; 128 + i as usize];
            fs::write(&file_path, &payload).unwrap();
            total += payload.len() as u64;
        }

        let size = dir_size(temp_dir.path()).unwrap();
        assert_eq!(size, total);
    }

    #[cfg(unix)]
    #[test]
    fn recursive_inspection_reports_unreadable_entries() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let allowed_dir = root.join("allowed");
        fs::create_dir(&allowed_dir).unwrap();
        let allowed_file = allowed_dir.join("ok.txt");
        fs::write(&allowed_file, b"ok").unwrap();

        let blocked_dir = root.join("blocked");
        fs::create_dir(&blocked_dir).unwrap();
        let blocked_file = blocked_dir.join("secret.txt");
        fs::write(&blocked_file, b"secret").unwrap();

        let mut perms = fs::metadata(&blocked_dir).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&blocked_dir, perms).unwrap();

        // Elevated test runners can still read mode-000 directories.
        if fs::read_dir(&blocked_dir).is_ok() {
            let mut restore = fs::metadata(&blocked_dir).unwrap().permissions();
            restore.set_mode(0o755);
            fs::set_permissions(&blocked_dir, restore).unwrap();
            return;
        }

        let size_result = dir_size(root);
        let info_result = dir_info(root);
        let listing_result = list_dir_with_warnings(root, true);

        let mut restore = fs::metadata(&blocked_dir).unwrap().permissions();
        restore.set_mode(0o755);
        fs::set_permissions(&blocked_dir, restore).unwrap();

        // Explicit size and info requests still report the unreadable folder...
        assert!(size_result.is_err());
        assert!(info_result.is_err());
        // ...but a listing with folder sizes shows it with a partial size.
        let listing = listing_result.unwrap();
        let size_of = |name: &str| {
            listing
                .entries
                .iter()
                .find(|entry| entry.path.ends_with(name))
                .map(|entry| entry.size)
        };
        assert_eq!(size_of("allowed"), Some(2));
        assert_eq!(size_of("blocked"), Some(0));
        assert_eq!(listing.warnings.len(), 1, "{:?}", listing.warnings);
        assert!(listing.warnings[0].contains("blocked"));
        assert!(listing.warnings[0].contains("incomplete"));
    }

    #[test]
    fn entries_deleted_during_a_listing_are_skipped() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("kept.txt"), b"kept").unwrap();
        fs::write(temp_dir.path().join("gone.txt"), b"gone").unwrap();
        let dir_entries: Vec<_> = fs::read_dir(temp_dir.path())
            .unwrap()
            .map(Result::unwrap)
            .collect();
        fs::remove_file(temp_dir.path().join("gone.txt")).unwrap();

        let listing = list_entries(dir_entries, &CustomFieldsDocument::new(), true, 0);
        assert_eq!(listing.entries.len(), 1);
        assert!(listing.entries[0].path.ends_with("kept.txt"));
        assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);
    }

    #[test]
    fn unnamed_directory_read_failures_are_counted_in_warnings() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("kept.txt"), b"kept").unwrap();
        let dir_entries: Vec<_> = fs::read_dir(temp_dir.path())
            .unwrap()
            .map(Result::unwrap)
            .collect();

        let listing = list_entries(dir_entries, &CustomFieldsDocument::new(), false, 2);
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(
            listing.warnings,
            vec!["Details for 2 items could not be read".to_string()]
        );
    }

    #[test]
    fn listings_share_the_cached_custom_fields_document() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join(".explorie.json"),
            r#"{"notes.txt": {"status": "Done"}}"#,
        )
        .unwrap();

        let first = load_custom_fields(temp_dir.path()).unwrap();
        let second = load_custom_fields(temp_dir.path()).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first["notes.txt"]["status"], json!("Done"));
    }

    #[test]
    fn warning_name_lists_are_sorted_and_bounded() {
        let mut names = ["d", "b", "a", "c", "e"].map(String::from);
        assert_eq!(describe_names(&mut names), "a, b, c and 2 more");
        let mut single = [String::from("only")];
        assert_eq!(describe_names(&mut single), "only");
    }

    #[cfg(unix)]
    #[test]
    fn test_dir_size_symlink_cycle() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let data_dir = root.join("data");
        fs::create_dir(&data_dir).unwrap();

        let link_path = data_dir.join("loop");
        symlink(root, &link_path).unwrap();

        let file_path = data_dir.join("file.txt");
        fs::write(&file_path, b"abc").unwrap();

        let size = dir_size(root).unwrap();
        assert_eq!(size, 3);
    }
}
