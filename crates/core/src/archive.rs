use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;
use walkdir::{DirEntry, WalkDir};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

// Re-export Password for 7z operations
pub use sevenz_rust::Password as SevenZPassword;

pub(crate) mod sevenzip;

#[derive(Clone, Copy, Debug)]
pub struct ExtractionLimits {
    pub max_entries: usize,
    pub max_entry_bytes: u64,
    pub max_total_bytes: u64,
    pub max_expansion_ratio: u64,
    /// Maximum bytes that may be written based on free space at extraction start.
    /// This remains in force when the user explicitly opts into extended limits.
    pub max_available_bytes: u64,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_entry_bytes: 1024 * 1024 * 1024,
            max_total_bytes: 8 * 1024 * 1024 * 1024,
            max_expansion_ratio: 10_000,
            max_available_bytes: u64::MAX,
        }
    }
}

impl ExtractionLimits {
    /// Higher limits used only after an explicit user confirmation.
    pub fn extended() -> Self {
        Self {
            max_entries: 1_000_000,
            max_entry_bytes: 8 * 1024 * 1024 * 1024,
            max_total_bytes: 32 * 1024 * 1024 * 1024,
            max_expansion_ratio: 100_000,
            max_available_bytes: u64::MAX,
        }
    }
}

struct ExtractionBudget<'a> {
    cancelled: &'a AtomicBool,
    limits: ExtractionLimits,
    effective_total_limit: u64,
    available_total_limit: u64,
    entries: usize,
    total_bytes: u64,
    entry_bytes: u64,
}

impl<'a> ExtractionBudget<'a> {
    fn new(
        archive_path: &Path,
        limits: ExtractionLimits,
        cancelled: &'a AtomicBool,
    ) -> io::Result<Self> {
        let compressed_bytes = fs::metadata(archive_path)?.len().max(1);
        let ratio_limit = compressed_bytes
            .saturating_mul(limits.max_expansion_ratio)
            .max(64 * 1024 * 1024);
        Ok(Self {
            cancelled,
            limits,
            effective_total_limit: limits.max_total_bytes.min(ratio_limit),
            available_total_limit: limits.max_available_bytes,
            entries: 0,
            total_bytes: 0,
            entry_bytes: 0,
        })
    }

    fn begin_entry(&mut self, declared_size: u64) -> io::Result<()> {
        self.check_cancelled()?;
        self.entries = self.entries.saturating_add(1);
        self.entry_bytes = 0;
        if self.entries > self.limits.max_entries {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Archive extraction exceeds the safety limit of {} entries",
                    self.limits.max_entries
                ),
            ));
        }
        if self.total_bytes.saturating_add(declared_size) > self.available_total_limit {
            return Err(insufficient_extraction_space());
        }
        if declared_size > self.limits.max_entry_bytes
            || self.total_bytes.saturating_add(declared_size) > self.effective_total_limit
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Archive extraction exceeds the configured safety limit",
            ));
        }
        Ok(())
    }

    fn add_bytes(&mut self, bytes: u64) -> io::Result<()> {
        self.check_cancelled()?;
        self.entry_bytes = self.entry_bytes.saturating_add(bytes);
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        if self.total_bytes > self.available_total_limit {
            return Err(insufficient_extraction_space());
        }
        if self.entry_bytes > self.limits.max_entry_bytes
            || self.total_bytes > self.effective_total_limit
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Archive extraction exceeded the configured safety limit",
            ));
        }
        Ok(())
    }

    fn check_cancelled(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "archive extraction cancelled",
            ))
        } else {
            Ok(())
        }
    }
}

fn insufficient_extraction_space() -> io::Error {
    io::Error::new(
        io::ErrorKind::StorageFull,
        "Archive extraction would consume the disk space reserved for safe operation",
    )
}

fn copy_with_extraction_budget<R: Read + ?Sized, W: Write + ?Sized>(
    reader: &mut R,
    writer: &mut W,
    budget: &mut ExtractionBudget<'_>,
) -> io::Result<u64> {
    let mut buffer = [0_u8; 128 * 1024];
    let mut written = 0_u64;
    loop {
        budget.check_cancelled()?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(written);
        }
        budget.add_bytes(count as u64)?;
        writer.write_all(&buffer[..count])?;
        written = written.saturating_add(count as u64);
    }
}

/// Supported archive formats
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveFormat {
    Zip,
    TarGz,
    Tar,
    Rar,
    SevenZ,
}

pub(crate) fn validate_archive_entry_path(entry_path: &Path) -> io::Result<()> {
    let mut saw_normal_component = false;
    for component in entry_path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid archive: unsafe path component detected",
                ));
            }
            Component::CurDir => continue,
            Component::Normal(name) => {
                saw_normal_component = true;
                let name = name.to_string_lossy();
                if name.contains('\0') {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Invalid archive: null byte in path",
                    ));
                }
                if cfg!(windows) {
                    let trimmed = name.trim_end_matches([' ', '.']);
                    if trimmed.is_empty() || trimmed != name {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Invalid archive: unsafe Windows path component",
                        ));
                    }
                    let upper = trimmed.to_ascii_uppercase();
                    if name.chars().any(|character| {
                        character.is_control()
                            || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
                    }) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Invalid archive: unsafe Windows filename",
                        ));
                    }
                    let base = upper.split('.').next().unwrap_or("");
                    if matches!(base, "CON" | "PRN" | "AUX" | "NUL") {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Invalid archive: reserved Windows filename",
                        ));
                    }
                    if let Some(num) = base
                        .strip_prefix("COM")
                        .or_else(|| base.strip_prefix("LPT"))
                        && matches!(num, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Invalid archive: reserved Windows filename",
                        ));
                    }
                }
            }
        }
    }
    if saw_normal_component {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid archive: empty entry path",
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArchiveSourceKind {
    File,
    Directory,
}

/// Links and junctions only (see `is_link_metadata`): cloud placeholders and
/// deduplicated files are reparse points too, but ordinary files and folders.
fn metadata_is_link_or_reparse(metadata: &Metadata) -> bool {
    super::is_link_metadata(metadata)
}

fn link_or_reparse_error(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "Archive operations do not follow symbolic links or junctions: {}",
            path.display()
        ),
    )
}

fn is_trusted_system_path_alias(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let expected = match path.to_str() {
            Some("/var") => Path::new("/private/var"),
            Some("/tmp") => Path::new("/private/tmp"),
            Some("/etc") => Path::new("/private/etc"),
            _ => return false,
        };
        fs::canonicalize(path).is_ok_and(|resolved| resolved == expected)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

fn ensure_no_link_ancestors(path: &Path) -> io::Result<()> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut ancestors: Vec<&Path> = absolute.ancestors().collect();
    ancestors.reverse();

    for ancestor in ancestors {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata)
                if metadata_is_link_or_reparse(&metadata)
                    && !is_trusted_system_path_alias(ancestor) =>
            {
                return Err(link_or_reparse_error(ancestor));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn classify_source(path: &Path) -> io::Result<ArchiveSourceKind> {
    ensure_no_link_ancestors(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata_is_link_or_reparse(&metadata) {
        return Err(link_or_reparse_error(path));
    }
    if metadata.is_file() {
        Ok(ArchiveSourceKind::File)
    } else if metadata.is_dir() {
        Ok(ArchiveSourceKind::Directory)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Unsupported archive source type: {}", path.display()),
        ))
    }
}

fn classify_walk_entry(entry: &DirEntry) -> io::Result<ArchiveSourceKind> {
    let path = entry.path();
    let metadata = fs::symlink_metadata(path)?;
    if entry.file_type().is_symlink() || metadata_is_link_or_reparse(&metadata) {
        return Err(link_or_reparse_error(path));
    }
    if entry.file_type().is_file() {
        Ok(ArchiveSourceKind::File)
    } else if entry.file_type().is_dir() {
        Ok(ArchiveSourceKind::Directory)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Unsupported archive source type: {}", path.display()),
        ))
    }
}

fn open_regular_file_without_links(path: &Path) -> io::Result<File> {
    ensure_no_link_ancestors(path)?;
    let mut options = OpenOptions::new();
    options.read(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }

    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if metadata_is_link_or_reparse(&metadata) {
        return Err(link_or_reparse_error(path));
    }
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Archive source is not a regular file: {}", path.display()),
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn file_has_multiple_hard_links(path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;

    Ok(fs::symlink_metadata(path)?.nlink() > 1)
}

#[cfg(windows)]
fn file_has_multiple_hard_links(path: &Path) -> io::Result<bool> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let file = open_regular_file_without_links(path)?;
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    let result =
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { information.assume_init() }.nNumberOfLinks > 1)
    }
}

#[cfg(not(any(unix, windows)))]
fn file_has_multiple_hard_links(_path: &Path) -> io::Result<bool> {
    Ok(false)
}

fn archive_source_name(path: &Path) -> io::Result<String> {
    path.file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Archive source has no filename: {}", path.display()),
            )
        })
}

fn ensure_output_outside_sources(sources: &[PathBuf], output_path: &Path) -> io::Result<()> {
    let (output, _) = resolve_sibling_target(output_path, false)?;
    for source in sources {
        let kind = classify_source(source)?;
        let source = fs::canonicalize(source)?;
        if output == source || (kind == ArchiveSourceKind::Directory && output.starts_with(&source))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Archive output must not be one of its sources or inside a source directory: {}",
                    output.display()
                ),
            ));
        }
    }
    Ok(())
}

fn resolve_sibling_target(path: &Path, create_parent: bool) -> io::Result<(PathBuf, PathBuf)> {
    let name = path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Destination has no filename: {}", path.display()),
            )
        })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    ensure_no_link_ancestors(parent)?;
    if create_parent {
        fs::create_dir_all(parent)?;
        ensure_no_link_ancestors(parent)?;
    }
    let parent = fs::canonicalize(parent)?;
    ensure_no_link_ancestors(&parent)?;
    Ok((parent.join(name), parent))
}

fn temporary_sibling(parent: &Path, target: &Path, role: &str) -> io::Result<PathBuf> {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    for _ in 0..16 {
        let candidate = parent.join(format!(".{name}.explorie-{role}-{}.tmp", Uuid::new_v4()));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Could not allocate a unique archive staging path",
    ))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn with_atomic_archive_output<T>(
    output_path: &Path,
    build: impl FnOnce(File) -> io::Result<T>,
) -> io::Result<T> {
    let (target, parent) = resolve_sibling_target(output_path, false)?;
    ensure_no_link_ancestors(&target)?;
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        if metadata_is_link_or_reparse(&metadata) {
            return Err(link_or_reparse_error(&target));
        }
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Archive output is not a regular file: {}", target.display()),
            ));
        }
    }

    let temporary = temporary_sibling(&parent, &target, "create")?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        let value = build(file)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temporary)?
            .sync_all()?;
        ensure_no_link_ancestors(&target)?;
        super::atomic_replace(&temporary, &target)?;
        if let Err(error) = sync_directory(&parent) {
            tracing::warn!(path = %parent.display(), %error, "failed to sync archive output directory");
        }
        Ok(value)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn ensure_safe_extraction_path(root: &Path, entry_path: &Path) -> io::Result<PathBuf> {
    validate_archive_entry_path(entry_path)?;
    let destination = root.join(entry_path);
    if !destination.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid archive: path traversal attempt detected",
        ));
    }
    ensure_no_link_ancestors(&destination)?;
    Ok(destination)
}

fn validate_tree_without_links(root: &Path) -> io::Result<()> {
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        classify_walk_entry(&entry)?;
    }
    Ok(())
}

fn remove_staging_directory(path: &Path) {
    if let Err(error) = fs::remove_dir_all(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "failed to remove archive staging directory");
    }
}

/// Publishes a fully staged extraction as a new folder. `rename_noreplace`
/// also refuses an empty folder that a plain rename would silently replace.
fn commit_staged_directory(staging: &Path, output: &Path, parent: &Path) -> io::Result<()> {
    ensure_no_link_ancestors(output)?;
    super::rename_noreplace(staging, output).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "Extraction destination appeared during extraction: {}",
                    output.display()
                ),
            )
        } else {
            error
        }
    })?;
    if let Err(error) = sync_directory(parent) {
        tracing::warn!(path = %parent.display(), %error, "failed to sync extraction output directory");
    }
    Ok(())
}

/// Extracts into a staging folder and publishes it as `output_dir`, which
/// must not exist yet: merging into an existing folder would replace files
/// that happen to share names with archive entries.
fn with_staged_extraction<T>(
    output_dir: &Path,
    extract: impl FnOnce(&Path) -> io::Result<T>,
) -> io::Result<T> {
    let (output, parent) = resolve_sibling_target(output_dir, true)?;
    ensure_no_link_ancestors(&output)?;
    match fs::symlink_metadata(&output) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "Extraction destination already exists: {}",
                    output.display()
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let staging = temporary_sibling(&parent, &output, "extract")?;
    fs::create_dir(&staging)?;
    let result = (|| {
        let value = extract(&staging)?;
        validate_tree_without_links(&staging)?;
        // An archive holding one top-level folder and nothing else extracts
        // that folder's contents into the output, as Finder's Archive
        // Utility does, instead of nesting the folder inside it.
        match single_top_level_directory(&staging)? {
            Some(inner) => {
                commit_staged_directory(&inner, &output, &parent)?;
                if let Err(error) = fs::remove_dir(&staging) {
                    tracing::warn!(path = %staging.display(), %error, "failed to remove empty archive staging directory");
                }
            }
            None => commit_staged_directory(&staging, &output, &parent)?,
        }
        Ok(value)
    })();
    if result.is_err() {
        remove_staging_directory(&staging);
    }
    result
}

/// The only entry in `staging` when it is a real directory, or `None`.
fn single_top_level_directory(staging: &Path) -> io::Result<Option<PathBuf>> {
    let mut entries = fs::read_dir(staging)?;
    let Some(first) = entries.next().transpose()? else {
        return Ok(None);
    };
    if entries.next().transpose()?.is_some() {
        return Ok(None);
    }
    let path = first.path();
    let metadata = fs::symlink_metadata(&path)?;
    Ok((metadata.is_dir() && !metadata.file_type().is_symlink()).then_some(path))
}

impl ArchiveFormat {
    /// Get the file extension for this format
    pub fn extension(&self) -> &'static str {
        match self {
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::TarGz => "tar.gz",
            ArchiveFormat::Tar => "tar",
            ArchiveFormat::Rar => "rar",
            ArchiveFormat::SevenZ => "7z",
        }
    }

    /// Detect format from file extension
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_lowercase();
        if name.ends_with(".zip") {
            Some(ArchiveFormat::Zip)
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Some(ArchiveFormat::TarGz)
        } else if name.ends_with(".tar") {
            Some(ArchiveFormat::Tar)
        } else if name.ends_with(".rar") {
            Some(ArchiveFormat::Rar)
        } else if name.ends_with(".7z") {
            Some(ArchiveFormat::SevenZ)
        } else {
            None
        }
    }

    /// Check if format supports creation (not just extraction)
    pub fn supports_creation(&self) -> bool {
        match self {
            ArchiveFormat::Zip
            | ArchiveFormat::TarGz
            | ArchiveFormat::Tar
            | ArchiveFormat::SevenZ => true,
            ArchiveFormat::Rar => false, // RAR is extract-only
        }
    }
}

/// Compression level for archives
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompressionLevel {
    None,
    Fast,
    Normal,
    Best,
}

impl CompressionLevel {
    fn to_flate2_level(self) -> flate2::Compression {
        match self {
            CompressionLevel::None => flate2::Compression::none(),
            CompressionLevel::Fast => flate2::Compression::fast(),
            CompressionLevel::Normal => flate2::Compression::default(),
            CompressionLevel::Best => flate2::Compression::best(),
        }
    }
}

/// Information about an archive entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub compressed_size: u64,
    pub is_dir: bool,
}

/// Result of listing archive contents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveInfo {
    pub format: String,
    pub total_size: u64,
    pub compressed_size: u64,
    pub entry_count: usize,
    pub entries: Vec<ArchiveEntry>,
}

#[derive(Debug, Clone)]
pub struct ArchiveProgress {
    pub processed_bytes: u64,
    pub total_bytes: u64,
    pub current_path: String,
}

/// Options for creating an archive
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressOptions {
    pub format: ArchiveFormat,
    pub compression_level: CompressionLevel,
    pub password: Option<String>,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            format: ArchiveFormat::Zip,
            compression_level: CompressionLevel::Normal,
            password: None,
        }
    }
}

fn estimate_sources_total_bytes(sources: &[PathBuf]) -> io::Result<u64> {
    let mut total: u64 = 0;
    for source in sources {
        match classify_source(source)? {
            ArchiveSourceKind::File => {
                total = total
                    .saturating_add(open_regular_file_without_links(source)?.metadata()?.len());
            }
            ArchiveSourceKind::Directory => {
                for entry in WalkDir::new(source).follow_links(false) {
                    let entry = entry.map_err(io::Error::other)?;
                    if classify_walk_entry(&entry)? == ArchiveSourceKind::File {
                        total = total.saturating_add(
                            open_regular_file_without_links(entry.path())?
                                .metadata()?
                                .len(),
                        );
                    }
                }
            }
        }
    }
    Ok(total)
}

/// Create a ZIP archive from a list of files/directories
pub fn create_zip_archive(
    sources: &[PathBuf],
    output_path: &Path,
    compression_level: CompressionLevel,
) -> io::Result<u64> {
    ensure_output_outside_sources(sources, output_path)?;
    with_atomic_archive_output(output_path, |file| {
        let writer = BufWriter::new(file);
        let mut zip = ZipWriter::new(writer);
        let method = match compression_level {
            CompressionLevel::None => CompressionMethod::Stored,
            _ => CompressionMethod::Deflated,
        };
        let options: SimpleFileOptions = SimpleFileOptions::default()
            .compression_method(method)
            .unix_permissions(0o755);
        let mut total_bytes: u64 = 0;

        for source in sources {
            match classify_source(source)? {
                ArchiveSourceKind::Directory => {
                    total_bytes += add_directory_to_zip(&mut zip, source, source, options)?;
                }
                ArchiveSourceKind::File => {
                    total_bytes +=
                        add_file_to_zip(&mut zip, source, &archive_source_name(source)?, options)?;
                }
            }
        }

        let mut writer = zip.finish()?;
        writer.flush()?;
        Ok(total_bytes)
    })
}

pub fn create_zip_archive_with_progress(
    sources: &[PathBuf],
    output_path: &Path,
    compression_level: CompressionLevel,
    progress: &mut impl FnMut(&Path, u64),
) -> io::Result<u64> {
    ensure_output_outside_sources(sources, output_path)?;
    with_atomic_archive_output(output_path, |file| {
        let writer = BufWriter::new(file);
        let mut zip = ZipWriter::new(writer);
        let method = match compression_level {
            CompressionLevel::None => CompressionMethod::Stored,
            _ => CompressionMethod::Deflated,
        };
        let options = SimpleFileOptions::default()
            .compression_method(method)
            .unix_permissions(0o755);
        let mut total_bytes: u64 = 0;

        for source in sources {
            match classify_source(source)? {
                ArchiveSourceKind::Directory => {
                    total_bytes += add_directory_to_zip_with_progress(
                        &mut zip, source, source, options, progress,
                    )?;
                }
                ArchiveSourceKind::File => {
                    total_bytes += add_file_to_zip_with_progress(
                        &mut zip,
                        source,
                        &archive_source_name(source)?,
                        options,
                        progress,
                    )?;
                }
            }
        }

        let mut writer = zip.finish()?;
        writer.flush()?;
        Ok(total_bytes)
    })
}

fn add_directory_to_zip_with_progress<'a, W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    dir_path: &Path,
    base_path: &Path,
    options: zip::write::FileOptions<'a, ()>,
    progress: &mut impl FnMut(&Path, u64),
) -> io::Result<u64> {
    let mut total_bytes: u64 = 0;
    let base_name = base_path
        .file_name()
        .unwrap_or_default()
        .to_str()
        .unwrap_or("");

    for entry in WalkDir::new(dir_path).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        let kind = classify_walk_entry(&entry)?;
        let path = entry.path();
        let relative_path = path.strip_prefix(dir_path).unwrap_or(path);

        // Build the archive path with the base directory name
        let archive_path = if relative_path.as_os_str().is_empty() {
            base_name.to_string()
        } else {
            format!(
                "{}/{}",
                base_name,
                relative_path.to_string_lossy().replace('\\', "/")
            )
        };

        if kind == ArchiveSourceKind::Directory {
            // Add directory entry
            let dir_name = format!("{}/", archive_path);
            zip.add_directory(&dir_name, options)?;
        } else {
            total_bytes +=
                add_file_to_zip_with_progress(zip, path, &archive_path, options, progress)?;
        }
    }

    Ok(total_bytes)
}

fn add_file_to_zip_with_progress<'a, W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    file_path: &Path,
    archive_name: &str,
    options: zip::write::FileOptions<'a, ()>,
    progress: &mut impl FnMut(&Path, u64),
) -> io::Result<u64> {
    let size = write_file_to_zip(zip, file_path, archive_name, options)?;
    progress(file_path, size);
    Ok(size)
}

fn add_directory_to_zip<'a, W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    dir_path: &Path,
    base_path: &Path,
    options: zip::write::FileOptions<'a, ()>,
) -> io::Result<u64> {
    let mut total_bytes: u64 = 0;
    let base_name = base_path
        .file_name()
        .unwrap_or_default()
        .to_str()
        .unwrap_or("");

    for entry in WalkDir::new(dir_path).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        let kind = classify_walk_entry(&entry)?;
        let path = entry.path();
        let relative_path = path.strip_prefix(dir_path).unwrap_or(path);

        // Build the archive path with the base directory name
        let archive_path = if relative_path.as_os_str().is_empty() {
            base_name.to_string()
        } else {
            format!(
                "{}/{}",
                base_name,
                relative_path.to_string_lossy().replace('\\', "/")
            )
        };

        if kind == ArchiveSourceKind::Directory {
            // Add directory entry
            let dir_name = format!("{}/", archive_path);
            zip.add_directory(&dir_name, options)?;
        } else {
            total_bytes += add_file_to_zip(zip, path, &archive_path, options)?;
        }
    }

    Ok(total_bytes)
}

fn add_file_to_zip<'a, W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    file_path: &Path,
    archive_name: &str,
    options: zip::write::FileOptions<'a, ()>,
) -> io::Result<u64> {
    write_file_to_zip(zip, file_path, archive_name, options)
}

fn write_file_to_zip<'a, W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    file_path: &Path,
    archive_name: &str,
    options: zip::write::FileOptions<'a, ()>,
) -> io::Result<u64> {
    let mut file = open_regular_file_without_links(file_path)?;
    let metadata = file.metadata()?;
    let size = metadata.len();

    zip.start_file(archive_name, options)?;

    let mut buffer = vec![0u8; 65536]; // 64KB buffer
    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        zip.write_all(&buffer[..bytes_read])?;
    }

    Ok(size)
}

/// Create a TAR archive (optionally gzipped) from a list of files/directories
pub fn create_tar_archive(
    sources: &[PathBuf],
    output_path: &Path,
    gzip: bool,
    compression_level: CompressionLevel,
) -> io::Result<u64> {
    ensure_output_outside_sources(sources, output_path)?;
    with_atomic_archive_output(output_path, |file| {
        let mut total_bytes: u64 = 0;
        if gzip {
            let encoder = flate2::write::GzEncoder::new(
                BufWriter::new(file),
                compression_level.to_flate2_level(),
            );
            let mut archive = tar::Builder::new(encoder);
            for source in sources {
                match classify_source(source)? {
                    ArchiveSourceKind::Directory => {
                        total_bytes += add_directory_to_tar(&mut archive, source)?;
                    }
                    ArchiveSourceKind::File => {
                        total_bytes +=
                            add_file_to_tar(&mut archive, source, &archive_source_name(source)?)?;
                    }
                }
            }
            let encoder = archive.into_inner()?;
            let mut writer = encoder.finish()?;
            writer.flush()?;
        } else {
            let mut archive = tar::Builder::new(BufWriter::new(file));
            for source in sources {
                match classify_source(source)? {
                    ArchiveSourceKind::Directory => {
                        total_bytes += add_directory_to_tar(&mut archive, source)?;
                    }
                    ArchiveSourceKind::File => {
                        total_bytes +=
                            add_file_to_tar(&mut archive, source, &archive_source_name(source)?)?;
                    }
                }
            }
            let mut writer = archive.into_inner()?;
            writer.flush()?;
        }
        Ok(total_bytes)
    })
}

pub fn create_tar_archive_with_progress(
    sources: &[PathBuf],
    output_path: &Path,
    gzip: bool,
    compression_level: CompressionLevel,
    progress: &mut impl FnMut(&Path, u64),
) -> io::Result<u64> {
    ensure_output_outside_sources(sources, output_path)?;
    with_atomic_archive_output(output_path, |file| {
        let mut total_bytes: u64 = 0;
        if gzip {
            let encoder = flate2::write::GzEncoder::new(
                BufWriter::new(file),
                compression_level.to_flate2_level(),
            );
            let mut archive = tar::Builder::new(encoder);
            for source in sources {
                match classify_source(source)? {
                    ArchiveSourceKind::Directory => {
                        total_bytes +=
                            add_directory_to_tar_with_progress(&mut archive, source, progress)?;
                    }
                    ArchiveSourceKind::File => {
                        total_bytes += add_file_to_tar_with_progress(
                            &mut archive,
                            source,
                            &archive_source_name(source)?,
                            progress,
                        )?;
                    }
                }
            }
            let encoder = archive.into_inner()?;
            let mut writer = encoder.finish()?;
            writer.flush()?;
        } else {
            let mut archive = tar::Builder::new(BufWriter::new(file));
            for source in sources {
                match classify_source(source)? {
                    ArchiveSourceKind::Directory => {
                        total_bytes +=
                            add_directory_to_tar_with_progress(&mut archive, source, progress)?;
                    }
                    ArchiveSourceKind::File => {
                        total_bytes += add_file_to_tar_with_progress(
                            &mut archive,
                            source,
                            &archive_source_name(source)?,
                            progress,
                        )?;
                    }
                }
            }
            let mut writer = archive.into_inner()?;
            writer.flush()?;
        }
        Ok(total_bytes)
    })
}

fn add_directory_to_tar_with_progress<W: Write>(
    archive: &mut tar::Builder<W>,
    dir_path: &Path,
    progress: &mut impl FnMut(&Path, u64),
) -> io::Result<u64> {
    let mut total_bytes: u64 = 0;
    let _base_name = dir_path.file_name().unwrap_or_default();

    for entry in WalkDir::new(dir_path).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        let kind = classify_walk_entry(&entry)?;
        let path = entry.path();
        if kind == ArchiveSourceKind::File {
            let relative_path = path
                .strip_prefix(dir_path.parent().unwrap_or(dir_path))
                .unwrap_or(path);
            let archive_name = relative_path.to_string_lossy().replace('\\', "/");
            total_bytes += add_file_to_tar_with_progress(archive, path, &archive_name, progress)?;
        }
    }

    Ok(total_bytes)
}

fn add_file_to_tar_with_progress<W: Write>(
    archive: &mut tar::Builder<W>,
    file_path: &Path,
    archive_name: &str,
    progress: &mut impl FnMut(&Path, u64),
) -> io::Result<u64> {
    let size = add_file_to_tar(archive, file_path, archive_name)?;
    progress(file_path, size);
    Ok(size)
}

fn add_directory_to_tar<W: Write>(
    archive: &mut tar::Builder<W>,
    dir_path: &Path,
) -> io::Result<u64> {
    let mut total_bytes: u64 = 0;
    let _base_name = dir_path.file_name().unwrap_or_default();

    for entry in WalkDir::new(dir_path).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        let kind = classify_walk_entry(&entry)?;
        let path = entry.path();
        if kind == ArchiveSourceKind::File {
            let relative_path = path
                .strip_prefix(dir_path.parent().unwrap_or(dir_path))
                .unwrap_or(path);
            let archive_name = relative_path.to_string_lossy().replace('\\', "/");
            total_bytes += add_file_to_tar(archive, path, &archive_name)?;
        }
    }

    Ok(total_bytes)
}

fn add_file_to_tar<W: Write>(
    archive: &mut tar::Builder<W>,
    file_path: &Path,
    archive_name: &str,
) -> io::Result<u64> {
    let mut file = open_regular_file_without_links(file_path)?;
    let metadata = file.metadata()?;
    let size = metadata.len();

    let mut header = tar::Header::new_gnu();
    header.set_path(archive_name)?;
    header.set_size(size);
    header.set_mode(0o644);
    header.set_mtime(
        metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0),
    );
    header.set_cksum();

    archive.append(&header, &mut file)?;
    Ok(size)
}

/// Extract a ZIP archive to a directory
pub fn extract_zip_archive(archive_path: &Path, output_dir: &Path) -> io::Result<u64> {
    let cancelled = AtomicBool::new(false);
    let limits = ExtractionLimits::default();
    with_staged_extraction(output_dir, |staging| {
        let mut budget = ExtractionBudget::new(archive_path, limits, &cancelled)?;
        extract_zip_archive_into(archive_path, staging, None, &mut budget)
    })
}

fn extract_zip_archive_into(
    archive_path: &Path,
    output_dir: &Path,
    password: Option<&str>,
    budget: &mut ExtractionBudget<'_>,
) -> io::Result<u64> {
    let file = File::open(archive_path)?;
    let reader = BufReader::new(file);
    let mut archive = ZipArchive::new(reader)?;
    let mut total_bytes: u64 = 0;

    for i in 0..archive.len() {
        let mut file = if let Some(password) = password {
            archive.by_index_decrypt(i, password.as_bytes())?
        } else {
            archive.by_index(i)?
        };
        budget.begin_entry(file.size())?;
        let name = file.name().to_string();
        let entry_path = Path::new(&name);
        let out_path = ensure_safe_extraction_path(output_dir, entry_path)?;
        if file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid archive: symbolic link entry {name}"),
            ));
        }

        if file.is_dir() {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
                ensure_no_link_ancestors(parent)?;
            }
            let mut out_file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&out_path)?;
            total_bytes = total_bytes.saturating_add(copy_with_extraction_budget(
                &mut file,
                &mut out_file,
                budget,
            )?);
            out_file.flush()?;
        }
    }

    Ok(total_bytes)
}

/// Extract a TAR archive (optionally gzipped) to a directory
pub fn extract_tar_archive(archive_path: &Path, output_dir: &Path, gzip: bool) -> io::Result<u64> {
    let cancelled = AtomicBool::new(false);
    let limits = ExtractionLimits::default();
    with_staged_extraction(output_dir, |staging| {
        let mut budget = ExtractionBudget::new(archive_path, limits, &cancelled)?;
        extract_tar_archive_into(archive_path, staging, gzip, &mut budget)
    })
}

fn extract_tar_archive_into(
    archive_path: &Path,
    output_dir: &Path,
    gzip: bool,
    budget: &mut ExtractionBudget<'_>,
) -> io::Result<u64> {
    let file = File::open(archive_path)?;
    let total_bytes = if gzip {
        let decoder = flate2::read::GzDecoder::new(BufReader::new(file));
        let mut archive = tar::Archive::new(decoder);
        extract_tar_entries(&mut archive, output_dir, budget)?
    } else {
        let mut archive = tar::Archive::new(BufReader::new(file));
        extract_tar_entries(&mut archive, output_dir, budget)?
    };

    Ok(total_bytes)
}

fn extract_tar_entries<R: Read>(
    archive: &mut tar::Archive<R>,
    output_dir: &Path,
    budget: &mut ExtractionBudget<'_>,
) -> io::Result<u64> {
    let mut total_bytes: u64 = 0;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        budget.begin_entry(entry.size())?;
        validate_archive_entry_path(&path)?;
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Invalid archive: unsupported TAR entry type for {}",
                    path.display()
                ),
            ));
        }
        let output_path = ensure_safe_extraction_path(output_dir, &path)?;
        if entry_type.is_dir() {
            fs::create_dir_all(&output_path)?;
        } else {
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent)?;
                ensure_no_link_ancestors(parent)?;
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&output_path)?;
            copy_with_extraction_budget(&mut entry, &mut output, budget)?;
            output.flush()?;
            #[cfg(unix)]
            if let Ok(mode) = entry.header().mode() {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&output_path, fs::Permissions::from_mode(mode & 0o777))?;
            }
        }
        total_bytes = budget.total_bytes;
    }

    Ok(total_bytes)
}

/// List contents of a ZIP archive
pub fn list_zip_contents(archive_path: &Path) -> io::Result<ArchiveInfo> {
    let file = File::open(archive_path)?;
    let reader = BufReader::new(file);
    let mut archive = ZipArchive::new(reader)?;

    let mut entries = Vec::with_capacity(archive.len());
    let mut total_size: u64 = 0;
    let mut compressed_size: u64 = 0;

    for i in 0..archive.len() {
        let file = archive.by_index_raw(i)?;
        let name = file.name().to_string();
        let is_dir = name.ends_with('/');
        let size = file.size();
        let comp_size = file.compressed_size();

        total_size += size;
        compressed_size += comp_size;

        entries.push(ArchiveEntry {
            name: name.split('/').next_back().unwrap_or(&name).to_string(),
            path: name,
            size,
            compressed_size: comp_size,
            is_dir,
        });
    }

    Ok(ArchiveInfo {
        format: "zip".to_string(),
        total_size,
        compressed_size,
        entry_count: entries.len(),
        entries,
    })
}

/// List contents of a TAR archive (optionally gzipped)
pub fn list_tar_contents(archive_path: &Path, gzip: bool) -> io::Result<ArchiveInfo> {
    let file = File::open(archive_path)?;
    let mut entries = Vec::new();
    let mut total_size: u64 = 0;

    if gzip {
        let decoder = flate2::read::GzDecoder::new(BufReader::new(file));
        let mut archive = tar::Archive::new(decoder);
        for entry in archive.entries()? {
            let entry = entry?;
            let path = entry.path()?.to_string_lossy().to_string();
            let size = entry.size();
            let is_dir = entry.header().entry_type().is_dir();

            total_size += size;
            entries.push(ArchiveEntry {
                name: path.split('/').next_back().unwrap_or(&path).to_string(),
                path,
                size,
                compressed_size: size, // TAR doesn't compress individual entries
                is_dir,
            });
        }
    } else {
        let mut archive = tar::Archive::new(BufReader::new(file));
        for entry in archive.entries()? {
            let entry = entry?;
            let path = entry.path()?.to_string_lossy().to_string();
            let size = entry.size();
            let is_dir = entry.header().entry_type().is_dir();

            total_size += size;
            entries.push(ArchiveEntry {
                name: path.split('/').next_back().unwrap_or(&path).to_string(),
                path,
                size,
                compressed_size: size,
                is_dir,
            });
        }
    }

    let format = if gzip { "tar.gz" } else { "tar" };
    let compressed_size = fs::metadata(archive_path)?.len();

    Ok(ArchiveInfo {
        format: format.to_string(),
        total_size,
        compressed_size,
        entry_count: entries.len(),
        entries,
    })
}

/// Extract a RAR archive to a directory (extract only - RAR creation not supported)
pub fn extract_rar_archive(
    archive_path: &Path,
    output_dir: &Path,
    password: Option<&str>,
) -> io::Result<u64> {
    let cancelled = AtomicBool::new(false);
    let limits = ExtractionLimits::default();
    with_staged_extraction(output_dir, |staging| {
        let mut budget = ExtractionBudget::new(archive_path, limits, &cancelled)?;
        extract_rar_archive_into(archive_path, staging, password, &mut budget)
    })
}

fn extract_rar_archive_into(
    archive_path: &Path,
    output_dir: &Path,
    password: Option<&str>,
    budget: &mut ExtractionBudget<'_>,
) -> io::Result<u64> {
    #[cfg(any(target_os = "linux", target_os = "netbsd"))]
    let archive_name = CString::new(archive_path.as_os_str().as_encoded_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "RAR path contains a null byte")
    })?;
    #[cfg(any(target_os = "linux", target_os = "netbsd"))]
    let mut open_data =
        unrar_sys::OpenArchiveDataEx::new(archive_name.as_ptr(), unrar_sys::RAR_OM_EXTRACT);

    #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
    let archive_name = widestring::WideCString::from_os_str(archive_path).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "RAR path contains a null byte")
    })?;
    #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
    let mut open_data =
        unrar_sys::OpenArchiveDataEx::new(archive_name.as_ptr().cast(), unrar_sys::RAR_OM_EXTRACT);

    // The UnRAR C API fills this structure even though unrar_sys exposes a const pointer.
    #[allow(clippy::unnecessary_mut_passed)]
    let handle = unsafe { unrar_sys::RAROpenArchiveEx(&mut open_data) };
    if handle.is_null() || open_data.open_result != unrar_sys::ERAR_SUCCESS as u32 {
        return Err(rar_error("open", open_data.open_result as i32));
    }
    let handle = RarHandle(handle);
    let password = password
        .map(|password| {
            CString::new(password).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "RAR password contains a null byte",
                )
            })
        })
        .transpose()?;
    if let Some(password) = password.as_ref() {
        unsafe { unrar_sys::RARSetPassword(handle.0, password.as_ptr()) };
    }

    loop {
        budget.check_cancelled()?;
        let mut header = unrar_sys::HeaderDataEx::default();
        // The UnRAR C API fills this structure even though unrar_sys exposes a const pointer.
        #[allow(clippy::unnecessary_mut_passed)]
        let result = unsafe { unrar_sys::RARReadHeaderEx(handle.0, &mut header) };
        if result == unrar_sys::ERAR_END_ARCHIVE {
            break;
        }
        if result != unrar_sys::ERAR_SUCCESS {
            return Err(rar_error("read header", result));
        }

        let filename = unsafe {
            widestring::WideCString::from_ptr_truncate(
                header.filename_w.as_ptr().cast(),
                header.filename_w.len(),
            )
        };
        let filename = PathBuf::from(filename.to_os_string());
        let size = ((header.unp_size_high as u64) << 32) | header.unp_size as u64;
        budget.begin_entry(size)?;
        let is_directory = header.flags & unrar_sys::RHDF_DIRECTORY != 0;
        let attributes = header.file_attr;
        let unix_kind = attributes & 0o170000;
        const WINDOWS_REPARSE_ATTRIBUTE: u32 = 0x400;
        let is_link = header.redir_type != 0
            || unix_kind == 0o120000
            || attributes & WINDOWS_REPARSE_ATTRIBUTE != 0;
        if is_link {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Invalid archive: RAR link or reparse entry {}",
                    filename.display()
                ),
            ));
        }
        let entry_path = ensure_safe_extraction_path(output_dir, &filename)?;

        if is_directory {
            fs::create_dir_all(&entry_path)?;
            let result = unsafe {
                unrar_sys::RARProcessFileW(handle.0, unrar_sys::RAR_SKIP, ptr::null(), ptr::null())
            };
            if result != unrar_sys::ERAR_SUCCESS {
                return Err(rar_error("process directory", result));
            }
        } else {
            if let Some(parent) = entry_path.parent() {
                fs::create_dir_all(parent)?;
                ensure_no_link_ancestors(parent)?;
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&entry_path)?;
            let mut context = RarWriteContext {
                writer: &mut file,
                cancelled: budget.cancelled,
                entry_bytes: budget.entry_bytes,
                total_bytes: budget.total_bytes,
                max_entry_bytes: budget.limits.max_entry_bytes,
                max_total_bytes: budget.effective_total_limit,
                max_available_bytes: budget.available_total_limit,
                error: None,
            };
            unsafe {
                unrar_sys::RARSetCallback(
                    handle.0,
                    Some(rar_stream_callback),
                    (&mut context as *mut RarWriteContext) as unrar_sys::LPARAM,
                )
            };
            let result = unsafe {
                unrar_sys::RARProcessFileW(handle.0, unrar_sys::RAR_TEST, ptr::null(), ptr::null())
            };
            if let Some(error) = context.error.take() {
                return Err(error);
            }
            if result != unrar_sys::ERAR_SUCCESS {
                return Err(rar_error("extract entry", result));
            }
            budget.entry_bytes = context.entry_bytes;
            budget.total_bytes = context.total_bytes;
            file.flush()?;
            let metadata = fs::symlink_metadata(&entry_path)?;
            if metadata_is_link_or_reparse(&metadata)
                || !metadata.is_file()
                || file_has_multiple_hard_links(&entry_path)?
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Invalid archive: unsafe RAR output {}",
                        entry_path.display()
                    ),
                ));
            }
            if metadata.len() != budget.entry_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "RAR entry size changed while extracting {}",
                        filename.display()
                    ),
                ));
            }
        }
    }

    Ok(budget.total_bytes)
}

struct RarHandle(*const unrar_sys::Handle);

impl Drop for RarHandle {
    fn drop(&mut self) {
        unsafe { unrar_sys::RARCloseArchive(self.0) };
    }
}

struct RarWriteContext<'a> {
    writer: &'a mut File,
    cancelled: &'a AtomicBool,
    entry_bytes: u64,
    total_bytes: u64,
    max_entry_bytes: u64,
    max_total_bytes: u64,
    max_available_bytes: u64,
    error: Option<io::Error>,
}

extern "C" fn rar_stream_callback(
    message: unrar_sys::UINT,
    user_data: unrar_sys::LPARAM,
    data: unrar_sys::LPARAM,
    size: unrar_sys::LPARAM,
) -> i32 {
    if user_data == 0 {
        return 1;
    }
    if matches!(
        message,
        unrar_sys::UCM_NEEDPASSWORD | unrar_sys::UCM_NEEDPASSWORDW
    ) {
        let context = unsafe { &mut *(user_data as *mut RarWriteContext<'_>) };
        context.error = Some(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "RAR password is required or incorrect",
        ));
        return -1;
    }
    if message != unrar_sys::UCM_PROCESSDATA {
        return 1;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let context = unsafe { &mut *(user_data as *mut RarWriteContext<'_>) };
        if context.cancelled.load(Ordering::Acquire) {
            context.error = Some(io::Error::new(
                io::ErrorKind::Interrupted,
                "archive extraction cancelled",
            ));
            return -1;
        }
        if data == 0 || size < 0 {
            context.error = Some(io::Error::new(
                io::ErrorKind::InvalidData,
                "RAR returned an invalid data chunk",
            ));
            return -1;
        }
        let bytes = size as u64;
        let next_entry_bytes = context.entry_bytes.saturating_add(bytes);
        let next_total_bytes = context.total_bytes.saturating_add(bytes);
        if next_total_bytes > context.max_available_bytes {
            context.error = Some(insufficient_extraction_space());
            return -1;
        }
        if next_entry_bytes > context.max_entry_bytes || next_total_bytes > context.max_total_bytes
        {
            context.error = Some(io::Error::new(
                io::ErrorKind::InvalidData,
                "Archive extraction exceeded the configured safety limit",
            ));
            return -1;
        }
        let chunk = unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) };
        if let Err(error) = context.writer.write_all(chunk) {
            context.error = Some(error);
            return -1;
        }
        context.entry_bytes = next_entry_bytes;
        context.total_bytes = next_total_bytes;
        1
    }))
    .unwrap_or(-1)
}

fn rar_error(action: &str, code: i32) -> io::Error {
    let description = match code {
        unrar_sys::ERAR_NO_MEMORY => "not enough memory",
        unrar_sys::ERAR_BAD_DATA => "corrupt data or checksum mismatch",
        unrar_sys::ERAR_BAD_ARCHIVE | unrar_sys::ERAR_UNKNOWN_FORMAT => "invalid RAR archive",
        unrar_sys::ERAR_EOPEN => "could not open archive or volume",
        unrar_sys::ERAR_ECREATE | unrar_sys::ERAR_EWRITE => "could not write output",
        unrar_sys::ERAR_ECLOSE | unrar_sys::ERAR_EREAD => "archive I/O failed",
        unrar_sys::ERAR_MISSING_PASSWORD => "password required",
        unrar_sys::ERAR_BAD_PASSWORD => "incorrect password",
        _ => "unknown UnRAR error",
    };
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("Failed to {action} RAR: {description} (code {code})"),
    )
}

/// List contents of a RAR archive
pub fn list_rar_contents(archive_path: &Path) -> io::Result<ArchiveInfo> {
    let archive = unrar::Archive::new(archive_path)
        .open_for_listing()
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to open RAR: {:?}", e),
            )
        })?;

    let mut entries = Vec::new();
    let mut total_size: u64 = 0;

    for entry_result in archive {
        let entry = entry_result.map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to read RAR entry: {:?}", e),
            )
        })?;

        let path = entry.filename.to_string_lossy().to_string();
        let is_dir = entry.is_directory();
        let size = entry.unpacked_size;

        total_size += size;

        entries.push(ArchiveEntry {
            name: path
                .split(['/', '\\'])
                .next_back()
                .unwrap_or(&path)
                .to_string(),
            path,
            size,
            compressed_size: size, // RAR doesn't expose packed_size in FileHeader
            is_dir,
        });
    }

    // Get compressed size from file metadata
    let compressed_size = fs::metadata(archive_path)?.len();

    Ok(ArchiveInfo {
        format: "rar".to_string(),
        total_size,
        compressed_size,
        entry_count: entries.len(),
        entries,
    })
}

/// Create a 7Z archive from a list of files/directories
pub fn create_7z_archive(
    sources: &[PathBuf],
    output_path: &Path,
    password: Option<&str>,
) -> io::Result<u64> {
    ensure_output_outside_sources(sources, output_path)?;
    with_atomic_archive_output(output_path, |output_file| {
        let mut total_bytes: u64 = 0;
        let mut sz = sevenz_rust::ArchiveWriter::new(output_file)
            .map_err(|error| io::Error::other(format!("Failed to create 7z: {error:?}")))?;
        configure_7z_writer(&mut sz, password);

        for source in sources {
            match classify_source(source)? {
                ArchiveSourceKind::Directory => {
                    total_bytes += add_directory_to_7z(&mut sz, source)?;
                }
                ArchiveSourceKind::File => {
                    total_bytes += add_file_to_7z(&mut sz, source, &archive_source_name(source)?)?;
                }
            }
        }

        sz.finish()
            .map_err(|error| io::Error::other(format!("Failed to finish 7z: {error:?}")))?;
        Ok(total_bytes)
    })
}

pub fn create_7z_archive_with_progress(
    sources: &[PathBuf],
    output_path: &Path,
    password: Option<&str>,
    progress: &mut impl FnMut(&Path, u64),
) -> io::Result<u64> {
    ensure_output_outside_sources(sources, output_path)?;
    with_atomic_archive_output(output_path, |output_file| {
        let mut total_bytes: u64 = 0;
        let mut sz = sevenz_rust::ArchiveWriter::new(output_file)
            .map_err(|error| io::Error::other(format!("Failed to create 7z: {error:?}")))?;
        configure_7z_writer(&mut sz, password);

        for source in sources {
            match classify_source(source)? {
                ArchiveSourceKind::Directory => {
                    total_bytes += add_directory_to_7z_with_progress(&mut sz, source, progress)?;
                }
                ArchiveSourceKind::File => {
                    total_bytes += add_file_to_7z_with_progress(
                        &mut sz,
                        source,
                        &archive_source_name(source)?,
                        progress,
                    )?;
                }
            }
        }

        sz.finish()
            .map_err(|error| io::Error::other(format!("Failed to finish 7z: {error:?}")))?;
        Ok(total_bytes)
    })
}

fn configure_7z_writer<W: Write + io::Seek>(
    writer: &mut sevenz_rust::ArchiveWriter<W>,
    password: Option<&str>,
) {
    if let Some(password) = password.filter(|password| !password.is_empty()) {
        writer.set_content_methods(vec![
            sevenz_rust::encoder_options::AesEncoderOptions::new(SevenZPassword::from(password))
                .into(),
            sevenz_rust::EncoderMethod::LZMA2.into(),
        ]);
    } else {
        writer.set_content_methods(vec![sevenz_rust::EncoderMethod::LZMA2.into()]);
    }
}

fn add_directory_to_7z_with_progress<W: Write + io::Seek>(
    sz: &mut sevenz_rust::ArchiveWriter<W>,
    dir_path: &Path,
    progress: &mut impl FnMut(&Path, u64),
) -> io::Result<u64> {
    let mut total_bytes: u64 = 0;
    let _base_name = dir_path.file_name().unwrap_or_default();

    for entry in WalkDir::new(dir_path).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        let kind = classify_walk_entry(&entry)?;
        let path = entry.path();
        if kind == ArchiveSourceKind::File {
            let relative_path = path
                .strip_prefix(dir_path.parent().unwrap_or(dir_path))
                .unwrap_or(path);
            let archive_name = relative_path.to_string_lossy().replace('\\', "/");
            total_bytes += add_file_to_7z_with_progress(sz, path, &archive_name, progress)?;
        }
    }

    Ok(total_bytes)
}

fn add_file_to_7z_with_progress<W: Write + io::Seek>(
    sz: &mut sevenz_rust::ArchiveWriter<W>,
    file_path: &Path,
    archive_name: &str,
    progress: &mut impl FnMut(&Path, u64),
) -> io::Result<u64> {
    let size = add_file_to_7z(sz, file_path, archive_name)?;
    progress(file_path, size);
    Ok(size)
}

fn add_directory_to_7z<W: Write + io::Seek>(
    sz: &mut sevenz_rust::ArchiveWriter<W>,
    dir_path: &Path,
) -> io::Result<u64> {
    let mut total_bytes: u64 = 0;
    let _base_name = dir_path.file_name().unwrap_or_default();

    for entry in WalkDir::new(dir_path).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        let kind = classify_walk_entry(&entry)?;
        let path = entry.path();
        if kind == ArchiveSourceKind::File {
            let relative_path = path
                .strip_prefix(dir_path.parent().unwrap_or(dir_path))
                .unwrap_or(path);
            let archive_name = relative_path.to_string_lossy().replace('\\', "/");
            total_bytes += add_file_to_7z(sz, path, &archive_name)?;
        }
    }

    Ok(total_bytes)
}

fn add_file_to_7z<W: Write + io::Seek>(
    sz: &mut sevenz_rust::ArchiveWriter<W>,
    file_path: &Path,
    archive_name: &str,
) -> io::Result<u64> {
    let mut file = open_regular_file_without_links(file_path)?;
    let metadata = file.metadata()?;
    let size = metadata.len();

    let mut entry = sevenz_rust::ArchiveEntry::new_file(archive_name);
    entry.size = size;
    sz.push_archive_entry(entry, Some(&mut file))
        .map_err(|e| io::Error::other(format!("Failed to add file to 7z: {:?}", e)))?;

    Ok(size)
}

/// Extract a 7Z archive to a directory
pub fn extract_7z_archive(
    archive_path: &Path,
    output_dir: &Path,
    password: Option<&str>,
) -> io::Result<u64> {
    let cancelled = AtomicBool::new(false);
    let limits = ExtractionLimits::default();
    with_staged_extraction(output_dir, |staging| {
        let mut budget = ExtractionBudget::new(archive_path, limits, &cancelled)?;
        extract_7z_archive_into(archive_path, staging, password, &mut budget)
    })
}

fn extract_7z_archive_into(
    archive_path: &Path,
    output_dir: &Path,
    password: Option<&str>,
    budget: &mut ExtractionBudget<'_>,
) -> io::Result<u64> {
    let file = File::open(archive_path)?;
    let reader = BufReader::new(file);
    let password = password
        .map(SevenZPassword::from)
        .unwrap_or_else(SevenZPassword::empty);
    let mut archive = sevenz_rust::ArchiveReader::new(reader, password).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to open 7z: {error:?}"),
        )
    })?;
    let mut total_bytes: u64 = 0;

    archive
        .for_each_entries(|entry, reader| {
            budget
                .begin_entry(entry.size)
                .map_err(|error| sevenz_rust::Error::Io(error, "".into()))?;
            if entry.is_anti_item() {
                return Err(sevenz_rust::Error::Other(
                    format!("Unsupported 7z anti-item: {}", entry.name()).into(),
                ));
            }
            let entry_path = Path::new(entry.name());
            let output_path = ensure_safe_extraction_path(output_dir, entry_path)
                .map_err(|error| sevenz_rust::Error::Io(error, "".into()))?;
            if entry.is_directory() {
                fs::create_dir_all(&output_path)
                    .map_err(|error| sevenz_rust::Error::Io(error, "".into()))?;
            } else {
                if let Some(parent) = output_path.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|error| sevenz_rust::Error::Io(error, "".into()))?;
                    ensure_no_link_ancestors(parent)
                        .map_err(|error| sevenz_rust::Error::Io(error, "".into()))?;
                }
                let mut output = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&output_path)
                    .map_err(|error| sevenz_rust::Error::Io(error, "".into()))?;
                let written = copy_with_extraction_budget(reader, &mut output, budget)
                    .map_err(|error| sevenz_rust::Error::Io(error, "".into()))?;
                output
                    .flush()
                    .map_err(|error| sevenz_rust::Error::Io(error, "".into()))?;
                total_bytes = total_bytes.saturating_add(written);
            }
            Ok(true)
        })
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to extract 7z: {error:?}"),
            )
        })?;

    Ok(total_bytes)
}

/// List contents of a 7Z archive
pub fn list_7z_contents(archive_path: &Path) -> io::Result<ArchiveInfo> {
    let file = File::open(archive_path)?;
    let reader = BufReader::new(file);
    let archive =
        sevenz_rust::ArchiveReader::new(reader, sevenz_rust::Password::empty()).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to open 7z: {:?}", e),
            )
        })?;

    let mut entries = Vec::new();
    let mut total_size: u64 = 0;

    for entry in archive.archive().files.iter() {
        let path = entry.name().to_string();
        let is_dir = entry.is_directory();
        let size = entry.size();

        total_size += size;

        entries.push(ArchiveEntry {
            name: path
                .split(['/', '\\'])
                .next_back()
                .unwrap_or(&path)
                .to_string(),
            path,
            size,
            compressed_size: size, // 7z doesn't provide per-entry compressed size easily
            is_dir,
        });
    }

    let compressed_size = fs::metadata(archive_path)?.len();

    Ok(ArchiveInfo {
        format: "7z".to_string(),
        total_size,
        compressed_size,
        entry_count: entries.len(),
        entries,
    })
}

/// Detect archive format and list its contents
pub fn list_archive_contents(archive_path: &Path) -> io::Result<ArchiveInfo> {
    match ArchiveFormat::from_path(archive_path) {
        Some(ArchiveFormat::Zip) => list_zip_contents(archive_path),
        Some(ArchiveFormat::TarGz) => list_tar_contents(archive_path, true),
        Some(ArchiveFormat::Tar) => list_tar_contents(archive_path, false),
        Some(ArchiveFormat::Rar) => list_rar_contents(archive_path),
        Some(ArchiveFormat::SevenZ) => list_7z_contents(archive_path),
        None if sevenzip::supports(archive_path) => sevenzip::list(archive_path),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unsupported archive format",
        )),
    }
}

/// Detect archive format and extract it
pub fn extract_archive(archive_path: &Path, output_dir: &Path) -> io::Result<u64> {
    extract_archive_with_password(archive_path, output_dir, None)
}

/// Detect archive format and extract it with optional password
pub fn extract_archive_with_password(
    archive_path: &Path,
    output_dir: &Path,
    password: Option<&str>,
) -> io::Result<u64> {
    let cancelled = AtomicBool::new(false);
    extract_archive_with_password_and_limits(
        archive_path,
        output_dir,
        password,
        ExtractionLimits::default(),
        &cancelled,
    )
}

pub fn extract_archive_with_password_and_limits(
    archive_path: &Path,
    output_dir: &Path,
    password: Option<&str>,
    limits: ExtractionLimits,
    cancelled: &AtomicBool,
) -> io::Result<u64> {
    with_staged_extraction(output_dir, |staging| {
        let mut budget = ExtractionBudget::new(archive_path, limits, cancelled)?;
        match ArchiveFormat::from_path(archive_path) {
            Some(ArchiveFormat::Zip) => {
                extract_zip_archive_into(archive_path, staging, password, &mut budget)
            }
            Some(ArchiveFormat::TarGz) => {
                extract_tar_archive_into(archive_path, staging, true, &mut budget)
            }
            Some(ArchiveFormat::Tar) => {
                extract_tar_archive_into(archive_path, staging, false, &mut budget)
            }
            Some(ArchiveFormat::Rar) => {
                extract_rar_archive_into(archive_path, staging, password, &mut budget)
            }
            Some(ArchiveFormat::SevenZ) => {
                extract_7z_archive_into(archive_path, staging, password, &mut budget)
            }
            None if sevenzip::supports(archive_path) => {
                sevenzip::extract_into(archive_path, staging, password, &mut budget)
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unsupported archive format",
            )),
        }
    })
}

/// Extract a password-protected ZIP archive to a directory
pub fn extract_zip_archive_with_password(
    archive_path: &Path,
    output_dir: &Path,
    password: Option<&str>,
) -> io::Result<u64> {
    let cancelled = AtomicBool::new(false);
    let limits = ExtractionLimits::default();
    with_staged_extraction(output_dir, |staging| {
        let mut budget = ExtractionBudget::new(archive_path, limits, &cancelled)?;
        extract_zip_archive_into(archive_path, staging, password, &mut budget)
    })
}

/// Create an archive from sources with the specified format
pub fn create_archive(
    sources: &[PathBuf],
    output_path: &Path,
    options: &CompressOptions,
) -> io::Result<u64> {
    match options.format {
        ArchiveFormat::Zip => {
            if options.password.is_some() {
                create_zip_archive_with_password(
                    sources,
                    output_path,
                    options.compression_level,
                    options.password.as_deref(),
                )
            } else {
                create_zip_archive(sources, output_path, options.compression_level)
            }
        }
        ArchiveFormat::TarGz => {
            create_tar_archive(sources, output_path, true, options.compression_level)
        }
        ArchiveFormat::Tar => {
            create_tar_archive(sources, output_path, false, options.compression_level)
        }
        ArchiveFormat::Rar => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "RAR creation is not supported. RAR is extract-only.",
        )),
        ArchiveFormat::SevenZ => {
            create_7z_archive(sources, output_path, options.password.as_deref())
        }
    }
}

pub fn create_archive_with_progress(
    sources: &[PathBuf],
    output_path: &Path,
    options: &CompressOptions,
    mut on_progress: impl FnMut(ArchiveProgress),
) -> io::Result<u64> {
    let total_bytes = estimate_sources_total_bytes(sources)?;
    let mut processed_bytes: u64 = 0;

    let mut progress = |path: &Path, bytes: u64| {
        processed_bytes = processed_bytes.saturating_add(bytes);
        on_progress(ArchiveProgress {
            processed_bytes,
            total_bytes,
            current_path: path.to_string_lossy().to_string(),
        });
    };

    match options.format {
        ArchiveFormat::Zip => {
            if options.password.is_some() {
                create_zip_archive_with_password_and_progress(
                    sources,
                    output_path,
                    options.compression_level,
                    options.password.as_deref(),
                    &mut progress,
                )
            } else {
                create_zip_archive_with_progress(
                    sources,
                    output_path,
                    options.compression_level,
                    &mut progress,
                )
            }
        }
        ArchiveFormat::TarGz => create_tar_archive_with_progress(
            sources,
            output_path,
            true,
            options.compression_level,
            &mut progress,
        ),
        ArchiveFormat::Tar => create_tar_archive_with_progress(
            sources,
            output_path,
            false,
            options.compression_level,
            &mut progress,
        ),
        ArchiveFormat::Rar => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "RAR creation is not supported. RAR is extract-only.",
        )),
        ArchiveFormat::SevenZ => create_7z_archive_with_progress(
            sources,
            output_path,
            options.password.as_deref(),
            &mut progress,
        ),
    }
}

/// Create a password-protected ZIP archive from a list of files/directories
pub fn create_zip_archive_with_password(
    sources: &[PathBuf],
    output_path: &Path,
    compression_level: CompressionLevel,
    password: Option<&str>,
) -> io::Result<u64> {
    ensure_output_outside_sources(sources, output_path)?;
    with_atomic_archive_output(output_path, |file| {
        let writer = BufWriter::new(file);
        let mut zip = ZipWriter::new(writer);
        let method = match compression_level {
            CompressionLevel::None => CompressionMethod::Stored,
            _ => CompressionMethod::Deflated,
        };
        let password_owned = password.map(str::to_owned);
        let mut total_bytes: u64 = 0;

        for source in sources {
            match classify_source(source)? {
                ArchiveSourceKind::Directory => {
                    total_bytes += add_directory_to_zip_with_password(
                        &mut zip,
                        source,
                        source,
                        method,
                        password_owned.as_deref(),
                    )?;
                }
                ArchiveSourceKind::File => {
                    total_bytes += add_file_to_zip_with_password(
                        &mut zip,
                        source,
                        &archive_source_name(source)?,
                        method,
                        password_owned.as_deref(),
                    )?;
                }
            }
        }

        let mut writer = zip.finish()?;
        writer.flush()?;
        Ok(total_bytes)
    })
}

pub fn create_zip_archive_with_password_and_progress(
    sources: &[PathBuf],
    output_path: &Path,
    compression_level: CompressionLevel,
    password: Option<&str>,
    progress: &mut impl FnMut(&Path, u64),
) -> io::Result<u64> {
    ensure_output_outside_sources(sources, output_path)?;
    with_atomic_archive_output(output_path, |file| {
        let writer = BufWriter::new(file);
        let mut zip = ZipWriter::new(writer);
        let method = match compression_level {
            CompressionLevel::None => CompressionMethod::Stored,
            _ => CompressionMethod::Deflated,
        };
        let password_owned = password.map(str::to_owned);
        let mut total_bytes: u64 = 0;

        for source in sources {
            match classify_source(source)? {
                ArchiveSourceKind::Directory => {
                    total_bytes += add_directory_to_zip_with_password_and_progress(
                        &mut zip,
                        source,
                        source,
                        method,
                        password_owned.as_deref(),
                        progress,
                    )?;
                }
                ArchiveSourceKind::File => {
                    total_bytes += add_file_to_zip_with_password_and_progress(
                        &mut zip,
                        source,
                        &archive_source_name(source)?,
                        method,
                        password_owned.as_deref(),
                        progress,
                    )?;
                }
            }
        }

        let mut writer = zip.finish()?;
        writer.flush()?;
        Ok(total_bytes)
    })
}

fn add_directory_to_zip_with_password_and_progress<W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    dir_path: &Path,
    base_path: &Path,
    method: CompressionMethod,
    password: Option<&str>,
    progress: &mut impl FnMut(&Path, u64),
) -> io::Result<u64> {
    let mut total_bytes: u64 = 0;
    let base_name = base_path
        .file_name()
        .unwrap_or_default()
        .to_str()
        .unwrap_or("");

    for entry in WalkDir::new(dir_path).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        let kind = classify_walk_entry(&entry)?;
        let path = entry.path();
        let relative_path = path.strip_prefix(dir_path).unwrap_or(path);

        // Build the archive path with the base directory name
        let archive_path = if relative_path.as_os_str().is_empty() {
            base_name.to_string()
        } else {
            format!(
                "{}/{}",
                base_name,
                relative_path.to_string_lossy().replace('\\', "/")
            )
        };

        if kind == ArchiveSourceKind::Directory {
            // Add directory entry
            let dir_name = format!("{}/", archive_path);
            let options = SimpleFileOptions::default()
                .compression_method(method)
                .unix_permissions(0o755);
            zip.add_directory(&dir_name, options)?;
        } else {
            total_bytes += add_file_to_zip_with_password_and_progress(
                zip,
                path,
                &archive_path,
                method,
                password,
                progress,
            )?;
        }
    }

    Ok(total_bytes)
}

fn add_file_to_zip_with_password_and_progress<W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    file_path: &Path,
    archive_name: &str,
    method: CompressionMethod,
    password: Option<&str>,
    progress: &mut impl FnMut(&Path, u64),
) -> io::Result<u64> {
    let options = if let Some(pwd) = password {
        SimpleFileOptions::default()
            .compression_method(method)
            .unix_permissions(0o755)
            .with_aes_encryption(zip::AesMode::Aes256, pwd)
    } else {
        SimpleFileOptions::default()
            .compression_method(method)
            .unix_permissions(0o755)
    };

    let size = write_file_to_zip(zip, file_path, archive_name, options)?;
    progress(file_path, size);
    Ok(size)
}

fn add_directory_to_zip_with_password<W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    dir_path: &Path,
    base_path: &Path,
    method: CompressionMethod,
    password: Option<&str>,
) -> io::Result<u64> {
    let mut total_bytes: u64 = 0;
    let base_name = base_path
        .file_name()
        .unwrap_or_default()
        .to_str()
        .unwrap_or("");

    for entry in WalkDir::new(dir_path).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        let kind = classify_walk_entry(&entry)?;
        let path = entry.path();
        let relative_path = path.strip_prefix(dir_path).unwrap_or(path);

        // Build the archive path with the base directory name
        let archive_path = if relative_path.as_os_str().is_empty() {
            base_name.to_string()
        } else {
            format!(
                "{}/{}",
                base_name,
                relative_path.to_string_lossy().replace('\\', "/")
            )
        };

        if kind == ArchiveSourceKind::Directory {
            // Add directory entry
            let dir_name = format!("{}/", archive_path);
            let options = SimpleFileOptions::default()
                .compression_method(method)
                .unix_permissions(0o755);
            zip.add_directory(&dir_name, options)?;
        } else {
            total_bytes +=
                add_file_to_zip_with_password(zip, path, &archive_path, method, password)?;
        }
    }

    Ok(total_bytes)
}

fn add_file_to_zip_with_password<W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    file_path: &Path,
    archive_name: &str,
    method: CompressionMethod,
    password: Option<&str>,
) -> io::Result<u64> {
    let options = if let Some(pwd) = password {
        SimpleFileOptions::default()
            .compression_method(method)
            .unix_permissions(0o755)
            .with_aes_encryption(zip::AesMode::Aes256, pwd)
    } else {
        SimpleFileOptions::default()
            .compression_method(method)
            .unix_permissions(0o755)
    };

    write_file_to_zip(zip, file_path, archive_name, options)
}

/// Check if a file is a supported archive format
pub fn is_archive(path: &Path) -> bool {
    ArchiveFormat::from_path(path).is_some() || sevenzip::supports(path)
}

/// Check if an archive requires a password
pub fn archive_needs_password(archive_path: &Path) -> io::Result<bool> {
    match ArchiveFormat::from_path(archive_path) {
        Some(ArchiveFormat::Zip) => {
            let file = File::open(archive_path)?;
            let reader = BufReader::new(file);
            let mut archive = ZipArchive::new(reader)?;
            // Check if any file in the archive is encrypted
            for i in 0..archive.len() {
                if let Ok(file) = archive.by_index_raw(i)
                    && file.encrypted()
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Some(ArchiveFormat::Rar) => {
            // RAR archives - check via unrar
            let archive = unrar::Archive::new(archive_path)
                .open_for_listing()
                .map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Failed to open RAR: {:?}", e),
                    )
                })?;

            for entry in archive.flatten() {
                if entry.is_encrypted() {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Some(ArchiveFormat::SevenZ) => {
            // For 7z, try opening without password; if it fails, assume password needed
            let file = File::open(archive_path)?;
            let reader = BufReader::new(file);
            match sevenz_rust::ArchiveReader::new(reader, sevenz_rust::Password::empty()) {
                Ok(_) => Ok(false),
                Err(_) => Ok(true), // Assume password needed if can't open
            }
        }
        Some(ArchiveFormat::TarGz) | Some(ArchiveFormat::Tar) => {
            // TAR archives don't support encryption
            Ok(false)
        }
        None if sevenzip::supports(archive_path) => sevenzip::needs_password(archive_path),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unsupported archive format",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use tempfile::TempDir;

    const RAR_VERSION_FIXTURE: &[u8] = &[
        82, 97, 114, 33, 26, 7, 0, 207, 144, 115, 0, 0, 13, 0, 0, 0, 0, 0, 0, 0, 15, 12, 116, 32,
        128, 39, 0, 21, 0, 0, 0, 11, 0, 0, 0, 3, 69, 243, 125, 198, 164, 138, 7, 71, 29, 51, 7, 0,
        164, 129, 0, 0, 86, 69, 82, 83, 73, 79, 78, 12, 0, 143, 236, 138, 69, 204, 35, 200, 72, 8,
        131, 98, 254, 95, 221, 92, 83, 136, 240, 114, 196, 61, 123, 0, 64, 7, 0,
    ];

    fn assert_no_staging_paths(parent: &Path) {
        let staging_paths: Vec<_> = fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".explorie-") && name.ends_with(".tmp"))
            .collect();
        assert!(
            staging_paths.is_empty(),
            "leftover staging paths: {staging_paths:?}"
        );
    }

    #[test]
    fn test_archive_format_detection() {
        assert_eq!(
            ArchiveFormat::from_path(Path::new("test.zip")),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("test.tar.gz")),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("test.tgz")),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("test.tar")),
            Some(ArchiveFormat::Tar)
        );
        assert_eq!(ArchiveFormat::from_path(Path::new("test.txt")), None);
    }

    #[test]
    fn test_create_and_extract_zip() {
        let temp_dir = TempDir::new().unwrap();
        let source_dir = temp_dir.path().join("source");
        fs::create_dir(&source_dir).unwrap();

        // Create test file
        let test_file = source_dir.join("test.txt");
        let mut file = File::create(&test_file).unwrap();
        file.write_all(b"Hello, World!").unwrap();

        // Create archive
        let archive_path = temp_dir.path().join("test.zip");
        let sources = vec![source_dir.clone()];
        create_zip_archive(&sources, &archive_path, CompressionLevel::Normal).unwrap();

        // Extract archive
        let extract_dir = temp_dir.path().join("extracted");
        extract_zip_archive(&archive_path, &extract_dir).unwrap();

        // The archive's only top-level entry is the "source" folder, so its
        // contents land directly in the extraction folder, as in Finder's
        // Archive Utility, rather than in "extracted/source".
        let extracted_file = extract_dir.join("test.txt");
        assert!(extracted_file.exists());
        assert!(!extract_dir.join("source").exists());
        let content = fs::read_to_string(&extracted_file).unwrap();
        assert_eq!(content, "Hello, World!");
        assert_no_staging_paths(temp_dir.path());
    }

    #[test]
    fn several_top_level_entries_or_one_file_extract_as_they_are() {
        let temp_dir = TempDir::new().unwrap();
        let first = temp_dir.path().join("first.txt");
        let second = temp_dir.path().join("second");
        fs::write(&first, b"first").unwrap();
        fs::create_dir(&second).unwrap();
        fs::write(second.join("inside.txt"), b"inside").unwrap();

        let several = temp_dir.path().join("several.zip");
        create_zip_archive(&[first.clone(), second], &several, CompressionLevel::Normal).unwrap();
        let several_out = temp_dir.path().join("several");
        extract_archive(&several, &several_out).unwrap();
        assert_eq!(fs::read(several_out.join("first.txt")).unwrap(), b"first");
        assert_eq!(
            fs::read(several_out.join("second/inside.txt")).unwrap(),
            b"inside"
        );

        let single_file = temp_dir.path().join("single.zip");
        create_zip_archive(&[first], &single_file, CompressionLevel::Normal).unwrap();
        let single_out = temp_dir.path().join("single");
        extract_archive(&single_file, &single_out).unwrap();
        assert_eq!(fs::read(single_out.join("first.txt")).unwrap(), b"first");
        assert_no_staging_paths(temp_dir.path());
    }

    #[test]
    fn test_create_and_extract_zip_with_password() {
        let temp_dir = TempDir::new().unwrap();
        let source_dir = temp_dir.path().join("source");
        fs::create_dir(&source_dir).unwrap();

        let test_file = source_dir.join("secret.txt");
        let mut file = File::create(&test_file).unwrap();
        file.write_all(b"Top secret").unwrap();

        let archive_path = temp_dir.path().join("secret.zip");
        let sources = vec![source_dir.clone()];
        create_zip_archive_with_password(
            &sources,
            &archive_path,
            CompressionLevel::Normal,
            Some("password123"),
        )
        .unwrap();

        assert!(archive_needs_password(&archive_path).unwrap());

        let wrong_extract_dir = temp_dir.path().join("wrong_extract");
        let wrong_result =
            extract_zip_archive_with_password(&archive_path, &wrong_extract_dir, Some("wrong"));
        assert!(wrong_result.is_err());

        let extract_dir = temp_dir.path().join("extracted");
        extract_zip_archive_with_password(&archive_path, &extract_dir, Some("password123"))
            .unwrap();

        let extracted_file = extract_dir.join("secret.txt");
        let content = fs::read_to_string(&extracted_file).unwrap();
        assert_eq!(content, "Top secret");
    }

    #[test]
    fn seven_z_round_trip_supports_listing_and_passwords() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("hello.txt"), b"Hello from 7z").unwrap();

        let archive_path = temp.path().join("plain.7z");
        create_7z_archive(std::slice::from_ref(&source), &archive_path, None).unwrap();

        let info = list_7z_contents(&archive_path).unwrap();
        assert_eq!(info.format, "7z");
        assert_eq!(info.entry_count, 1);
        assert_eq!(info.entries[0].path, "source/hello.txt");
        assert_eq!(info.entries[0].size, 13);

        let extracted = temp.path().join("plain-extracted");
        extract_7z_archive(&archive_path, &extracted, None).unwrap();
        assert_eq!(
            fs::read(extracted.join("hello.txt")).unwrap(),
            b"Hello from 7z"
        );

        let protected_path = temp.path().join("protected.7z");
        create_7z_archive(
            std::slice::from_ref(&source),
            &protected_path,
            Some("password123"),
        )
        .unwrap();
        assert!(archive_needs_password(&protected_path).unwrap());

        let wrong = temp.path().join("wrong-password");
        assert!(extract_7z_archive(&protected_path, &wrong, Some("wrong")).is_err());
        assert!(!wrong.exists());

        let protected_extracted = temp.path().join("protected-extracted");
        extract_7z_archive(&protected_path, &protected_extracted, Some("password123")).unwrap();
        assert_eq!(
            fs::read(protected_extracted.join("hello.txt")).unwrap(),
            b"Hello from 7z"
        );
    }

    #[test]
    fn test_zip_large_file_handling() {
        let temp_dir = TempDir::new().unwrap();
        let source_dir = temp_dir.path().join("source");
        fs::create_dir(&source_dir).unwrap();

        let file_size = 2 * 1024 * 1024;
        let payload: Vec<u8> = (0..file_size).map(|i| (i % 251) as u8).collect();

        let large_file = source_dir.join("large.bin");
        let mut file = File::create(&large_file).unwrap();
        file.write_all(&payload).unwrap();

        let archive_path = temp_dir.path().join("large.zip");
        let sources = vec![source_dir.clone()];
        create_zip_archive(&sources, &archive_path, CompressionLevel::Normal).unwrap();

        let extract_dir = temp_dir.path().join("extracted");
        extract_zip_archive(&archive_path, &extract_dir).unwrap();

        let extracted_file = extract_dir.join("large.bin");
        let extracted_payload = fs::read(&extracted_file).unwrap();
        assert_eq!(extracted_payload.len(), payload.len());
        assert_eq!(extracted_payload, payload);
    }

    #[test]
    fn test_zip_unicode_filenames() {
        let temp_dir = TempDir::new().unwrap();
        let source_dir = temp_dir.path().join("source");
        fs::create_dir(&source_dir).unwrap();

        let file_name = "unicode_\u{00E9}_\u{03B1}.txt";
        let test_file = source_dir.join(file_name);
        let mut file = File::create(&test_file).unwrap();
        file.write_all(b"Unicode content").unwrap();

        let archive_path = temp_dir.path().join("unicode.zip");
        let sources = vec![source_dir.clone()];
        create_zip_archive(&sources, &archive_path, CompressionLevel::Normal).unwrap();

        let extract_dir = temp_dir.path().join("extracted");
        extract_zip_archive(&archive_path, &extract_dir).unwrap();

        let extracted_file = extract_dir.join(file_name);
        let content = fs::read_to_string(&extracted_file).unwrap();
        assert_eq!(content, "Unicode content");
    }

    #[cfg(unix)]
    #[test]
    fn test_zip_symlink_handling() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let source_dir = temp_dir.path().join("source");
        fs::create_dir(&source_dir).unwrap();

        let target_file = source_dir.join("target.txt");
        let mut file = File::create(&target_file).unwrap();
        file.write_all(b"Symlink target").unwrap();

        let link_path = source_dir.join("link.txt");
        symlink(&target_file, &link_path).unwrap();

        let archive_path = temp_dir.path().join("symlink.zip");
        let sources = vec![source_dir.clone()];
        fs::write(&archive_path, b"prior archive").unwrap();
        assert!(create_zip_archive(&sources, &archive_path, CompressionLevel::Normal).is_err());
        assert_eq!(fs::read(&archive_path).unwrap(), b"prior archive");
    }

    #[cfg(unix)]
    #[test]
    fn archive_source_through_user_symlinked_parent_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let actual = temp.path().join("actual");
        let alias = temp.path().join("alias");
        fs::create_dir(&actual).unwrap();
        fs::write(actual.join("input.txt"), b"input").unwrap();
        symlink(&actual, &alias).unwrap();

        let output = temp.path().join("output.zip");
        let result = create_zip_archive(
            &[alias.join("input.txt")],
            &output,
            CompressionLevel::Normal,
        );

        assert!(result.is_err());
        assert!(!output.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_root_owned_path_aliases_are_trusted() {
        for alias in ["/var", "/tmp", "/etc"] {
            ensure_no_link_ancestors(Path::new(alias)).unwrap();
        }
    }

    #[test]
    fn failed_archive_creation_preserves_every_existing_output() {
        let temp = TempDir::new().unwrap();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        fs::write(&first, b"first").unwrap();
        let sources = vec![first.clone(), second.clone()];
        let cases = [
            (ArchiveFormat::Zip, "zip", None),
            (ArchiveFormat::Zip, "protected.zip", Some("secret")),
            (ArchiveFormat::Tar, "tar", None),
            (ArchiveFormat::TarGz, "tar.gz", None),
            (ArchiveFormat::SevenZ, "7z", None),
        ];

        for (index, (format, extension, password)) in cases.into_iter().enumerate() {
            fs::write(&second, b"second").unwrap();
            let output = temp.path().join(format!("existing-{index}.{extension}"));
            fs::write(&output, b"prior archive").unwrap();
            let options = CompressOptions {
                format,
                compression_level: CompressionLevel::Normal,
                password: password.map(str::to_owned),
            };
            let mut removed_second = false;
            let result = create_archive_with_progress(&sources, &output, &options, |_| {
                if !removed_second {
                    fs::remove_file(&second).unwrap();
                    removed_second = true;
                }
            });

            assert!(removed_second, "case {index} never began writing");
            assert!(result.is_err(), "case {index} unexpectedly succeeded");
            assert_eq!(fs::read(&output).unwrap(), b"prior archive");
            assert_no_staging_paths(temp.path());
        }
    }

    #[test]
    fn archive_output_inside_source_is_rejected() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("input.txt"), b"input").unwrap();
        let output = source.join("nested.zip");

        let result = create_zip_archive(
            std::slice::from_ref(&source),
            &output,
            CompressionLevel::Normal,
        );
        assert!(result.is_err());
        assert!(!output.exists());
        assert_no_staging_paths(&source);
    }

    #[test]
    fn failed_zip_extraction_publishes_nothing() {
        let temp = TempDir::new().unwrap();
        let archive_path = temp.path().join("traversal.zip");
        let output = temp.path().join("output");

        let file = File::create(&archive_path).unwrap();
        let mut archive = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        archive.start_file("good.txt", options).unwrap();
        archive.write_all(b"good").unwrap();
        archive.start_file("../escape.txt", options).unwrap();
        archive.write_all(b"escaped").unwrap();
        archive.finish().unwrap();

        assert!(extract_zip_archive(&archive_path, &output).is_err());
        assert!(!output.exists());
        assert!(!temp.path().join("escape.txt").exists());
        assert_no_staging_paths(temp.path());
    }

    #[test]
    fn extraction_budget_failure_publishes_nothing() {
        let temp = TempDir::new().unwrap();
        let archive_path = temp.path().join("budget.zip");
        let output = temp.path().join("output");
        let file = File::create(&archive_path).unwrap();
        let mut archive = ZipWriter::new(file);
        archive
            .start_file("too-large.txt", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"more than four bytes").unwrap();
        archive.finish().unwrap();
        let cancelled = AtomicBool::new(false);

        let result = extract_archive_with_password_and_limits(
            &archive_path,
            &output,
            None,
            ExtractionLimits {
                max_entries: 10,
                max_entry_bytes: 4,
                max_total_bytes: 4,
                max_expansion_ratio: u64::MAX,
                max_available_bytes: u64::MAX,
            },
            &cancelled,
        );

        assert!(result.is_err());
        assert!(!output.exists());
        assert_no_staging_paths(temp.path());
    }

    #[test]
    fn free_space_budget_is_not_bypassed_by_extended_limits() {
        let temp = TempDir::new().unwrap();
        let archive_path = temp.path().join("space.zip");
        let output = temp.path().join("output");
        let file = File::create(&archive_path).unwrap();
        let mut archive = ZipWriter::new(file);
        archive
            .start_file("too-large.txt", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"more than four bytes").unwrap();
        archive.finish().unwrap();
        let cancelled = AtomicBool::new(false);
        let mut limits = ExtractionLimits::extended();
        limits.max_available_bytes = 4;

        let error = extract_archive_with_password_and_limits(
            &archive_path,
            &output,
            None,
            limits,
            &cancelled,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        assert!(!output.exists());
        assert_no_staging_paths(temp.path());
    }

    #[test]
    fn real_rar_fixture_extracts_and_enforces_budget_and_cancellation() {
        let temp = TempDir::new().unwrap();
        let archive_path = temp.path().join("version.rar");
        fs::write(&archive_path, RAR_VERSION_FIXTURE).unwrap();

        let output = temp.path().join("output");
        let cancelled = AtomicBool::new(false);
        extract_archive_with_password_and_limits(
            &archive_path,
            &output,
            None,
            ExtractionLimits::default(),
            &cancelled,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(output.join("VERSION")).unwrap(),
            "unrar-0.4.0"
        );

        let limited_output = temp.path().join("limited");
        let error = extract_archive_with_password_and_limits(
            &archive_path,
            &limited_output,
            None,
            ExtractionLimits {
                max_entries: 1,
                max_entry_bytes: 4,
                max_total_bytes: 4,
                max_expansion_ratio: u64::MAX,
                max_available_bytes: u64::MAX,
            },
            &cancelled,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!limited_output.exists());

        let cancelled_output = temp.path().join("cancelled");
        let cancelled = AtomicBool::new(true);
        let error = extract_archive_with_password_and_limits(
            &archive_path,
            &cancelled_output,
            None,
            ExtractionLimits::default(),
            &cancelled,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!cancelled_output.exists());
        assert_no_staging_paths(temp.path());
    }

    #[test]
    fn rar_stream_callback_stops_before_over_budget_data_is_written() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("output.bin");
        let mut file = File::create(&path).unwrap();
        let cancelled = AtomicBool::new(false);
        let chunk = b"five!";
        let mut context = RarWriteContext {
            writer: &mut file,
            cancelled: &cancelled,
            entry_bytes: 0,
            total_bytes: 0,
            max_entry_bytes: 4,
            max_total_bytes: 4,
            max_available_bytes: u64::MAX,
            error: None,
        };

        let result = rar_stream_callback(
            unrar_sys::UCM_PROCESSDATA,
            (&mut context as *mut RarWriteContext) as unrar_sys::LPARAM,
            chunk.as_ptr() as unrar_sys::LPARAM,
            chunk.len() as unrar_sys::LPARAM,
        );

        assert_eq!(result, -1);
        assert_eq!(context.entry_bytes, 0);
        assert_eq!(
            context.error.as_ref().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        drop(context);
        assert_eq!(fs::metadata(path).unwrap().len(), 0);
    }

    #[test]
    fn rar_stream_callback_observes_cancellation_before_writing() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("output.bin");
        let mut file = File::create(&path).unwrap();
        let cancelled = AtomicBool::new(true);
        let chunk = b"data";
        let mut context = RarWriteContext {
            writer: &mut file,
            cancelled: &cancelled,
            entry_bytes: 0,
            total_bytes: 0,
            max_entry_bytes: u64::MAX,
            max_total_bytes: u64::MAX,
            max_available_bytes: u64::MAX,
            error: None,
        };

        let result = rar_stream_callback(
            unrar_sys::UCM_PROCESSDATA,
            (&mut context as *mut RarWriteContext) as unrar_sys::LPARAM,
            chunk.as_ptr() as unrar_sys::LPARAM,
            chunk.len() as unrar_sys::LPARAM,
        );

        assert_eq!(result, -1);
        assert_eq!(
            context.error.as_ref().unwrap().kind(),
            io::ErrorKind::Interrupted
        );
        drop(context);
        assert_eq!(fs::metadata(path).unwrap().len(), 0);
    }

    #[test]
    fn cancelled_extraction_publishes_nothing() {
        let temp = TempDir::new().unwrap();
        let archive_path = temp.path().join("cancelled.zip");
        let output = temp.path().join("output");
        let file = File::create(&archive_path).unwrap();
        let mut archive = ZipWriter::new(file);
        archive
            .start_file("new.txt", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"new contents").unwrap();
        archive.finish().unwrap();
        let cancelled = AtomicBool::new(true);

        let error = extract_archive_with_password_and_limits(
            &archive_path,
            &output,
            None,
            ExtractionLimits::default(),
            &cancelled,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!output.exists());
        assert_no_staging_paths(temp.path());
    }

    fn single_file_zip(path: &Path, name: &str, contents: &[u8]) {
        let mut archive = ZipWriter::new(File::create(path).unwrap());
        archive
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(contents).unwrap();
        archive.finish().unwrap();
    }

    #[test]
    fn extraction_refuses_an_existing_destination_without_touching_it() {
        let temp = TempDir::new().unwrap();
        let archive_path = temp.path().join("bundle.zip");
        single_file_zip(&archive_path, "same.txt", b"new");
        let output = temp.path().join("output");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("same.txt"), b"old").unwrap();
        fs::write(output.join("unrelated.txt"), b"untouched").unwrap();
        let empty = temp.path().join("empty");
        fs::create_dir(&empty).unwrap();

        for destination in [&output, &empty] {
            let error = extract_archive(&archive_path, destination).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        }

        assert_eq!(fs::read(output.join("same.txt")).unwrap(), b"old");
        assert_eq!(
            fs::read(output.join("unrelated.txt")).unwrap(),
            b"untouched"
        );
        assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);
        assert_no_staging_paths(temp.path());
    }

    #[test]
    fn extraction_refuses_a_destination_that_appears_while_extracting() {
        let temp = TempDir::new().unwrap();
        let output = temp.path().join("output");

        let error = with_staged_extraction(&output, |staging| {
            fs::write(staging.join("extracted.txt"), b"new")?;
            fs::create_dir(&output)?;
            Ok(())
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
        assert_no_staging_paths(temp.path());
    }

    #[test]
    fn seven_z_traversal_publishes_nothing() {
        let temp = TempDir::new().unwrap();
        let archive_path = temp.path().join("traversal.7z");
        let output = temp.path().join("output");

        let file = File::create(&archive_path).unwrap();
        let mut archive = sevenz_rust::ArchiveWriter::new(file).unwrap();
        let mut entry = sevenz_rust::ArchiveEntry::new_file("../escape.txt");
        entry.size = 7;
        archive
            .push_archive_entry(entry, Some(Cursor::new(b"escaped")))
            .unwrap();
        archive.finish().unwrap();

        assert!(extract_7z_archive(&archive_path, &output, None).is_err());
        assert!(!output.exists());
        assert!(!temp.path().join("escape.txt").exists());
        assert_no_staging_paths(temp.path());
    }

    #[test]
    fn test_zip_corrupted_archive_handling() {
        let temp_dir = TempDir::new().unwrap();
        let archive_path = temp_dir.path().join("corrupt.zip");
        let mut file = File::create(&archive_path).unwrap();
        file.write_all(b"not a zip").unwrap();

        let extract_dir = temp_dir.path().join("extracted");
        let result = extract_zip_archive(&archive_path, &extract_dir);
        assert!(result.is_err());
    }

    #[test]
    fn tar_extraction_rejects_parent_path_entry() {
        let temp = TempDir::new().unwrap();
        let archive_path = temp.path().join("traversal.tar");
        let output = temp.path().join("output");
        let payload = b"escaped";

        let file = File::create(&archive_path).unwrap();
        let mut builder = tar::Builder::new(file);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_path("placeholder").unwrap();
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..13].copy_from_slice(b"../escape.txt");
        header.set_cksum();
        builder.append(&header, &payload[..]).unwrap();
        builder.finish().unwrap();

        assert!(extract_tar_archive(&archive_path, &output, false).is_err());
        assert!(!temp.path().join("escape.txt").exists());
        assert!(!output.exists());
        assert_no_staging_paths(temp.path());
    }

    #[cfg(unix)]
    #[test]
    fn tar_extraction_rejects_archive_symlink_breakout() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let archive_path = temp.path().join("malicious.tar");
        let outside = temp.path().join("outside");
        let output = temp.path().join("output");
        fs::create_dir(&outside).unwrap();

        let file = File::create(&archive_path).unwrap();
        let mut builder = tar::Builder::new(file);
        let mut link_header = tar::Header::new_gnu();
        link_header.set_entry_type(tar::EntryType::Symlink);
        link_header.set_size(0);
        link_header.set_mode(0o777);
        link_header.set_path("link").unwrap();
        link_header.set_link_name(&outside).unwrap();
        link_header.set_cksum();
        builder.append(&link_header, io::empty()).unwrap();

        let payload = b"escaped";
        let mut file_header = tar::Header::new_gnu();
        file_header.set_entry_type(tar::EntryType::Regular);
        file_header.set_size(payload.len() as u64);
        file_header.set_mode(0o644);
        file_header.set_path("link/escaped.txt").unwrap();
        file_header.set_cksum();
        builder.append(&file_header, &payload[..]).unwrap();
        builder.finish().unwrap();

        assert!(extract_tar_archive(&archive_path, &output, false).is_err());
        assert!(!outside.join("escaped.txt").exists());

        // An existing output folder, here holding a planted link, is never
        // written into.
        assert!(!output.exists());
        fs::create_dir(&output).unwrap();
        symlink(&outside, output.join("link")).unwrap();
        assert!(extract_tar_archive(&archive_path, &output, false).is_err());
        assert!(!outside.join("escaped.txt").exists());
    }
}
