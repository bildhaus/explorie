use crate::listing::normalize_path;
use crate::{
    ActiveOperation, BlockingTask, ErrorCode, FileOperationEvent, FileOperationState,
    ServiceContext, ServiceError, ServiceEvent, ServiceResult, SharedState,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use uuid::Uuid;

/// Typed requests for the small, user-initiated mutations that do not need a
/// long-running operation queue entry.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum SafeMutationRequest {
    Rename {
        source_path: PathBuf,
        new_base_name: String,
    },
    CreateFolder {
        dir_path: PathBuf,
        base_name: String,
    },
    CreateNote {
        dir_path: PathBuf,
        base_name: String,
    },
    CreateWebsiteLink {
        dir_path: PathBuf,
        base_name: String,
        url: String,
    },
    DeletePermanently {
        path: PathBuf,
        recursive: bool,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermanentDeleteFailure {
    pub path: PathBuf,
    pub error: ServiceError,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermanentDeleteResult {
    pub requested_items: usize,
    pub deleted_items: usize,
    pub failure: Option<PermanentDeleteFailure>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRenameItem {
    pub source_path: PathBuf,
    pub new_base_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRenamePair {
    pub before: PathBuf,
    pub after: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRenameResult {
    pub renamed: Vec<BatchRenamePair>,
}

#[derive(Clone)]
pub struct MutationService {
    context: ServiceContext,
    shared: Arc<SharedState>,
    batch_rename_lock: Arc<Mutex<()>>,
}

impl MutationService {
    pub(crate) fn new(context: ServiceContext, shared: Arc<SharedState>) -> Self {
        let journal_path = batch_rename_journal_path(context.resources().config_dir.as_path());
        if let Err(error) = recover_batch_rename_journal(&journal_path) {
            eprintln!(
                "unable to recover interrupted batch rename at {}: {error}",
                journal_path.display()
            );
        }
        Self {
            context,
            shared,
            batch_rename_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn start_file_operation(
        &self,
        request: explorie_core::FileOperationRequest,
    ) -> ServiceResult<String> {
        if request
            .sources
            .iter()
            .any(|source| self.is_remote_root(source))
            || request
                .destination
                .as_deref()
                .is_some_and(|destination| self.is_remote_root(destination))
        {
            return Err(ServiceError::new(
                ErrorCode::RemoteUnavailable,
                "Refusing to mutate a managed remote-drive root",
            ));
        }

        let job_id = format!(
            "{}-{}",
            std::process::id(),
            self.shared.next_job_id.fetch_add(1, Ordering::Relaxed)
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        self.shared
            .cancellations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(file_cancellation_key(&job_id), Arc::clone(&cancelled));
        let guard = ActiveOperation::new(Arc::clone(&self.shared));
        let shared = Arc::clone(&self.shared);
        let events = self.context.events();
        let task_job_id = job_id.clone();
        std::thread::spawn(move || {
            let request_sources = request.sources.clone();
            let operation_kind = request.kind;
            let operation =
                explorie_core::perform_file_operation_report(request, &cancelled, |progress| {
                    events.publish(ServiceEvent::FileOperation(FileOperationEvent {
                        job_id: task_job_id.clone(),
                        state: FileOperationState::Running,
                        progress: Some(progress),
                        result: None,
                        retryable_sources: Vec::new(),
                        error: None,
                    }));
                });

            let event = match operation {
                Ok(result) => FileOperationEvent {
                    job_id: task_job_id.clone(),
                    state: FileOperationState::Completed,
                    progress: None,
                    result: Some(result),
                    retryable_sources: Vec::new(),
                    error: None,
                },
                Err(failure) => {
                    let retryable_sources =
                        if operation_kind == explorie_core::FileOperationKind::Trash {
                            Vec::new()
                        } else {
                            let completed = failure
                                .partial_result
                                .targets
                                .len()
                                .min(request_sources.len());
                            request_sources[completed..].to_vec()
                        };
                    if failure.error.kind() == io::ErrorKind::Interrupted {
                        FileOperationEvent {
                            job_id: task_job_id.clone(),
                            state: FileOperationState::Cancelled,
                            progress: None,
                            result: Some(failure.partial_result),
                            retryable_sources,
                            error: None,
                        }
                    } else {
                        FileOperationEvent {
                            job_id: task_job_id.clone(),
                            state: FileOperationState::Failed,
                            progress: None,
                            result: Some(failure.partial_result),
                            retryable_sources,
                            error: Some(ServiceError::from(failure.error)),
                        }
                    }
                }
            };
            remove_job(&shared, &task_job_id);
            events.publish(ServiceEvent::FileOperation(event));
            drop(guard);
        });

        Ok(job_id)
    }

    pub fn cancel_file_operation(&self, job_id: &str) -> bool {
        let cancellations = self
            .shared
            .cancellations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cancelled) = cancellations.get(&file_cancellation_key(job_id)) {
            cancelled.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn cancel_all(&self) {
        for cancelled in self
            .shared
            .cancellations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            cancelled.store(true, Ordering::Relaxed);
        }
    }

    pub fn active_count(&self) -> u64 {
        self.shared.active_mutations.load(Ordering::Acquire)
    }

    /// Mark an exit attempt. The host should prevent exit and call
    /// [`cancel_all`](Self::cancel_all) while this returns `true`.
    pub fn request_exit(&self) -> bool {
        let active = self.active_count() > 0;
        self.shared.exit_requested.store(active, Ordering::Release);
        active
    }

    pub fn rename_path(&self, source_path: PathBuf, new_base_name: String) -> BlockingTask<String> {
        let shared = Arc::clone(&self.shared);
        let guard = ActiveOperation::new(Arc::clone(&shared));
        self.context.spawn_blocking(move || {
            if remote_root(&shared, &source_path) {
                return Err(ServiceError::new(
                    ErrorCode::RemoteUnavailable,
                    "Refusing to rename a managed remote-drive root",
                ));
            }
            let _guard = guard;
            rename_path_impl(&source_path, &new_base_name).map_err(ServiceError::from)
        })
    }

    pub fn batch_rename(&self, items: Vec<BatchRenameItem>) -> BlockingTask<BatchRenameResult> {
        let shared = Arc::clone(&self.shared);
        let lock = Arc::clone(&self.batch_rename_lock);
        let journal_path = batch_rename_journal_path(self.context.resources().config_dir.as_path());
        let guard = ActiveOperation::new(Arc::clone(&shared));
        self.context.spawn_blocking(move || {
            let _guard = guard;
            let _transaction = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            recover_batch_rename_journal(&journal_path).map_err(ServiceError::from)?;
            batch_rename_impl(&shared, items, &journal_path).map_err(ServiceError::from)
        })
    }

    pub fn create_folder(&self, dir_path: PathBuf, base_name: String) -> BlockingTask<String> {
        let shared = Arc::clone(&self.shared);
        let guard = ActiveOperation::new(Arc::clone(&shared));
        self.context.spawn_blocking(move || {
            if remote_root(&shared, &dir_path) {
                return Err(ServiceError::new(
                    ErrorCode::RemoteUnavailable,
                    "Refusing to mutate a managed remote-drive root",
                ));
            }
            let _guard = guard;
            create_folder_impl(&dir_path, &base_name).map_err(ServiceError::from)
        })
    }

    pub fn create_note(&self, dir_path: PathBuf, base_name: String) -> BlockingTask<String> {
        let shared = Arc::clone(&self.shared);
        let guard = ActiveOperation::new(Arc::clone(&shared));
        self.context.spawn_blocking(move || {
            if remote_root(&shared, &dir_path) {
                return Err(ServiceError::new(
                    ErrorCode::RemoteUnavailable,
                    "Refusing to mutate a managed remote-drive root",
                ));
            }
            let _guard = guard;
            create_note_impl(&dir_path, &base_name).map_err(ServiceError::from)
        })
    }

    pub fn create_website_link(
        &self,
        dir_path: PathBuf,
        base_name: String,
        url: String,
    ) -> BlockingTask<String> {
        let shared = Arc::clone(&self.shared);
        let guard = ActiveOperation::new(Arc::clone(&shared));
        self.context.spawn_blocking(move || {
            if remote_root(&shared, &dir_path) {
                return Err(ServiceError::new(
                    ErrorCode::RemoteUnavailable,
                    "Refusing to mutate a managed remote-drive root",
                ));
            }
            let _guard = guard;
            create_website_link_impl(&dir_path, &base_name, &url).map_err(ServiceError::from)
        })
    }

    pub fn delete_permanently(&self, path: PathBuf, recursive: bool) -> BlockingTask<()> {
        let shared = Arc::clone(&self.shared);
        let guard = ActiveOperation::new(Arc::clone(&shared));
        self.context.spawn_blocking(move || {
            if remote_root(&shared, &path) {
                return Err(ServiceError::new(
                    ErrorCode::RemoteUnavailable,
                    "Refusing to delete a managed remote-drive root",
                ));
            }
            let _guard = guard;
            delete_path_permanently_impl(&path, recursive).map_err(ServiceError::from)
        })
    }

    pub fn delete_permanently_batch(
        &self,
        items: Vec<(PathBuf, bool)>,
    ) -> BlockingTask<PermanentDeleteResult> {
        let shared = Arc::clone(&self.shared);
        let guard = ActiveOperation::new(Arc::clone(&shared));
        self.context.spawn_blocking(move || {
            let requested_items = items.len();
            let _guard = guard;
            let mut deleted_items = 0;
            for (path, recursive) in items {
                let result = if remote_root(&shared, &path) {
                    Err(ServiceError::new(
                        ErrorCode::RemoteUnavailable,
                        "Refusing to delete a managed remote-drive root",
                    ))
                } else {
                    delete_path_permanently_impl(&path, recursive).map_err(ServiceError::from)
                };
                if let Err(error) = result {
                    return Ok(PermanentDeleteResult {
                        requested_items,
                        deleted_items,
                        failure: Some(PermanentDeleteFailure { path, error }),
                    });
                }
                deleted_items += 1;
            }
            Ok(PermanentDeleteResult {
                requested_items,
                deleted_items,
                failure: None,
            })
        })
    }

    pub fn path_exists(&self, path: PathBuf) -> BlockingTask<bool> {
        self.context
            .spawn_blocking(move || path.try_exists().map_err(ServiceError::from))
    }

    pub fn path_matches_snapshot(
        &self,
        path: PathBuf,
        snapshot: explorie_core::FileTreeSnapshot,
    ) -> BlockingTask<bool> {
        self.context.spawn_blocking(move || {
            explorie_core::path_matches_snapshot(&path, &snapshot).map_err(ServiceError::from)
        })
    }

    pub fn run_safe(&self, request: SafeMutationRequest) -> BlockingTask<String> {
        match request {
            SafeMutationRequest::Rename {
                source_path,
                new_base_name,
            } => self.rename_path(source_path, new_base_name),
            SafeMutationRequest::CreateFolder {
                dir_path,
                base_name,
            } => self.create_folder(dir_path, base_name),
            SafeMutationRequest::CreateNote {
                dir_path,
                base_name,
            } => self.create_note(dir_path, base_name),
            SafeMutationRequest::CreateWebsiteLink {
                dir_path,
                base_name,
                url,
            } => self.create_website_link(dir_path, base_name, url),
            SafeMutationRequest::DeletePermanently { path, recursive } => {
                let task = self.delete_permanently(path, recursive);
                self.context
                    .spawn_blocking(move || task.wait().map(|()| String::new()))
            }
        }
    }

    fn is_remote_root(&self, path: &Path) -> bool {
        remote_root(&self.shared, path)
    }
}

fn remote_root(shared: &SharedState, path: &Path) -> bool {
    shared
        .remote_roots
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&normalize_path(path))
}

fn validate_file_name(name: &str) -> io::Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "File name cannot be empty",
        ));
    }
    if name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "File name cannot contain path separators or be . or ..",
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "File name cannot contain control characters",
        ));
    }
    #[cfg(windows)]
    {
        if name.contains(['<', '>', ':', '"', '|', '?', '*']) || name.ends_with(['.', ' ']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "File name is invalid on Windows",
            ));
        }
        let base = name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || base
                .strip_prefix("COM")
                .or_else(|| base.strip_prefix("LPT"))
                .is_some_and(|number| {
                    matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "File name is reserved on Windows",
            ));
        }
    }
    Ok(name.to_string())
}

fn ensure_extension(name: &str, extension: &str) -> String {
    if name
        .to_ascii_lowercase()
        .ends_with(&extension.to_ascii_lowercase())
    {
        name.to_string()
    } else {
        format!("{name}{extension}")
    }
}

/// Symbolic links and, on Windows, junctions: std reports a link only for
/// name-surrogate reparse tags, so cloud-file placeholders (OneDrive) and
/// deduplicated files are treated as the files and folders they are.
fn metadata_is_link(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(target_os = "macos")]
fn is_macos_system_root_alias(path: &Path) -> bool {
    // macOS exposes these immutable, root-owned aliases for compatibility.
    // Paths below them are ordinary local paths; all other link ancestors
    // remain rejected, and a link used as the operation target is still rejected.
    let expected = match path.to_str() {
        Some("/etc") => Path::new("/private/etc"),
        Some("/tmp") => Path::new("/private/tmp"),
        Some("/var") => Path::new("/private/var"),
        _ => return false,
    };
    fs::canonicalize(path).is_ok_and(|canonical| canonical == expected)
}

#[cfg(not(target_os = "macos"))]
fn is_macos_system_root_alias(_path: &Path) -> bool {
    false
}

fn validate_no_link_ancestors(path: &Path) -> io::Result<()> {
    let mut ancestors: Vec<&Path> = path.ancestors().collect();
    ancestors.reverse();
    for ancestor in ancestors {
        let metadata = fs::symlink_metadata(ancestor)?;
        if metadata_is_link(&metadata) && !is_macos_system_root_alias(ancestor) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Refusing to traverse a symbolic link or junction: {}",
                    ancestor.display()
                ),
            ));
        }
    }
    Ok(())
}

fn validate_real_directory(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Directory path must be absolute",
        ));
    }
    validate_no_link_ancestors(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata_is_link(&metadata) || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Destination must be a real directory",
        ));
    }
    Ok(())
}

fn mount_check_path(_path: &Path, canonical: io::Result<PathBuf>) -> io::Result<PathBuf> {
    match canonical {
        Ok(path) => Ok(path),
        #[cfg(windows)]
        Err(error) if error.raw_os_error() == Some(1005) => Ok(_path.to_path_buf()),
        Err(error) => Err(error),
    }
}

fn ensure_not_mount_root(path: &Path) -> io::Result<()> {
    let canonical = mount_check_path(path, fs::canonicalize(path))?;
    if canonical.parent().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Refusing to mutate a filesystem root",
        ));
    }
    let disks = sysinfo::Disks::new_with_refreshed_list();
    if disks.iter().any(|disk| {
        mount_check_path(disk.mount_point(), fs::canonicalize(disk.mount_point()))
            .is_ok_and(|mount| mount == canonical)
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Refusing to mutate a mounted volume root",
        ));
    }
    Ok(())
}

/// `base_name` for the first attempt, then numbered the platform's way
/// ("untitled folder 2" on macOS, "untitled folder (2)" elsewhere), the same
/// numbering Keep Both uses for copies.
fn suffixed_name(base_name: &str, number: u32, preserve_extension: bool) -> OsString {
    if number == 1 {
        return OsString::from(base_name);
    }
    explorie_core::numbered_name(std::ffi::OsStr::new(base_name), number, preserve_extension)
}

fn create_unique_path(
    parent: &Path,
    base_name: &str,
    preserve_extension: bool,
    mut create: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    for number in 1..=9_999 {
        let candidate = parent.join(suffixed_name(base_name, number, preserve_extension));
        match fs::symlink_metadata(&candidate) {
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Could not find a unique file name",
    ))
}

fn write_new_text_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(contents)?;
        file.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn rename_path_impl(source: &Path, new_base_name: &str) -> io::Result<String> {
    if !source.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Source path must be absolute",
        ));
    }
    let metadata = fs::symlink_metadata(source)?;
    if !metadata_is_link(&metadata) {
        ensure_not_mount_root(source)?;
    }
    if !metadata_is_link(&metadata) && !metadata.is_file() && !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Unsupported filesystem entry",
        ));
    }
    let parent = source.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Cannot rename a filesystem root",
        )
    })?;
    validate_real_directory(parent)?;
    let name = validate_file_name(new_base_name)?;
    if source.file_name() == Some(std::ffi::OsStr::new(&name)) {
        // Submitting the unchanged name is not a rename.
        return Ok(source.to_string_lossy().into_owned());
    }
    // On case- or normalization-insensitive volumes (APFS, NTFS, exFAT) a name
    // that differs only in case or Unicode form still denotes the source:
    // rename it in place rather than choosing "name 2".
    let requested = parent.join(&name);
    if is_same_entry(source, &metadata, &requested)? {
        rename_same_entry(source, &requested)?;
        return Ok(requested.to_string_lossy().into_owned());
    }
    let destination = create_unique_path(parent, &name, true, |candidate| {
        explorie_core::rename_noreplace(source, candidate)
    })?;
    Ok(destination.to_string_lossy().into_owned())
}

/// Whether `candidate` resolves to the source entry itself (without following
/// a final link).
#[cfg(unix)]
fn is_same_entry(
    _source: &Path,
    source_metadata: &fs::Metadata,
    candidate: &Path,
) -> io::Result<bool> {
    match fs::symlink_metadata(candidate) {
        Ok(candidate) => Ok(entry_identity(&candidate) == entry_identity(source_metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn is_same_entry(
    source: &Path,
    _source_metadata: &fs::Metadata,
    candidate: &Path,
) -> io::Result<bool> {
    match (windows_file_id(source)?, windows_file_id(candidate)?) {
        (Some(source), Some(candidate)) => Ok(source == candidate),
        _ => Ok(false),
    }
}

#[cfg(not(any(unix, windows)))]
fn is_same_entry(
    _source: &Path,
    _source_metadata: &fs::Metadata,
    _candidate: &Path,
) -> io::Result<bool> {
    Ok(false)
}

/// Volume serial number and file index, which identify an entry on Windows.
#[cfg(windows)]
fn windows_file_id(path: &Path) -> io::Result<Option<(u32, u64)>> {
    use std::mem::MaybeUninit;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        GetFileInformationByHandle,
    };

    let file = match OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let information = unsafe { information.assume_init() };
    Ok(Some((
        information.dwVolumeSerialNumber,
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    )))
}

/// Renames an entry to another spelling of its own name. The exclusive rename
/// handles case-only renames on APFS and NTFS; filesystems without it (exFAT)
/// report the name as taken, which is safe to override because it was just
/// verified to be the source itself.
fn rename_same_entry(source: &Path, destination: &Path) -> io::Result<()> {
    match explorie_core::rename_noreplace(source, destination) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            fs::rename(source, destination)
        }
        result => result,
    }
}

/// Device and inode, which identify an entry on Unix.
#[cfg(unix)]
fn entry_identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn entry_identity(_metadata: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

const BATCH_RENAME_JOURNAL: &str = "batch-rename-recovery-v1.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchRenameJournalItem {
    source: PathBuf,
    temporary: PathBuf,
    target: PathBuf,
    #[serde(default)]
    identity: Option<BatchRenameIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchRenameIdentity {
    kind: u8,
    len: u64,
    modified_ns: Option<u64>,
    platform_id: Option<(u64, u64)>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "phase")]
enum BatchRenameJournalPhase {
    Staging { staged: usize },
    Committing { committed: usize },
    Finished,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchRenameJournal {
    version: u32,
    phase: BatchRenameJournalPhase,
    items: Vec<BatchRenameJournalItem>,
}

fn batch_rename_journal_path(config_dir: &Path) -> PathBuf {
    config_dir.join(BATCH_RENAME_JOURNAL)
}

fn write_batch_rename_journal(path: &Path, journal: &BatchRenameJournal) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "batch rename journal has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{BATCH_RENAME_JOURNAL}.{}.tmp",
        Uuid::new_v4().simple()
    ));
    let result = (|| {
        let bytes = serde_json::to_vec_pretty(journal).map_err(io::Error::other)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        replace_journal_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(windows))]
fn replace_journal_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)?;
    sync_parent_directory(destination)
}

#[cfg(not(windows))]
fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "journal path has no parent"))?;
    fs::File::open(parent)?.sync_all()
}

#[cfg(windows)]
fn replace_journal_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn remove_batch_rename_journal(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {
            #[cfg(not(windows))]
            sync_parent_directory(path)?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn recover_batch_rename(journal: &BatchRenameJournal) -> io::Result<()> {
    if !matches!(journal.version, 1 | 2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported batch rename recovery version {}",
                journal.version
            ),
        ));
    }
    if matches!(journal.phase, BatchRenameJournalPhase::Finished) {
        return Ok(());
    }

    validate_batch_rename_journal(journal)?;
    if journal.version == 2 {
        return recover_identified_batch_rename(journal);
    }

    // A commit may have completed immediately before its progress update was
    // persisted. Move every published target back to its unique staging path
    // first so swaps and rename cycles can be restored without collisions.
    if matches!(journal.phase, BatchRenameJournalPhase::Committing { .. }) {
        for item in &journal.items {
            if !item.temporary.exists() && item.target.exists() {
                explorie_core::rename_noreplace(&item.target, &item.temporary)?;
            }
        }
    }
    for item in journal.items.iter().rev() {
        if item.temporary.exists() {
            explorie_core::rename_noreplace(&item.temporary, &item.source)?;
        } else if !item.source.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "batch rename recovery could not find {} or {}",
                    item.source.display(),
                    item.temporary.display()
                ),
            ));
        }
    }
    Ok(())
}

fn validate_batch_rename_journal(journal: &BatchRenameJournal) -> io::Result<()> {
    if journal.items.is_empty() || journal.items.len() > 10_000 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "batch rename recovery journal has an invalid item count",
        ));
    }
    let completed = match journal.phase {
        BatchRenameJournalPhase::Staging { staged } => staged,
        BatchRenameJournalPhase::Committing { committed } => committed,
        BatchRenameJournalPhase::Finished => 0,
    };
    if completed > journal.items.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "batch rename recovery journal has an invalid progress count",
        ));
    }

    let mut sources = HashSet::with_capacity(journal.items.len());
    let mut temporaries = HashSet::with_capacity(journal.items.len());
    let mut targets = HashSet::with_capacity(journal.items.len());
    for item in &journal.items {
        if !item.source.is_absolute() || !item.temporary.is_absolute() || !item.target.is_absolute()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "batch rename recovery paths must be absolute",
            ));
        }
        let parent = item.source.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "batch rename recovery source has no parent",
            )
        })?;
        if item.temporary.parent() != Some(parent) || item.target.parent() != Some(parent) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "batch rename recovery paths must share one parent per item",
            ));
        }
        let temporary_name = item
            .temporary
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if !temporary_name.starts_with(".explorie-rename-") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "batch rename recovery temporary path is not owned by Explorie",
            ));
        }
        if journal.version == 2 && item.identity.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "batch rename recovery journal is missing an item identity",
            ));
        }
        if !sources.insert(normalize_path(&item.source))
            || !temporaries.insert(normalize_path(&item.temporary))
            || !targets.insert(normalize_path(&item.target))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "batch rename recovery journal contains duplicate paths",
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BatchRenameLocation {
    Source,
    Temporary,
    Target,
}

fn recover_identified_batch_rename(journal: &BatchRenameJournal) -> io::Result<()> {
    let mut locations = Vec::with_capacity(journal.items.len());
    for item in &journal.items {
        let identity = item.identity.as_ref().expect("validated journal identity");
        if item.temporary.exists()
            && batch_rename_identity(&item.temporary).as_ref() != Some(identity)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "batch rename recovery temporary item changed: {}",
                    item.temporary.display()
                ),
            ));
        }
        let matches = [
            (BatchRenameLocation::Source, &item.source),
            (BatchRenameLocation::Temporary, &item.temporary),
            (BatchRenameLocation::Target, &item.target),
        ]
        .into_iter()
        .filter_map(|(location, path)| {
            (batch_rename_identity(path).as_ref() == Some(identity)).then_some(location)
        })
        .collect::<Vec<_>>();
        match matches.as_slice() {
            [location] => locations.push(*location),
            [] => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "batch rename recovery could not identify the original item for {}",
                        item.source.display()
                    ),
                ));
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "batch rename recovery found duplicate copies of {}",
                        item.source.display()
                    ),
                ));
            }
        }
    }

    for (item, location) in journal.items.iter().zip(&locations) {
        if *location == BatchRenameLocation::Target {
            explorie_core::rename_noreplace(&item.target, &item.temporary)?;
        }
    }
    for (item, location) in journal.items.iter().zip(locations).rev() {
        if location != BatchRenameLocation::Source {
            explorie_core::rename_noreplace(&item.temporary, &item.source)?;
        }
    }
    Ok(())
}

fn batch_rename_identity(path: &Path) -> Option<BatchRenameIdentity> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let kind = if metadata.file_type().is_symlink() {
        2
    } else if metadata.is_dir() {
        1
    } else if metadata.is_file() {
        0
    } else {
        3
    };
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok());
    #[cfg(unix)]
    let platform_id = {
        use std::os::unix::fs::MetadataExt;
        Some((metadata.dev(), metadata.ino()))
    };
    #[cfg(windows)]
    let platform_id = {
        use std::os::windows::fs::MetadataExt;
        Some((
            metadata.creation_time(),
            u64::from(metadata.file_attributes()),
        ))
    };
    #[cfg(not(any(unix, windows)))]
    let platform_id = None;
    Some(BatchRenameIdentity {
        kind,
        len: metadata.len(),
        modified_ns,
        platform_id,
    })
}

fn recover_batch_rename_journal(path: &Path) -> io::Result<()> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let journal: BatchRenameJournal = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    recover_batch_rename(&journal)?;
    remove_batch_rename_journal(path)
}

fn batch_rename_impl(
    shared: &SharedState,
    items: Vec<BatchRenameItem>,
    journal_path: &Path,
) -> io::Result<BatchRenameResult> {
    if items.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Batch rename requires at least one changed item",
        ));
    }
    if items.len() > 10_000 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Batch rename is limited to 10,000 items",
        ));
    }

    let mut sources = HashSet::with_capacity(items.len());
    let mut source_identities = HashSet::with_capacity(items.len());
    let mut targets = HashSet::with_capacity(items.len());
    let mut plan = Vec::with_capacity(items.len());
    for item in items {
        let source = item.source_path;
        if !source.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Batch rename source paths must be absolute",
            ));
        }
        if remote_root(shared, &source) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Refusing to rename a managed remote-drive root",
            ));
        }
        let metadata = fs::symlink_metadata(&source)?;
        if !metadata_is_link(&metadata) {
            ensure_not_mount_root(&source)?;
        }
        if !metadata_is_link(&metadata) && !metadata.is_file() && !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unsupported filesystem entry in batch rename",
            ));
        }
        source_identities.extend(entry_identity(&metadata));
        let parent = source.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cannot rename a filesystem root",
            )
        })?;
        validate_real_directory(parent)?;
        let name = validate_file_name(&item.new_base_name)?;
        let target = parent.join(name);
        let source_key = normalize_path(&source);
        let target_key = normalize_path(&target);
        if !sources.insert(source_key) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Batch rename source paths must be unique",
            ));
        }
        if !targets.insert(target_key) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Batch rename would create duplicate names",
            ));
        }
        if source != target {
            plan.push((source, target));
        }
    }

    if plan.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Batch rename does not change any names",
        ));
    }

    // A target that already exists is acceptable only when it is one of the
    // sources, which are staged away first. Case-insensitive volumes report
    // "Foo.txt" as existing when renaming "foo.txt", so compare identities too.
    for (_, target) in &plan {
        match fs::symlink_metadata(target) {
            Ok(metadata)
                if !sources.contains(&normalize_path(target))
                    && !entry_identity(&metadata)
                        .is_some_and(|identity| source_identities.contains(&identity)) =>
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("Batch rename target already exists: {}", target.display()),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    let transaction = Uuid::new_v4().simple().to_string();
    let staged: Vec<(PathBuf, PathBuf, PathBuf)> = plan
        .iter()
        .enumerate()
        .map(|(index, (source, target))| {
            let parent = source.parent().expect("validated batch source parent");
            let temporary = parent.join(format!(".explorie-rename-{transaction}-{index}"));
            (source.clone(), temporary, target.clone())
        })
        .collect();
    let mut journal = BatchRenameJournal {
        version: 2,
        phase: BatchRenameJournalPhase::Staging { staged: 0 },
        items: staged
            .iter()
            .map(|(source, temporary, target)| BatchRenameJournalItem {
                source: source.clone(),
                temporary: temporary.clone(),
                target: target.clone(),
                identity: batch_rename_identity(source),
            })
            .collect(),
    };
    write_batch_rename_journal(journal_path, &journal)?;

    for (index, (source, temporary, _)) in staged.iter().enumerate() {
        if let Err(error) = explorie_core::rename_noreplace(source, temporary) {
            let rollback = recover_batch_rename(&journal)
                .and_then(|()| remove_batch_rename_journal(journal_path));
            return Err(batch_rename_error("staging", error, rollback));
        }
        journal.phase = BatchRenameJournalPhase::Staging { staged: index + 1 };
        if let Err(error) = write_batch_rename_journal(journal_path, &journal) {
            let rollback = recover_batch_rename(&journal)
                .and_then(|()| remove_batch_rename_journal(journal_path));
            return Err(batch_rename_error("journal update", error, rollback));
        }
    }

    journal.phase = BatchRenameJournalPhase::Committing { committed: 0 };
    if let Err(error) = write_batch_rename_journal(journal_path, &journal) {
        let rollback =
            recover_batch_rename(&journal).and_then(|()| remove_batch_rename_journal(journal_path));
        return Err(batch_rename_error("journal update", error, rollback));
    }
    for (committed, (_, temporary, target)) in staged.iter().enumerate() {
        if let Err(error) = explorie_core::rename_noreplace(temporary, target) {
            let rollback = recover_batch_rename(&journal)
                .and_then(|()| remove_batch_rename_journal(journal_path));
            return Err(batch_rename_error("commit", error, rollback));
        }
        journal.phase = BatchRenameJournalPhase::Committing {
            committed: committed + 1,
        };
        if let Err(error) = write_batch_rename_journal(journal_path, &journal) {
            let rollback = recover_batch_rename(&journal)
                .and_then(|()| remove_batch_rename_journal(journal_path));
            return Err(batch_rename_error("journal update", error, rollback));
        }
    }

    journal.phase = BatchRenameJournalPhase::Finished;
    write_batch_rename_journal(journal_path, &journal)?;
    if let Err(error) = remove_batch_rename_journal(journal_path) {
        eprintln!(
            "batch rename completed but its finished journal could not be removed at {}: {error}",
            journal_path.display()
        );
    }

    Ok(BatchRenameResult {
        renamed: staged
            .into_iter()
            .map(|(before, _, after)| BatchRenamePair { before, after })
            .collect(),
    })
}

fn batch_rename_error(stage: &str, error: io::Error, rollback: io::Result<()>) -> io::Error {
    match rollback {
        Ok(()) => io::Error::new(
            error.kind(),
            format!("Batch rename {stage} failed and was rolled back: {error}"),
        ),
        Err(rollback_error) => io::Error::other(format!(
            "Batch rename {stage} failed ({error}); rollback also failed ({rollback_error}). Manual recovery may be required for .explorie-rename-* entries."
        )),
    }
}

fn create_folder_impl(directory: &Path, base_name: &str) -> io::Result<String> {
    validate_real_directory(directory)?;
    let name = validate_file_name(base_name)?;
    let path = create_unique_path(directory, &name, false, |candidate| {
        fs::create_dir(candidate)
    })?;
    Ok(path.to_string_lossy().into_owned())
}

fn create_note_impl(directory: &Path, base_name: &str) -> io::Result<String> {
    validate_real_directory(directory)?;
    let name = validate_file_name(&ensure_extension(base_name, ".md"))?;
    let path = create_unique_path(directory, &name, true, |candidate| {
        write_new_text_file(candidate, b"# New Note\n")
    })?;
    Ok(path.to_string_lossy().into_owned())
}

fn create_website_link_impl(directory: &Path, base_name: &str, url: &str) -> io::Result<String> {
    validate_real_directory(directory)?;
    let name = validate_file_name(&ensure_extension(base_name, ".url"))?;
    let url = url.trim();
    if url.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Website URL cannot contain control characters",
        ));
    }
    let normalized = url.to_ascii_lowercase();
    if !normalized.starts_with("https://") && !normalized.starts_with("http://") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Website URL must use http or https",
        ));
    }
    let contents = format!("[InternetShortcut]\nURL={url}\n");
    let path = create_unique_path(directory, &name, true, |candidate| {
        write_new_text_file(candidate, contents.as_bytes())
    })?;
    Ok(path.to_string_lossy().into_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeleteEntryKind {
    File,
    Directory,
    Link,
}

fn delete_entry_kind(metadata: &fs::Metadata) -> io::Result<DeleteEntryKind> {
    if metadata_is_link(metadata) {
        Ok(DeleteEntryKind::Link)
    } else if metadata.is_file() {
        Ok(DeleteEntryKind::File)
    } else if metadata.is_dir() {
        Ok(DeleteEntryKind::Directory)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "special filesystem entries cannot be permanently deleted",
        ))
    }
}

#[cfg(unix)]
fn same_device(root: &fs::Metadata, candidate: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    root.dev() == candidate.dev()
}

#[cfg(not(unix))]
fn same_device(_root: &fs::Metadata, _candidate: &fs::Metadata) -> bool {
    true
}

fn collect_delete_entries(
    path: &Path,
    root_metadata: &fs::Metadata,
    entries: &mut Vec<(PathBuf, DeleteEntryKind)>,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let kind = delete_entry_kind(&metadata)?;
    if kind != DeleteEntryKind::Link && !same_device(root_metadata, &metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to cross a mounted filesystem at {}",
                path.display()
            ),
        ));
    }
    entries.push((path.to_path_buf(), kind));
    if kind == DeleteEntryKind::Directory {
        for child in fs::read_dir(path)? {
            collect_delete_entries(&child?.path(), root_metadata, entries)?;
        }
    }
    Ok(())
}

fn remove_link(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(file_error) => fs::remove_dir(path).map_err(|_| file_error),
    }
}

fn remove_planned_entry(path: &Path, expected: DeleteEntryKind) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let actual = delete_entry_kind(&metadata)?;
    if actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "filesystem entry changed during deletion: {}",
                path.display()
            ),
        ));
    }
    match actual {
        DeleteEntryKind::File => fs::remove_file(path),
        DeleteEntryKind::Directory => fs::remove_dir(path),
        DeleteEntryKind::Link => remove_link(path),
    }
}

fn delete_path_permanently_impl(path: &Path, recursive: bool) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Delete path must be absolute",
        ));
    }
    if let Some(parent) = path.parent() {
        validate_no_link_ancestors(parent)?;
    }
    let root_metadata = fs::symlink_metadata(path)?;
    let root_kind = delete_entry_kind(&root_metadata)?;
    if root_kind != DeleteEntryKind::Link {
        ensure_not_mount_root(path)?;
    }
    if root_kind != DeleteEntryKind::Directory || !recursive {
        return remove_planned_entry(path, root_kind);
    }
    let mut entries = Vec::new();
    collect_delete_entries(path, &root_metadata, &mut entries)?;
    for (entry, kind) in entries.into_iter().rev() {
        remove_planned_entry(&entry, kind)?;
    }
    Ok(())
}

fn remove_job(shared: &SharedState, job_id: &str) {
    shared
        .cancellations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&file_cancellation_key(job_id));
}

fn file_cancellation_key(job_id: &str) -> String {
    format!("file:{job_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NativeServices, ResourcePaths};
    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temporary directory")
    }

    #[test]
    fn safe_mutations_never_overwrite_and_validate_urls() {
        let temp = temp_dir();
        let root = temp.path().to_path_buf();
        fs::write(root.join("report.txt"), "existing").unwrap();
        fs::write(root.join("draft.txt"), "draft").unwrap();
        let services = NativeServices::new(ResourcePaths::test(&root));

        let renamed = services
            .mutations
            .rename_path(root.join("draft.txt"), "report.txt".into())
            .wait()
            .unwrap();
        assert_eq!(
            Path::new(&renamed).file_name().unwrap(),
            if cfg!(target_os = "macos") {
                "report 2.txt"
            } else {
                "report (2).txt"
            }
        );
        assert_eq!(
            fs::read_to_string(root.join("report.txt")).unwrap(),
            "existing"
        );

        let note = services
            .mutations
            .create_note(root.clone(), "Meeting Notes".into())
            .wait()
            .unwrap();
        assert_eq!(fs::read_to_string(note).unwrap(), "# New Note\n");
        assert!(
            services
                .mutations
                .path_exists(root.join("report.txt"))
                .wait()
                .unwrap()
        );
        assert!(
            !services
                .mutations
                .path_exists(root.join("missing.txt"))
                .wait()
                .unwrap()
        );
        assert!(
            services
                .mutations
                .create_website_link(root, "unsafe".into(), "javascript:alert(1)".into())
                .wait()
                .is_err()
        );
    }

    fn names_in(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn rename_to_the_unchanged_name_is_a_no_op() {
        let temp = temp_dir();
        let directory = temp.path().join("files");
        let source = directory.join("notes.txt");
        fs::create_dir(&directory).unwrap();
        fs::write(&source, "keep").unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));

        let renamed = services
            .mutations
            .rename_path(source.clone(), " notes.txt ".into())
            .wait()
            .unwrap();

        assert_eq!(PathBuf::from(renamed), source);
        assert_eq!(names_in(&directory), ["notes.txt"]);
    }

    #[test]
    fn case_only_rename_renames_in_place() {
        let temp = temp_dir();
        let directory = temp.path().join("files");
        fs::create_dir_all(directory.join("folder")).unwrap();
        fs::write(directory.join("notes.txt"), "keep").unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));

        for (before, after) in [("notes.txt", "Notes.txt"), ("folder", "FOLDER")] {
            let renamed = services
                .mutations
                .rename_path(directory.join(before), after.into())
                .wait()
                .unwrap();
            assert_eq!(PathBuf::from(renamed), directory.join(after));
        }

        assert_eq!(names_in(&directory), ["FOLDER", "Notes.txt"]);
        assert_eq!(
            fs::read_to_string(directory.join("Notes.txt")).unwrap(),
            "keep"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unicode_normalization_only_rename_renames_in_place() {
        let temp = temp_dir();
        let directory = temp.path().join("files");
        let composed = "caf\u{e9}.txt";
        let decomposed = "cafe\u{301}.txt";
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join(composed), "keep").unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));

        let renamed = services
            .mutations
            .rename_path(directory.join(composed), decomposed.into())
            .wait()
            .unwrap();

        assert_eq!(PathBuf::from(renamed), directory.join(decomposed));
        assert_eq!(names_in(&directory), [decomposed]);
    }

    #[test]
    fn batch_rename_changes_letter_case() {
        let temp = temp_dir();
        let directory = temp.path().join("files");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("alpha.txt"), "alpha").unwrap();
        fs::write(directory.join("Beta.txt"), "beta").unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));

        let result = services
            .mutations
            .batch_rename(vec![
                BatchRenameItem {
                    source_path: directory.join("alpha.txt"),
                    new_base_name: "ALPHA.txt".to_string(),
                },
                BatchRenameItem {
                    source_path: directory.join("Beta.txt"),
                    new_base_name: "beta.txt".to_string(),
                },
            ])
            .wait()
            .unwrap();

        assert_eq!(result.renamed.len(), 2);
        assert_eq!(names_in(&directory), ["ALPHA.txt", "beta.txt"]);
        assert_eq!(
            fs::read_to_string(directory.join("ALPHA.txt")).unwrap(),
            "alpha"
        );
    }

    #[cfg(unix)]
    #[test]
    fn mutations_reject_user_controlled_link_ancestors() {
        use std::os::unix::fs::symlink;

        let temp = temp_dir();
        let real = temp.path().join("real");
        let alias = temp.path().join("alias");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("source.txt"), "keep").unwrap();
        symlink(&real, &alias).unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));

        let error = services
            .mutations
            .rename_path(alias.join("source.txt"), "renamed.txt".into())
            .wait()
            .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert_eq!(fs::read_to_string(real.join("source.txt")).unwrap(), "keep");
        assert!(!real.join("renamed.txt").exists());
    }

    #[test]
    fn batch_rename_supports_swaps_without_overwriting() {
        let temp = temp_dir();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        fs::write(&first, "first contents").unwrap();
        fs::write(&second, "second contents").unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));

        let result = services
            .mutations
            .batch_rename(vec![
                BatchRenameItem {
                    source_path: first.clone(),
                    new_base_name: "second.txt".to_string(),
                },
                BatchRenameItem {
                    source_path: second.clone(),
                    new_base_name: "first.txt".to_string(),
                },
            ])
            .wait()
            .unwrap();

        assert_eq!(result.renamed.len(), 2);
        assert_eq!(fs::read_to_string(first).unwrap(), "second contents");
        assert_eq!(fs::read_to_string(second).unwrap(), "first contents");
        assert_eq!(services.mutations.active_count(), 0);
        assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".explorie-rename-")
        }));
    }

    #[test]
    fn batch_rename_rejects_external_conflicts_before_changing_any_source() {
        let temp = temp_dir();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        let occupied = temp.path().join("occupied.txt");
        fs::write(&first, "first contents").unwrap();
        fs::write(&second, "second contents").unwrap();
        fs::write(&occupied, "keep contents").unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));

        let result = services
            .mutations
            .batch_rename(vec![
                BatchRenameItem {
                    source_path: first.clone(),
                    new_base_name: "renamed.txt".to_string(),
                },
                BatchRenameItem {
                    source_path: second.clone(),
                    new_base_name: "occupied.txt".to_string(),
                },
            ])
            .wait();

        assert!(result.is_err());
        assert_eq!(fs::read_to_string(first).unwrap(), "first contents");
        assert_eq!(fs::read_to_string(second).unwrap(), "second contents");
        assert_eq!(fs::read_to_string(occupied).unwrap(), "keep contents");
    }

    #[test]
    fn batch_rename_commits_a_single_changed_item() {
        let temp = temp_dir();
        let source = temp.path().join("before.txt");
        let target = temp.path().join("after.txt");
        fs::write(&source, "contents").unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));

        let result = services
            .mutations
            .batch_rename(vec![BatchRenameItem {
                source_path: source.clone(),
                new_base_name: "after.txt".to_string(),
            }])
            .wait()
            .unwrap();

        assert_eq!(result.renamed.len(), 1);
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(target).unwrap(), "contents");
        assert_eq!(services.mutations.active_count(), 0);
    }

    #[test]
    fn batch_rename_journal_recovers_an_interrupted_staging_pass() {
        let temp = temp_dir();
        let source = temp.path().join("before.txt");
        let staged = temp.path().join(".explorie-rename-test");
        let target = temp.path().join("after.txt");
        fs::write(&source, "contents").unwrap();
        let identity = batch_rename_identity(&source);
        fs::rename(&source, &staged).unwrap();
        let journal = BatchRenameJournal {
            version: 2,
            items: vec![BatchRenameJournalItem {
                source: source.clone(),
                temporary: staged.clone(),
                target,
                identity,
            }],
            phase: BatchRenameJournalPhase::Staging { staged: 1 },
        };

        recover_batch_rename(&journal).unwrap();

        assert_eq!(fs::read_to_string(source).unwrap(), "contents");
        assert!(!staged.exists());
    }

    #[cfg(unix)]
    #[test]
    fn unix_batch_rename_journal_persists_replaces_and_removes_durably() {
        let temp = temp_dir();
        let journal_path = batch_rename_journal_path(temp.path());
        let mut journal = BatchRenameJournal {
            version: 2,
            phase: BatchRenameJournalPhase::Staging { staged: 0 },
            items: Vec::new(),
        };

        write_batch_rename_journal(&journal_path, &journal).unwrap();
        assert!(journal_path.is_file());

        journal.phase = BatchRenameJournalPhase::Finished;
        write_batch_rename_journal(&journal_path, &journal).unwrap();
        let persisted: BatchRenameJournal =
            serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
        assert!(matches!(persisted.phase, BatchRenameJournalPhase::Finished));

        remove_batch_rename_journal(&journal_path).unwrap();
        assert!(!journal_path.exists());
    }

    #[test]
    fn batch_rename_journal_recovers_a_partially_committed_swap() {
        let temp = temp_dir();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        let first_staged = temp.path().join(".explorie-rename-first");
        let second_staged = temp.path().join(".explorie-rename-second");
        fs::write(&first, "first contents").unwrap();
        fs::write(&second, "second contents").unwrap();
        fs::rename(&first, &first_staged).unwrap();
        fs::rename(&second, &second_staged).unwrap();
        fs::rename(&first_staged, &second).unwrap();
        let journal = BatchRenameJournal {
            version: 1,
            items: vec![
                BatchRenameJournalItem {
                    source: first.clone(),
                    temporary: first_staged.clone(),
                    target: second.clone(),
                    identity: None,
                },
                BatchRenameJournalItem {
                    source: second.clone(),
                    temporary: second_staged.clone(),
                    target: first.clone(),
                    identity: None,
                },
            ],
            phase: BatchRenameJournalPhase::Committing { committed: 1 },
        };

        recover_batch_rename(&journal).unwrap();

        assert_eq!(fs::read_to_string(first).unwrap(), "first contents");
        assert_eq!(fs::read_to_string(second).unwrap(), "second contents");
        assert!(!first_staged.exists());
        assert!(!second_staged.exists());
    }

    #[test]
    fn batch_rename_journal_refuses_to_move_a_replaced_staged_item() {
        let temp = temp_dir();
        let source = temp.path().join("before.txt");
        let staged = temp.path().join(".explorie-rename-test");
        let target = temp.path().join("after.txt");
        fs::write(&source, "original").unwrap();
        let identity = batch_rename_identity(&source);
        fs::rename(&source, &staged).unwrap();
        fs::write(&staged, "replacement with a different size").unwrap();
        let journal = BatchRenameJournal {
            version: 2,
            items: vec![BatchRenameJournalItem {
                source: source.clone(),
                temporary: staged.clone(),
                target,
                identity,
            }],
            phase: BatchRenameJournalPhase::Staging { staged: 1 },
        };

        let error = recover_batch_rename(&journal).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!source.exists());
        assert_eq!(
            fs::read_to_string(staged).unwrap(),
            "replacement with a different size"
        );
    }

    #[test]
    fn exit_request_tracks_active_jobs_and_cancellation() {
        let root = temp_dir();
        let services = NativeServices::new(ResourcePaths::test(root.path()));
        let events = services.subscribe();
        let shared = Arc::clone(&services.mutations.shared);
        let guard = ActiveOperation::new(shared);
        assert_eq!(services.mutations.active_count(), 1);
        assert!(services.mutations.request_exit());
        services.mutations.cancel_all();
        drop(guard);
        assert_eq!(services.mutations.active_count(), 0);
        assert!(matches!(
            events
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            ServiceEvent::MutationIdle
        ));
    }

    #[test]
    fn permanent_delete_batch_reports_the_irreversible_completed_prefix() {
        let temp = temp_dir();
        let first = temp.path().join("first.txt");
        let missing = temp.path().join("missing.txt");
        fs::write(&first, "delete me").unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));

        let result = services
            .mutations
            .delete_permanently_batch(vec![(first.clone(), false), (missing.clone(), false)])
            .wait()
            .unwrap();

        assert_eq!(result.requested_items, 2);
        assert_eq!(result.deleted_items, 1);
        assert_eq!(
            result.failure.as_ref().map(|failure| &failure.path),
            Some(&missing)
        );
        assert!(!first.exists());
        assert_eq!(services.mutations.active_count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn permanent_delete_unlinks_links_without_following_them() {
        use std::os::unix::fs::symlink;
        let temp = temp_dir();
        let outside = temp.path().join("outside");
        let source = temp.path().join("source");
        fs::create_dir(&outside).unwrap();
        fs::create_dir(&source).unwrap();
        fs::write(outside.join("keep.txt"), "keep").unwrap();
        symlink(&outside, source.join("link")).unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));
        services
            .mutations
            .delete_permanently(source, true)
            .wait()
            .unwrap();
        assert_eq!(
            fs::read_to_string(outside.join("keep.txt")).unwrap(),
            "keep"
        );
    }
}
