use super::is_link_metadata;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
#[cfg(not(windows))]
use std::io::Write;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use uuid::Uuid;
use walkdir::WalkDir;

const COPY_BUFFER_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileOperationKind {
    Copy,
    Move,
    Trash,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictPolicy {
    #[default]
    Error,
    Rename,
    Replace,
    /// Name a target that already exists as a duplicate: Finder's "report
    /// copy.pdf" on macOS, Explorer's "report - Copy.pdf" elsewhere. For
    /// pasting items into the folder they came from, which never asks.
    Duplicate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationRequest {
    pub kind: FileOperationKind,
    pub sources: Vec<PathBuf>,
    pub destination: Option<PathBuf>,
    #[serde(default)]
    pub conflict_policy: ConflictPolicy,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationProgress {
    pub processed_entries: u64,
    pub total_entries: u64,
    pub processed_bytes: u64,
    pub total_bytes: u64,
    pub current_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationResult {
    pub processed_entries: u64,
    pub processed_bytes: u64,
    pub targets: Vec<PathBuf>,
    pub target_snapshots: Vec<FileTreeSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTreeSnapshot {
    entries: Vec<FileTreeEntrySnapshot>,
}

impl FileTreeSnapshot {
    pub fn estimated_bytes(&self) -> usize {
        self.entries.iter().fold(0, |total, entry| {
            total
                .saturating_add(std::mem::size_of::<FileTreeEntrySnapshot>())
                .saturating_add(entry.relative.as_os_str().len())
                .saturating_add(
                    entry
                        .link_target
                        .as_ref()
                        .map_or(0, |target| target.as_os_str().len()),
                )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileTreeEntrySnapshot {
    relative: PathBuf,
    is_directory: bool,
    len: u64,
    modified: Option<(u64, u32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    link_target: Option<PathBuf>,
}

#[derive(Debug)]
pub struct FileOperationFailure {
    pub error: io::Error,
    pub partial_result: FileOperationResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlannedKind {
    Directory,
    File,
    /// Copied as a link with the same target; never followed.
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedEntry {
    relative: PathBuf,
    kind: PlannedKind,
    len: u64,
    modified: Option<SystemTime>,
    link_target: Option<PathBuf>,
    /// Windows distinguishes file and directory symbolic links.
    link_is_directory: bool,
}

struct SourcePlan {
    source: PathBuf,
    canonical: PathBuf,
    entries: Vec<PlannedEntry>,
    total_bytes: u64,
    /// False when only the top-level item was inspected (same-volume moves are
    /// a single rename and never need the tree).
    complete: bool,
}

struct ProgressTracker<'a, F: FnMut(FileOperationProgress)> {
    progress: FileOperationProgress,
    notify: &'a mut F,
}

impl<F: FnMut(FileOperationProgress)> ProgressTracker<'_, F> {
    fn emit(&mut self, current_path: Option<PathBuf>) {
        self.progress.current_path = current_path;
        (self.notify)(self.progress.clone());
    }

    fn copied_bytes(&mut self, path: &Path, bytes: u64) {
        self.progress.processed_bytes = self.progress.processed_bytes.saturating_add(bytes);
        self.emit(Some(path.to_path_buf()));
    }

    fn completed_entry(&mut self, path: &Path) {
        self.progress.processed_entries = self.progress.processed_entries.saturating_add(1);
        self.emit(Some(path.to_path_buf()));
    }

    fn completed_plan(&mut self, plan: &SourcePlan) {
        self.progress.processed_entries = self
            .progress
            .processed_entries
            .saturating_add(plan.entries.len() as u64);
        self.progress.processed_bytes = self
            .progress
            .processed_bytes
            .saturating_add(plan.total_bytes);
        self.emit(Some(plan.source.clone()));
    }

    /// A move that turned out to cross volumes replaces its top-level plan
    /// with the full tree, so the totals grow by the difference.
    fn expanded_plan(&mut self, partial: &SourcePlan, full: &SourcePlan) {
        let entries = full.entries.len().saturating_sub(partial.entries.len()) as u64;
        let bytes = full.total_bytes.saturating_sub(partial.total_bytes);
        self.progress.total_entries = self.progress.total_entries.saturating_add(entries);
        self.progress.total_bytes = self.progress.total_bytes.saturating_add(bytes);
        self.emit(Some(full.source.clone()));
    }
}

pub fn perform_file_operation(
    request: FileOperationRequest,
    cancelled: &AtomicBool,
    on_progress: impl FnMut(FileOperationProgress),
) -> io::Result<FileOperationResult> {
    perform_file_operation_report(request, cancelled, on_progress).map_err(|failure| failure.error)
}

pub fn perform_file_operation_report(
    request: FileOperationRequest,
    cancelled: &AtomicBool,
    mut on_progress: impl FnMut(FileOperationProgress),
) -> Result<FileOperationResult, FileOperationFailure> {
    if request.sources.is_empty() {
        return Err(empty_operation_failure(io::Error::new(
            io::ErrorKind::InvalidInput,
            "at least one source is required",
        )));
    }

    if request.kind == FileOperationKind::Trash {
        return trash_sources(request.sources, cancelled, on_progress);
    }

    let destination = request.destination.ok_or_else(|| {
        empty_operation_failure(io::Error::new(
            io::ErrorKind::InvalidInput,
            "copy and move operations require a destination directory",
        ))
    })?;
    let destination_metadata =
        fs::symlink_metadata(&destination).map_err(empty_operation_failure)?;
    if is_link_metadata(&destination_metadata) || !destination_metadata.is_dir() {
        return Err(empty_operation_failure(io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination must be a real directory",
        )));
    }
    let destination_canonical = fs::canonicalize(&destination).map_err(empty_operation_failure)?;

    // Moves start from the top-level item only: a same-volume move is a single
    // rename, and the tree is walked only if the move has to copy.
    let plans: Vec<SourcePlan> = request
        .sources
        .iter()
        .map(|source| match request.kind {
            FileOperationKind::Move => plan_top_level(source),
            _ => plan_source(source),
        })
        .collect::<io::Result<_>>()
        .map_err(empty_operation_failure)?;
    validate_sources(&plans, &destination_canonical).map_err(empty_operation_failure)?;

    let total_entries = plans.iter().map(|plan| plan.entries.len() as u64).sum();
    let total_bytes = plans.iter().map(|plan| plan.total_bytes).sum();
    let mut tracker = ProgressTracker {
        progress: FileOperationProgress {
            processed_entries: 0,
            total_entries,
            processed_bytes: 0,
            total_bytes,
            current_path: None,
        },
        notify: &mut on_progress,
    };
    tracker.emit(None);

    let mut targets = Vec::with_capacity(plans.len());
    let mut target_snapshots = Vec::with_capacity(plans.len());
    for plan in &plans {
        if let Err(error) = check_cancelled(cancelled) {
            return Err(tracked_operation_failure(
                error,
                &tracker,
                targets,
                target_snapshots,
            ));
        }
        let Some(file_name) = plan.source.file_name() else {
            return Err(tracked_operation_failure(
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "cannot operate on filesystem root {}",
                        plan.source.display()
                    ),
                ),
                &tracker,
                targets,
                target_snapshots,
            ));
        };
        let requested_target = destination.join(file_name);
        let target = match resolve_target(&requested_target, request.conflict_policy) {
            Ok(target) => target,
            Err(error) => {
                return Err(tracked_operation_failure(
                    error,
                    &tracker,
                    targets,
                    target_snapshots,
                ));
            }
        };
        let same = match same_path(&plan.canonical, &target) {
            Ok(same) => same,
            Err(error) => {
                return Err(tracked_operation_failure(
                    error,
                    &tracker,
                    targets,
                    target_snapshots,
                ));
            }
        };
        if same {
            return Err(tracked_operation_failure(
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "source and destination are the same path",
                ),
                &tracker,
                targets,
                target_snapshots,
            ));
        }

        let result = match request.kind {
            FileOperationKind::Copy => copy_plan(
                plan,
                &target,
                request.conflict_policy,
                cancelled,
                &mut tracker,
            ),
            FileOperationKind::Move => move_plan(
                plan,
                &target,
                request.conflict_policy,
                cancelled,
                &mut tracker,
            ),
            FileOperationKind::Trash => unreachable!(),
        };
        if let Err(error) = result {
            return Err(tracked_operation_failure(
                error,
                &tracker,
                targets,
                target_snapshots,
            ));
        }
        target_snapshots.push(file_tree_snapshot(plan));
        targets.push(target);
    }

    tracker.emit(None);
    Ok(FileOperationResult {
        processed_entries: tracker.progress.processed_entries,
        processed_bytes: tracker.progress.processed_bytes,
        targets,
        target_snapshots,
    })
}

fn empty_operation_failure(error: io::Error) -> FileOperationFailure {
    FileOperationFailure {
        error,
        partial_result: FileOperationResult {
            processed_entries: 0,
            processed_bytes: 0,
            targets: Vec::new(),
            target_snapshots: Vec::new(),
        },
    }
}

fn tracked_operation_failure<F: FnMut(FileOperationProgress)>(
    error: io::Error,
    tracker: &ProgressTracker<'_, F>,
    targets: Vec<PathBuf>,
    target_snapshots: Vec<FileTreeSnapshot>,
) -> FileOperationFailure {
    FileOperationFailure {
        error,
        partial_result: FileOperationResult {
            processed_entries: tracker.progress.processed_entries,
            processed_bytes: tracker.progress.processed_bytes,
            targets,
            target_snapshots,
        },
    }
}

/// Items per Trash request. Bounds the osascript command line on macOS and
/// lets cancellation and progress take effect between batches.
const TRASH_BATCH_SIZE: usize = 64;

fn trash_sources(
    sources: Vec<PathBuf>,
    cancelled: &AtomicBool,
    mut on_progress: impl FnMut(FileOperationProgress),
) -> Result<FileOperationResult, FileOperationFailure> {
    check_cancelled(cancelled).map_err(empty_operation_failure)?;
    // The Trash moves each selected item as a unit, so only the items
    // themselves are inspected; links inside folders (and top-level links,
    // which are trashed as links) never block it.
    let mut locations = Vec::with_capacity(sources.len());
    for source in &sources {
        fs::symlink_metadata(source)
            .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", source.display())))
            .map_err(empty_operation_failure)?;
        if source.file_name().is_none() {
            return Err(empty_operation_failure(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot move {} to the Trash", source.display()),
            )));
        }
        locations.push(canonical_location(source).map_err(empty_operation_failure)?);
    }
    for (index, location) in locations.iter().enumerate() {
        if locations
            .iter()
            .skip(index + 1)
            .any(|other| path_starts_with(location, other) || path_starts_with(other, location))
        {
            return Err(empty_operation_failure(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sources must not overlap",
            )));
        }
    }

    let total = sources.len() as u64;
    let mut progress = FileOperationProgress {
        processed_entries: 0,
        total_entries: total,
        processed_bytes: 0,
        total_bytes: 0,
        current_path: None,
    };
    on_progress(progress.clone());
    let trashed_result = |processed_entries| FileOperationResult {
        processed_entries,
        processed_bytes: 0,
        targets: Vec::new(),
        target_snapshots: Vec::new(),
    };
    for batch in sources.chunks(TRASH_BATCH_SIZE) {
        if let Err(error) = check_cancelled(cancelled) {
            return Err(FileOperationFailure {
                error,
                partial_result: trashed_result(progress.processed_entries),
            });
        }
        progress.current_path = batch.first().cloned();
        on_progress(progress.clone());
        if let Err(error) = move_to_trash(batch, TrashPurpose::UserRequest) {
            // The platform APIs do not say which items were moved before the
            // failure, so ask the filesystem. Trashed items stay recoverable.
            let remaining: Vec<&PathBuf> = batch
                .iter()
                .filter(|path| fs::symlink_metadata(path).is_ok())
                .collect();
            if let Some(first) = remaining.first() {
                let moved = progress.processed_entries + (batch.len() - remaining.len()) as u64;
                let others = match remaining.len() - 1 {
                    0 => String::new(),
                    count => format!(" and {count} other item(s)"),
                };
                let not_attempted = total - progress.processed_entries - batch.len() as u64;
                let skipped = if not_attempted > 0 {
                    format!("; {not_attempted} remaining item(s) were not attempted")
                } else {
                    String::new()
                };
                return Err(FileOperationFailure {
                    error: io::Error::new(
                        error.kind(),
                        format!(
                            "Moved {moved} of {total} item(s) to the Trash; {}{others} could not be moved: {error}{skipped}. Items already in the Trash can be restored from there.",
                            first.display()
                        ),
                    ),
                    partial_result: trashed_result(moved),
                });
            }
            tracing::warn!(error = %error, "Trash reported an error but every item was moved");
        }
        progress.processed_entries += batch.len() as u64;
        progress.current_path = batch.last().cloned();
        on_progress(progress.clone());
    }
    progress.current_path = None;
    on_progress(progress);
    Ok(trashed_result(total))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrashPurpose {
    /// Items the user asked to trash: on macOS Finder performs the move so
    /// that "Put Back" works.
    UserRequest,
    /// An item displaced by Replace. It is kept recoverable, but Put Back
    /// cannot apply, so macOS uses NSFileManager (no Finder automation).
    ReplacedItem,
}

fn move_to_trash(paths: &[PathBuf], purpose: TrashPurpose) -> io::Result<()> {
    #[cfg(test)]
    return tests::fake_trash(paths, purpose);

    #[cfg(not(test))]
    {
        #[cfg(target_os = "macos")]
        let context = {
            use trash::macos::{DeleteMethod, TrashContextExtMacos};
            let mut context = trash::TrashContext::default();
            if purpose == TrashPurpose::ReplacedItem {
                context.set_delete_method(DeleteMethod::NsFileManager);
            }
            context
        };
        #[cfg(not(target_os = "macos"))]
        let context = {
            let _ = purpose;
            trash::TrashContext::default()
        };
        context
            .delete_all(paths)
            .map_err(|error| io::Error::other(trash_error_message(error)))
    }
}

#[cfg(not(test))]
fn trash_error_message(error: trash::Error) -> String {
    match error {
        trash::Error::Os { description, .. } | trash::Error::Unknown { description } => description,
        trash::Error::CouldNotAccess { target } => format!("could not access {target}"),
        error => error.to_string(),
    }
}

fn file_tree_snapshot(plan: &SourcePlan) -> FileTreeSnapshot {
    FileTreeSnapshot {
        entries: plan
            .entries
            .iter()
            .map(|entry| FileTreeEntrySnapshot {
                relative: entry.relative.clone(),
                is_directory: entry.kind == PlannedKind::Directory,
                len: entry.len,
                modified: entry
                    .modified
                    .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|duration| (duration.as_secs(), duration.subsec_nanos())),
                link_target: entry.link_target.clone(),
            })
            .collect(),
    }
}

pub fn path_matches_snapshot(path: &Path, expected: &FileTreeSnapshot) -> io::Result<bool> {
    Ok(file_tree_snapshot(&plan_source(path)?) == *expected)
}

fn plan_source(source: &Path) -> io::Result<SourcePlan> {
    let entries = snapshot_entries(source)?;
    let total_bytes = entries.iter().map(|entry| entry.len).sum();
    Ok(SourcePlan {
        source: source.to_path_buf(),
        canonical: canonical_location(source)?,
        entries,
        total_bytes,
        complete: true,
    })
}

fn plan_top_level(source: &Path) -> io::Result<SourcePlan> {
    let metadata = fs::symlink_metadata(source)?;
    let entry = planned_entry(PathBuf::new(), source, &metadata)?;
    Ok(SourcePlan {
        source: source.to_path_buf(),
        canonical: canonical_location(source)?,
        total_bytes: entry.len,
        complete: entry.kind != PlannedKind::Directory,
        entries: vec![entry],
    })
}

/// Canonical path of `path` itself: its parent is resolved, but a final
/// symbolic link is not followed.
fn canonical_location(path: &Path) -> io::Result<PathBuf> {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => {
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            Ok(fs::canonicalize(parent)?.join(name))
        }
        _ => fs::canonicalize(path),
    }
}

fn snapshot_entries(root: &Path) -> io::Result<Vec<PlannedEntry>> {
    // ponytail: retain one path manifest for safe move verification; stream it
    // only if million-entry directories show measurable memory pressure.
    let mut entries = Vec::new();
    let metadata = fs::symlink_metadata(root)?;
    if metadata.is_dir() && !is_link_metadata(&metadata) {
        // Links are recorded as entries and never descended into.
        for entry in WalkDir::new(root).follow_links(false) {
            let entry = entry.map_err(io::Error::other)?;
            let metadata = entry.path().symlink_metadata()?;
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(io::Error::other)?
                .to_path_buf();
            entries.push(planned_entry(relative, entry.path(), &metadata)?);
        }
    } else {
        entries.push(planned_entry(PathBuf::new(), root, &metadata)?);
    }

    entries.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(entries)
}

fn planned_entry(
    relative: PathBuf,
    path: &Path,
    metadata: &fs::Metadata,
) -> io::Result<PlannedEntry> {
    if is_link_metadata(metadata) {
        return Ok(PlannedEntry {
            relative,
            kind: PlannedKind::Symlink,
            len: 0,
            modified: None,
            link_target: Some(fs::read_link(path)?),
            link_is_directory: link_is_directory(metadata),
        });
    }
    let kind = if metadata.is_dir() {
        PlannedKind::Directory
    } else if metadata.is_file() {
        PlannedKind::File
    } else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "special filesystem entries are not supported: {}",
                path.display()
            ),
        ));
    };
    Ok(PlannedEntry {
        relative,
        kind,
        len: if kind == PlannedKind::File {
            metadata.len()
        } else {
            0
        },
        modified: if kind == PlannedKind::File {
            metadata.modified().ok()
        } else {
            None
        },
        link_target: None,
        link_is_directory: false,
    })
}

#[cfg(windows)]
fn link_is_directory(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::FileTypeExt;
    metadata.file_type().is_symlink_dir()
}

#[cfg(not(windows))]
fn link_is_directory(_metadata: &fs::Metadata) -> bool {
    false
}

fn validate_sources(plans: &[SourcePlan], destination: &Path) -> io::Result<()> {
    for (index, plan) in plans.iter().enumerate() {
        if path_starts_with(destination, &plan.canonical) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "destination {} is inside source {}",
                    destination.display(),
                    plan.source.display()
                ),
            ));
        }
        for other in plans.iter().skip(index + 1) {
            if path_starts_with(&plan.canonical, &other.canonical)
                || path_starts_with(&other.canonical, &plan.canonical)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "sources must not overlap",
                ));
            }
        }
    }
    Ok(())
}

/// The number the first alternative name gets when an item is kept beside
/// an existing one: Finder's "report 2.pdf" on macOS, Explorer's
/// "report (1).pdf" elsewhere.
pub const FIRST_CONFLICT_NUMBER: u32 = if cfg!(target_os = "macos") { 2 } else { 1 };

/// `name` numbered the platform's way, for keeping an item beside an existing
/// one: "report 2.pdf" on macOS (Finder), "report (2).pdf" elsewhere
/// (Explorer). With `preserve_extension` the number goes before the
/// extension; folders and extensionless names are numbered at the end.
pub fn numbered_name(name: &std::ffi::OsStr, number: u32, preserve_extension: bool) -> OsString {
    let suffix = if cfg!(target_os = "macos") {
        format!(" {number}")
    } else {
        format!(" ({number})")
    };
    let path = Path::new(name);
    let (stem, extension) = if preserve_extension {
        (path.file_stem().unwrap_or(name), path.extension())
    } else {
        (name, None)
    };
    let mut numbered = OsString::from(stem);
    numbered.push(suffix);
    if let Some(extension) = extension {
        numbered.push(".");
        numbered.push(extension);
    }
    numbered
}

/// `name` named the platform's way for a duplicate in the same folder:
/// "report copy.pdf", then "report copy 2.pdf" on macOS (Finder);
/// "report - Copy.pdf", then "report - Copy (2).pdf" elsewhere (Explorer).
/// `number` 1 is the first copy. A name that is already a copy is
/// duplicated from its original, so a copy of "report copy.pdf" becomes
/// "report copy 2.pdf" rather than "report copy copy.pdf". With
/// `preserve_extension` the suffix goes before the extension.
pub fn duplicate_name(name: &std::ffi::OsStr, number: u32, preserve_extension: bool) -> OsString {
    let path = Path::new(name);
    let (stem, extension) = if preserve_extension {
        (path.file_stem().unwrap_or(name), path.extension())
    } else {
        (name, None)
    };
    let mut duplicate = OsString::from(
        stem.to_str()
            .and_then(original_of_duplicate)
            .map_or(stem, std::ffi::OsStr::new),
    );
    duplicate.push(match (cfg!(target_os = "macos"), number) {
        (true, 1) => " copy".to_string(),
        (true, number) => format!(" copy {number}"),
        (false, 1) => " - Copy".to_string(),
        (false, number) => format!(" - Copy ({number})"),
    });
    if let Some(extension) = extension {
        duplicate.push(".");
        duplicate.push(extension);
    }
    duplicate
}

/// The name a duplicate was made from ("report" for "report copy 2"), or
/// `None` when `stem` is not a duplicate's name.
fn original_of_duplicate(stem: &str) -> Option<&str> {
    let marker = if cfg!(target_os = "macos") {
        " copy"
    } else {
        " - Copy"
    };
    let base = match stem.strip_suffix(marker) {
        Some(base) => base,
        None => {
            let (head, tail) = stem.rsplit_once(marker)?;
            let tail = tail.strip_prefix(' ')?;
            let digits = if cfg!(target_os = "macos") {
                tail
            } else {
                tail.strip_prefix('(')?.strip_suffix(')')?
            };
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            head
        }
    };
    (!base.is_empty()).then_some(base)
}

fn resolve_target(target: &Path, policy: ConflictPolicy) -> io::Result<PathBuf> {
    if !path_exists_no_follow(target)? {
        return Ok(target.to_path_buf());
    }
    match policy {
        ConflictPolicy::Error => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination already exists: {}", target.display()),
        )),
        ConflictPolicy::Replace => Ok(target.to_path_buf()),
        ConflictPolicy::Rename => {
            let name = target.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid target name")
            })?;
            for number in FIRST_CONFLICT_NUMBER.. {
                let candidate = target.with_file_name(numbered_name(name, number, true));
                if !path_exists_no_follow(&candidate)? {
                    return Ok(candidate);
                }
            }
            unreachable!()
        }
        ConflictPolicy::Duplicate => {
            let name = target.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid target name")
            })?;
            // Folders keep their whole name: "my.folder copy".
            let preserve_extension = !fs::symlink_metadata(target)?.is_dir();
            for number in 1.. {
                let candidate =
                    target.with_file_name(duplicate_name(name, number, preserve_extension));
                if !path_exists_no_follow(&candidate)? {
                    return Ok(candidate);
                }
            }
            unreachable!()
        }
    }
}

fn copy_plan<F: FnMut(FileOperationProgress)>(
    plan: &SourcePlan,
    target: &Path,
    policy: ConflictPolicy,
    cancelled: &AtomicBool,
    tracker: &mut ProgressTracker<'_, F>,
) -> io::Result<()> {
    let stage = temporary_sibling(target, "copy");
    let result = (|| {
        copy_to_stage(plan, &stage, cancelled, tracker)?;
        check_cancelled(cancelled)?;
        commit_stage(&stage, target, policy)
    })();
    if result.is_err() {
        let _ = remove_tree(&stage);
    }
    result
}

fn move_plan<F: FnMut(FileOperationProgress)>(
    plan: &SourcePlan,
    target: &Path,
    policy: ConflictPolicy,
    cancelled: &AtomicBool,
    tracker: &mut ProgressTracker<'_, F>,
) -> io::Result<()> {
    check_cancelled(cancelled)?;
    if policy != ConflictPolicy::Replace {
        match rename_noreplace(&plan.source, target) {
            Ok(()) => {
                tracker.completed_plan(plan);
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {}
            Err(error) => return Err(error),
        }
    }

    let stage = temporary_sibling(target, "move");
    if policy == ConflictPolicy::Replace {
        match rename_noreplace(&plan.source, &stage) {
            Ok(()) => {
                let commit =
                    check_cancelled(cancelled).and_then(|()| commit_stage(&stage, target, policy));
                if let Err(error) = commit {
                    return Err(restore_quarantine(&stage, &plan.source, error));
                }
                tracker.completed_plan(plan);
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {}
            Err(error) => return Err(error),
        }
    }

    // Crossing volumes means copying, which needs the whole tree.
    let full_plan;
    let plan = if plan.complete {
        plan
    } else {
        full_plan = plan_source(&plan.source)?;
        tracker.expanded_plan(plan, &full_plan);
        &full_plan
    };
    let quarantine = temporary_sibling(&plan.source, "source");
    let result = (|| {
        copy_to_stage(plan, &stage, cancelled, tracker)?;
        check_cancelled(cancelled)?;
        rename_noreplace(&plan.source, &quarantine)?;

        let precommit = (|| {
            if snapshot_entries(&quarantine)? != plan.entries {
                return Err(io::Error::other("source changed while it was being copied"));
            }
            verify_staged_copy(&quarantine, &stage, plan, cancelled)?;
            #[cfg(windows)]
            flush_staged_files(&stage, plan)?;
            check_cancelled(cancelled)?;
            commit_stage(&stage, target, policy)
        })();
        if let Err(error) = precommit {
            return Err(restore_quarantine(&quarantine, &plan.source, error));
        }
        if let Err(error) = remove_tree(&quarantine) {
            let error = io::Error::new(
                error.kind(),
                format!("destination committed but source cleanup failed: {error}"),
            );
            return Err(restore_quarantine(&quarantine, &plan.source, error));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = remove_tree(&stage);
    }
    result
}

fn copy_to_stage<F: FnMut(FileOperationProgress)>(
    plan: &SourcePlan,
    stage: &Path,
    cancelled: &AtomicBool,
    tracker: &mut ProgressTracker<'_, F>,
) -> io::Result<()> {
    let mut directories = Vec::new();
    for entry in &plan.entries {
        check_cancelled(cancelled)?;
        let source = if entry.relative.as_os_str().is_empty() {
            plan.source.clone()
        } else {
            plan.source.join(&entry.relative)
        };
        let destination = if entry.relative.as_os_str().is_empty() {
            stage.to_path_buf()
        } else {
            stage.join(&entry.relative)
        };
        ensure_entry_unchanged(&source, entry)?;
        if entry.kind != PlannedKind::Directory
            && let Some(parent) = destination.parent()
            && !parent.is_dir()
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("staged parent directory is missing: {}", parent.display()),
            ));
        }

        match entry.kind {
            PlannedKind::Directory => {
                create_directory_from_source(&source, &destination)?;
                let permissions = fs::symlink_metadata(&source)?.permissions();
                directories.push((source.clone(), destination, permissions));
            }
            PlannedKind::File => copy_file(&source, &destination, entry, cancelled, tracker)?,
            PlannedKind::Symlink => create_symlink(&source, entry, &destination)?,
        }
        tracker.completed_entry(&source);
    }
    // Folder permissions and ACLs are applied once their contents exist, deepest
    // first: a read-only source folder would otherwise block its own children.
    for (source, directory, permissions) in directories.into_iter().rev() {
        finish_directory_metadata(&source, &directory, permissions)?;
    }
    Ok(())
}

/// Recreates a symbolic link with the planned target without following it.
#[cfg(unix)]
fn create_symlink(_source: &Path, planned: &PlannedEntry, destination: &Path) -> io::Result<()> {
    let target = planned
        .link_target
        .as_deref()
        .ok_or_else(|| io::Error::other("planned link has no target"))?;
    std::os::unix::fs::symlink(target, destination)
}

#[cfg(windows)]
fn create_symlink(source: &Path, planned: &PlannedEntry, destination: &Path) -> io::Result<()> {
    const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
    let target = planned
        .link_target
        .as_deref()
        .ok_or_else(|| io::Error::other("planned link has no target"))?;
    let created = if planned.link_is_directory {
        std::os::windows::fs::symlink_dir(target, destination)
    } else {
        std::os::windows::fs::symlink_file(target, destination)
    };
    created.map_err(|error| {
        if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "cannot copy the symbolic link {}: Windows allows creating symbolic links only with Developer Mode enabled or administrator rights",
                    source.display()
                ),
            )
        } else {
            io::Error::new(
                error.kind(),
                format!("cannot copy the symbolic link {}: {error}", source.display()),
            )
        }
    })
}

#[cfg(not(any(unix, windows)))]
fn create_symlink(source: &Path, _planned: &PlannedEntry, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("cannot copy the symbolic link {}", source.display()),
    ))
}

#[cfg(windows)]
fn create_directory_from_source(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateDirectoryExW, FILE_ATTRIBUTE_REPARSE_POINT,
    };

    // A template folder with a reparse point (a cloud placeholder, for example)
    // would pass its reparse data on to the copy.
    if fs::symlink_metadata(source)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return fs::create_dir(destination);
    }
    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // A null security descriptor lets the copy inherit the destination's
    // permissions, as Explorer does.
    let created = unsafe {
        CreateDirectoryExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            std::ptr::null(),
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn create_directory_from_source(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir(destination)?;
    // Extended attributes only: the mode and ACLs are applied after the
    // folder's contents (see `finish_directory_metadata`).
    #[cfg(target_os = "macos")]
    copy_macos_path_metadata(source, destination, libc::COPYFILE_XATTR)?;
    #[cfg(not(target_os = "macos"))]
    let _ = source;
    Ok(())
}

#[cfg(target_os = "macos")]
fn finish_directory_metadata(
    source: &Path,
    destination: &Path,
    _permissions: fs::Permissions,
) -> io::Result<()> {
    copy_macos_path_metadata(
        source,
        destination,
        libc::COPYFILE_STAT | libc::COPYFILE_ACL,
    )
}

#[cfg(not(target_os = "macos"))]
fn finish_directory_metadata(
    _source: &Path,
    destination: &Path,
    permissions: fs::Permissions,
) -> io::Result<()> {
    fs::set_permissions(destination, permissions)
}

#[cfg(target_os = "macos")]
fn copy_macos_path_metadata(
    source: &Path,
    destination: &Path,
    flags: libc::copyfile_flags_t,
) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination path contains NUL")
    })?;
    let result = unsafe {
        libc::copyfile(
            source.as_ptr(),
            destination.as_ptr(),
            std::ptr::null_mut(),
            flags | libc::COPYFILE_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn windows_streams(path: &Path) -> io::Result<Vec<(OsString, u64)>> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Foundation::{ERROR_HANDLE_EOF, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindFirstStreamW, FindNextStreamW, WIN32_FIND_STREAM_DATA,
    };

    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut data: WIN32_FIND_STREAM_DATA = unsafe { std::mem::zeroed() };
    let handle = unsafe {
        FindFirstStreamW(
            path.as_ptr(),
            0,
            (&mut data as *mut WIN32_FIND_STREAM_DATA).cast(),
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    let mut streams = Vec::new();
    let result = loop {
        let length = data
            .cStreamName
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(data.cStreamName.len());
        streams.push((
            OsString::from_wide(&data.cStreamName[..length]),
            data.StreamSize.max(0) as u64,
        ));

        let next =
            unsafe { FindNextStreamW(handle, (&mut data as *mut WIN32_FIND_STREAM_DATA).cast()) };
        if next != 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_HANDLE_EOF as i32) {
            break Ok(());
        }
        break Err(error);
    };
    unsafe {
        FindClose(handle);
    }
    result?;
    streams.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(streams)
}

#[cfg(windows)]
fn path_with_stream(path: &Path, stream: &std::ffi::OsStr) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(stream);
    PathBuf::from(value)
}

#[cfg(windows)]
fn compare_windows_stream(
    source: &Path,
    destination: &Path,
    len: u64,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    let mut source_file = open_source_file(source)?;
    let mut destination_file = open_source_file(destination)?;
    let mut source_buffer = vec![0; COPY_BUFFER_SIZE];
    let mut destination_buffer = vec![0; COPY_BUFFER_SIZE];
    let mut remaining = len;
    while remaining > 0 {
        check_cancelled(cancelled)?;
        let length = remaining.min(COPY_BUFFER_SIZE as u64) as usize;
        source_file.read_exact(&mut source_buffer[..length])?;
        destination_file.read_exact(&mut destination_buffer[..length])?;
        if source_buffer[..length] != destination_buffer[..length] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("alternate stream mismatch for {}", source.display()),
            ));
        }
        remaining -= length as u64;
    }
    Ok(())
}

/// Copies inherit the destination's permissions (as in Explorer), so only the
/// alternate data streams are compared.
#[cfg(windows)]
fn verify_windows_metadata(
    source: &Path,
    destination: &Path,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    let source_streams = windows_streams(source)?;
    let destination_streams = windows_streams(destination)?;
    if source_streams != destination_streams {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "alternate stream manifest mismatch for {}",
                source.display()
            ),
        ));
    }
    for (name, len) in source_streams {
        if name == "::$DATA" {
            continue;
        }
        compare_windows_stream(
            &path_with_stream(source, &name),
            &path_with_stream(destination, &name),
            len,
            cancelled,
        )?;
    }
    Ok(())
}

/// CopyFileEx leaves written data in the system cache. Flush the staged files
/// before a move deletes its source so a crash or power loss cannot lose both
/// copies. (The Unix copy path already syncs every file it writes.)
#[cfg(windows)]
fn flush_staged_files(stage: &Path, plan: &SourcePlan) -> io::Result<()> {
    for entry in &plan.entries {
        if entry.kind != PlannedKind::File {
            continue;
        }
        let path = if entry.relative.as_os_str().is_empty() {
            stage.to_path_buf()
        } else {
            stage.join(&entry.relative)
        };
        flush_file(&path)?;
    }
    Ok(())
}

#[cfg(windows)]
fn flush_file(path: &Path) -> io::Result<()> {
    // FlushFileBuffers needs write access, which the read-only attribute
    // (copied by CopyFileEx) denies; lift it on the staged copy meanwhile.
    let permissions = fs::symlink_metadata(path)?.permissions();
    let read_only = permissions.readonly();
    if read_only {
        let mut writable = permissions.clone();
        #[allow(clippy::permissions_set_readonly_false)]
        writable.set_readonly(false);
        fs::set_permissions(path, writable)?;
    }
    let flushed = OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all());
    if read_only {
        fs::set_permissions(path, permissions)?;
    }
    flushed
}

#[cfg(windows)]
fn copy_file<F: FnMut(FileOperationProgress)>(
    source: &Path,
    destination: &Path,
    planned: &PlannedEntry,
    cancelled: &AtomicBool,
    tracker: &mut ProgressTracker<'_, F>,
) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::CopyFileExW;

    struct CopyProgress<'a> {
        cancelled: &'a AtomicBool,
        reported_bytes: u64,
        planned_bytes: u64,
        report: &'a mut dyn FnMut(u64),
    }

    unsafe extern "system" fn progress_callback(
        _total_file_size: i64,
        total_bytes_transferred: i64,
        _stream_size: i64,
        _stream_bytes_transferred: i64,
        _stream_number: u32,
        _callback_reason: u32,
        _source_handle: windows_sys::Win32::Foundation::HANDLE,
        _destination_handle: windows_sys::Win32::Foundation::HANDLE,
        data: *const core::ffi::c_void,
    ) -> u32 {
        let context = unsafe { &mut *(data as *mut CopyProgress<'_>) };
        let transferred = (total_bytes_transferred.max(0) as u64).min(context.planned_bytes);
        if transferred > context.reported_bytes {
            (context.report)(transferred - context.reported_bytes);
            context.reported_bytes = transferred;
        }
        if context.cancelled.load(Ordering::Relaxed) {
            1 // PROGRESS_CANCEL: CopyFileEx removes the partial destination.
        } else {
            0 // PROGRESS_CONTINUE
        }
    }

    ensure_entry_unchanged(source, planned)?;
    check_cancelled(cancelled)?;
    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut report = |bytes| tracker.copied_bytes(source, bytes);
    let mut context = CopyProgress {
        cancelled,
        reported_bytes: 0,
        planned_bytes: planned.len,
        report: &mut report,
    };

    let copied = unsafe {
        CopyFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            Some(progress_callback),
            (&mut context as *mut CopyProgress<'_>).cast(),
            std::ptr::null_mut(),
            1, // COPY_FILE_FAIL_IF_EXISTS
        )
    };
    if copied == 0 {
        let error = if cancelled.load(Ordering::Relaxed) {
            io::Error::new(io::ErrorKind::Interrupted, "file operation cancelled")
        } else {
            io::Error::last_os_error()
        };
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    if context.reported_bytes < planned.len {
        (context.report)(planned.len - context.reported_bytes);
    }

    ensure_entry_unchanged(source, planned)?;
    let destination_len = fs::symlink_metadata(destination)?.len();
    if destination_len != planned.len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("copied size mismatch for {}", source.display()),
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn copy_file<F: FnMut(FileOperationProgress)>(
    source: &Path,
    destination: &Path,
    planned: &PlannedEntry,
    cancelled: &AtomicBool,
    tracker: &mut ProgressTracker<'_, F>,
) -> io::Result<()> {
    let mut source_file = open_source_file(source)?;
    let opened_metadata = source_file.metadata()?;
    ensure_metadata_unchanged(source, &opened_metadata, planned)?;
    let mut destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut buffer = vec![0; COPY_BUFFER_SIZE];
    let mut copied = 0;
    loop {
        check_cancelled(cancelled)?;
        let read = source_file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        destination_file.write_all(&buffer[..read])?;
        copied += read as u64;
        tracker.copied_bytes(source, read as u64);
    }
    if let Some(modified) = planned.modified {
        destination_file.set_times(fs::FileTimes::new().set_modified(modified))?;
    }
    #[cfg(target_os = "macos")]
    copy_macos_metadata(&source_file, &destination_file)?;
    destination_file.set_permissions(opened_metadata.permissions())?;
    destination_file.sync_all()?;
    ensure_entry_unchanged(source, planned)?;

    let destination_len = fs::symlink_metadata(destination)?.len();
    if copied != planned.len || destination_len != planned.len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("copied size mismatch for {}", source.display()),
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn copy_macos_metadata(source: &File, destination: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let result = unsafe {
        libc::fcopyfile(
            source.as_raw_fd(),
            destination.as_raw_fd(),
            std::ptr::null_mut(),
            libc::COPYFILE_METADATA,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn open_source_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn open_source_file(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_source_file(path: &Path) -> io::Result<File> {
    File::open(path)
}

fn ensure_entry_unchanged(path: &Path, planned: &PlannedEntry) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if planned.kind == PlannedKind::Symlink {
        if !is_link_metadata(&metadata) || Some(fs::read_link(path)?) != planned.link_target {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("source changed while copying: {}", path.display()),
            ));
        }
        return Ok(());
    }
    if is_link_metadata(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("source became a link: {}", path.display()),
        ));
    }
    ensure_metadata_unchanged(path, &metadata, planned)
}

fn ensure_metadata_unchanged(
    path: &Path,
    metadata: &fs::Metadata,
    planned: &PlannedEntry,
) -> io::Result<()> {
    let actual_kind = if metadata.is_dir() {
        PlannedKind::Directory
    } else if metadata.is_file() {
        PlannedKind::File
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("source type changed: {}", path.display()),
        ));
    };
    let changed = actual_kind != planned.kind
        || (actual_kind == PlannedKind::File
            && (metadata.len() != planned.len || metadata.modified().ok() != planned.modified));
    if changed {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("source changed while copying: {}", path.display()),
        ))
    } else {
        Ok(())
    }
}

fn verify_staged_copy(
    source_root: &Path,
    staged_root: &Path,
    plan: &SourcePlan,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    let mut source_buffer = vec![0; COPY_BUFFER_SIZE];
    let mut staged_buffer = vec![0; COPY_BUFFER_SIZE];
    for entry in &plan.entries {
        check_cancelled(cancelled)?;
        if entry.kind == PlannedKind::Directory {
            continue;
        }
        let source = if entry.relative.as_os_str().is_empty() {
            source_root.to_path_buf()
        } else {
            source_root.join(&entry.relative)
        };
        let staged = if entry.relative.as_os_str().is_empty() {
            staged_root.to_path_buf()
        } else {
            staged_root.join(&entry.relative)
        };
        ensure_entry_unchanged(&source, entry)?;
        if entry.kind == PlannedKind::Symlink {
            // Links are compared by target; their targets are never opened.
            ensure_entry_unchanged(&staged, entry).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("staged link mismatch for {}", staged.display()),
                )
            })?;
            continue;
        }
        let staged_metadata = fs::symlink_metadata(&staged)?;
        let staged_kind = if staged_metadata.is_dir() {
            PlannedKind::Directory
        } else if staged_metadata.is_file() {
            PlannedKind::File
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("staged type mismatch for {}", staged.display()),
            ));
        };
        if staged_kind != entry.kind
            || (staged_kind == PlannedKind::File && staged_metadata.len() != entry.len)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("staged metadata mismatch for {}", staged.display()),
            ));
        }

        #[cfg(windows)]
        verify_windows_metadata(&source, &staged, cancelled)?;

        let mut source_file = open_source_file(&source)?;
        let mut staged_file = open_source_file(&staged)?;
        let mut remaining = entry.len;
        while remaining > 0 {
            check_cancelled(cancelled)?;
            let length = remaining.min(COPY_BUFFER_SIZE as u64) as usize;
            source_file.read_exact(&mut source_buffer[..length])?;
            staged_file.read_exact(&mut staged_buffer[..length])?;
            if source_buffer[..length] != staged_buffer[..length] {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("copied content mismatch for {}", source.display()),
                ));
            }
            remaining -= length as u64;
        }
    }
    Ok(())
}

fn restore_quarantine(quarantine: &Path, source: &Path, cause: io::Error) -> io::Error {
    match rename_noreplace(quarantine, source) {
        Ok(()) => cause,
        Err(restore_error) => io::Error::new(
            restore_error.kind(),
            format!(
                "{cause}; source recovery also failed, data remains at {}: {restore_error}",
                quarantine.display()
            ),
        ),
    }
}

fn commit_stage(stage: &Path, target: &Path, policy: ConflictPolicy) -> io::Result<()> {
    if policy != ConflictPolicy::Replace || !path_exists_no_follow(target)? {
        return rename_noreplace(stage, target);
    }

    let backup = temporary_sibling(target, "backup");
    rename_noreplace(target, &backup)?;
    if let Err(error) = rename_noreplace(stage, target) {
        return match rename_noreplace(&backup, target) {
            Ok(()) => Err(error),
            Err(restore_error) => Err(io::Error::new(
                restore_error.kind(),
                format!(
                    "{error}; destination recovery also failed, backup remains at {}: {restore_error}",
                    backup.display()
                ),
            )),
        };
    }
    discard_replaced_item(&backup, target);
    Ok(())
}

/// Moves an item displaced by Replace to the Trash so the user can still
/// recover it, falling back to deletion where there is no Trash (some network
/// and removable volumes). The replacement itself has already committed.
fn discard_replaced_item(backup: &Path, target: &Path) {
    // Trash it under its original name rather than the hidden backup name,
    // via a private holder folder next to the target.
    let holder = temporary_sibling(target, "replaced");
    let named = target.file_name().map(|name| holder.join(name));
    let trashed_path = match named {
        Some(named)
            if fs::create_dir(&holder).is_ok() && rename_noreplace(backup, &named).is_ok() =>
        {
            named
        }
        _ => {
            let _ = fs::remove_dir(&holder);
            backup.to_path_buf()
        }
    };
    if let Err(trash_error) = move_to_trash(
        std::slice::from_ref(&trashed_path),
        TrashPurpose::ReplacedItem,
    ) {
        tracing::warn!(
            path = ?target,
            error = %trash_error,
            "Replaced item could not be moved to the Trash; deleting it"
        );
        if let Err(error) = remove_tree(&trashed_path) {
            tracing::warn!(
                path = ?trashed_path,
                error = %error,
                "Replacement committed; preserving backup after cleanup failure"
            );
            return;
        }
    }
    if trashed_path != backup {
        let _ = fs::remove_dir(&holder);
    }
}

fn remove_path(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if is_link_metadata(&metadata) {
        // Windows removes directory links with RemoveDirectory.
        fs::remove_file(path).or_else(|error| fs::remove_dir(path).map_err(|_| error))
    } else if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Removes a staged or quarantined tree. Copies of read-only folders cannot
/// have their contents unlinked, so owner access is restored and the removal
/// retried; only trees Explorie is about to delete anyway pass through here.
fn remove_tree(path: &Path) -> io::Result<()> {
    match remove_path(path) {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            make_directories_writable(path);
            remove_path(path)
        }
        result => result,
    }
}

#[cfg(unix)]
fn make_directories_writable(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_dir() {
            continue;
        }
        if let Ok(metadata) = entry.metadata() {
            let mode = metadata.permissions().mode();
            if mode & 0o700 != 0o700 {
                let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode | 0o700));
            }
        }
    }
}

#[cfg(not(unix))]
fn make_directories_writable(_root: &Path) {}

fn temporary_sibling(path: &Path, purpose: &str) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".explorie-{purpose}-{}", Uuid::new_v4()))
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "file operation cancelled",
        ))
    } else {
        Ok(())
    }
}

fn same_path(source_canonical: &Path, target: &Path) -> io::Result<bool> {
    if !path_exists_no_follow(target)? {
        return Ok(false);
    }
    Ok(paths_equal(source_canonical, &canonical_location(target)?))
}

/// Existence without following a final link: a dangling link still occupies
/// its name.
fn path_exists_no_follow(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn path_starts_with(path: &Path, prefix: &Path) -> bool {
    let mut path = path.components();
    let mut prefix = prefix.components();
    loop {
        match prefix.next() {
            None => return true,
            Some(prefix) => match path.next() {
                Some(path) if components_equal(path.as_os_str(), prefix.as_os_str()) => {}
                _ => return false,
            },
        }
    }
}

/// Whether `left` and `right` name the same directory once links are
/// resolved (case-insensitively on Windows). Paths that cannot be resolved
/// are compared as given.
pub fn same_directory(left: &Path, right: &Path) -> bool {
    let resolve = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    paths_equal(&resolve(left), &resolve(right))
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    let mut left = left.components();
    let mut right = right.components();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(left), Some(right)) if components_equal(left.as_os_str(), right.as_os_str()) => {}
            _ => return false,
        }
    }
}

#[cfg(windows)]
fn components_equal(left: &std::ffi::OsStr, right: &std::ffi::OsStr) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(windows))]
fn components_equal(left: &std::ffi::OsStr, right: &std::ffi::OsStr) -> bool {
    left == right
}

#[cfg(windows)]
pub fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

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
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    #[cfg(test)]
    if let Some(result) = tests::rename_override(source, destination) {
        return result;
    }
    let source_c = CString::new(source.as_os_str().as_bytes())?;
    let destination_c = CString::new(destination.as_os_str().as_bytes())?;
    let result =
        unsafe { libc::renamex_np(source_c.as_ptr(), destination_c.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if exclusive_rename_unsupported(&error) {
        rename_noreplace_fallback(source, destination)
    } else {
        Err(error)
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    #[cfg(test)]
    if let Some(result) = tests::rename_override(source, destination) {
        return result;
    }
    let source_c = CString::new(source.as_os_str().as_bytes())?;
    let destination_c = CString::new(destination.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source_c.as_ptr(),
            libc::AT_FDCWD,
            destination_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if exclusive_rename_unsupported(&error) {
        rename_noreplace_fallback(source, destination)
    } else {
        Err(error)
    }
}

/// exFAT, FAT and some SMB servers reject `RENAME_EXCL` with ENOTSUP; Linux
/// filesystems without `RENAME_NOREPLACE` return EINVAL (ENOSYS on old kernels).
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
fn exclusive_rename_unsupported(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|code| {
        code == libc::ENOTSUP
            || code == libc::EOPNOTSUPP
            || code == libc::EINVAL
            || code == libc::ENOSYS
    })
}

/// No-replace rename for filesystems without an exclusive rename primitive.
///
/// Files are hard-linked to the destination and then unlinked from the source:
/// `link` fails atomically when the destination exists, so the no-replace
/// guarantee holds wherever hard links work. Directories, symbolic links and
/// filesystems without hard links (exFAT, FAT) use an existence check followed
/// by a plain `rename`. That leaves a window of a few syscalls in which an
/// entry created concurrently at the destination could be replaced (a file, or
/// an empty directory when renaming a directory); nothing in Explorie races
/// its own staging paths, so only an external writer can hit it.
#[cfg(unix)]
fn rename_noreplace_fallback(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_file() {
        match fs::hard_link(source, destination) {
            Ok(()) => {
                return fs::remove_file(source).inspect_err(|_| {
                    let _ = fs::remove_file(destination);
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Err(error),
            // No hard-link support (ENOTSUP/EPERM/EMLINK…): use check-then-rename.
            Err(_) => {}
        }
    }
    match fs::symlink_metadata(destination) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination already exists: {}", destination.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::rename(source, destination),
        Err(error) => Err(error),
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "linux", target_os = "android"))
))]
pub fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(test)]
    if let Some(result) = tests::rename_override(source, destination) {
        return result;
    }
    rename_noreplace_fallback(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::AtomicBool;
    use tempfile::tempdir;

    /// The name Keep Both gives the first copy of `name`.
    fn kept(name: &str) -> OsString {
        numbered_name(std::ffi::OsStr::new(name), FIRST_CONFLICT_NUMBER, true)
    }

    #[test]
    fn kept_copies_are_numbered_the_platform_file_managers_way() {
        let names: Vec<_> = ["report.pdf", "source", ".profile", "archive.tar.gz"]
            .into_iter()
            .map(kept)
            .collect();
        if cfg!(target_os = "macos") {
            assert_eq!(
                names,
                ["report 2.pdf", "source 2", ".profile 2", "archive.tar 2.gz"]
            );
        } else {
            assert_eq!(
                names,
                [
                    "report (1).pdf",
                    "source (1)",
                    ".profile (1)",
                    "archive.tar (1).gz"
                ]
            );
        }
        assert_eq!(
            numbered_name(std::ffi::OsStr::new("my.folder"), 3, false),
            if cfg!(target_os = "macos") {
                "my.folder 3"
            } else {
                "my.folder (3)"
            }
        );
    }

    #[test]
    fn keep_both_takes_the_next_free_number() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("report.pdf");
        let destination = temp.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(destination.join("report.pdf"), b"old").unwrap();
        fs::write(destination.join(kept("report.pdf")), b"older copy").unwrap();

        let result = perform_file_operation(
            request(
                FileOperationKind::Copy,
                &source,
                &destination,
                ConflictPolicy::Rename,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let expected = destination.join(numbered_name(
            std::ffi::OsStr::new("report.pdf"),
            FIRST_CONFLICT_NUMBER + 1,
            true,
        ));
        assert_eq!(result.targets, vec![expected.clone()]);
        assert_eq!(fs::read(expected).unwrap(), b"new");
    }

    #[test]
    fn duplicates_are_named_the_platform_file_managers_way() {
        let name = |name: &str, number: u32, preserve_extension: bool| {
            duplicate_name(std::ffi::OsStr::new(name), number, preserve_extension)
        };
        if cfg!(target_os = "macos") {
            assert_eq!(name("report.pdf", 1, true), "report copy.pdf");
            assert_eq!(name("report.pdf", 2, true), "report copy 2.pdf");
            assert_eq!(name("my.folder", 1, false), "my.folder copy");
            assert_eq!(name("archive.tar.gz", 1, true), "archive.tar copy.gz");
            // Duplicating a duplicate continues its sequence.
            assert_eq!(name("report copy.pdf", 2, true), "report copy 2.pdf");
            assert_eq!(name("report copy 2.pdf", 3, true), "report copy 3.pdf");
            assert_eq!(name("copy.txt", 1, true), "copy copy.txt");
            assert_eq!(
                name("photo copy two.jpg", 1, true),
                "photo copy two copy.jpg"
            );
        } else {
            assert_eq!(name("report.pdf", 1, true), "report - Copy.pdf");
            assert_eq!(name("report.pdf", 2, true), "report - Copy (2).pdf");
            assert_eq!(name("my.folder", 1, false), "my.folder - Copy");
            assert_eq!(name("report - Copy.pdf", 2, true), "report - Copy (2).pdf");
            assert_eq!(
                name("report - Copy (2).pdf", 3, true),
                "report - Copy (3).pdf"
            );
        }
    }

    #[test]
    fn copying_into_the_same_folder_makes_numbered_duplicates() {
        let temp = tempdir().unwrap();
        let folder = temp.path();
        let note = folder.join("note.txt");
        let subfolder = folder.join("folder");
        fs::write(&note, b"note").unwrap();
        fs::create_dir(&subfolder).unwrap();
        fs::write(subfolder.join("inside.txt"), b"inside").unwrap();

        let duplicate = |source: &Path| {
            perform_file_operation(
                request(
                    FileOperationKind::Copy,
                    source,
                    folder,
                    ConflictPolicy::Duplicate,
                ),
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap()
            .targets
        };
        let first = folder.join(duplicate_name("note.txt".as_ref(), 1, true));
        let second = folder.join(duplicate_name("note.txt".as_ref(), 2, true));
        let folder_copy = folder.join(duplicate_name("folder".as_ref(), 1, false));
        assert_eq!(duplicate(&note), vec![first.clone()]);
        assert_eq!(duplicate(&note), vec![second.clone()]);
        assert_eq!(duplicate(&subfolder), vec![folder_copy.clone()]);
        assert_eq!(fs::read(&first).unwrap(), b"note");
        assert_eq!(fs::read(&second).unwrap(), b"note");
        assert_eq!(fs::read(folder_copy.join("inside.txt")).unwrap(), b"inside");
        assert_eq!(fs::read(&note).unwrap(), b"note", "the original stays");
    }

    /// How `rename_noreplace` behaves on the current test thread.
    #[cfg(unix)]
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum RenameMode {
        Native,
        /// As on exFAT/FAT: no exclusive rename primitive.
        Fallback,
        /// Renames to another directory fail as if it were on another volume.
        CrossDevice,
    }

    #[cfg(unix)]
    thread_local! {
        static RENAME_MODE: std::cell::Cell<RenameMode> =
            const { std::cell::Cell::new(RenameMode::Native) };
    }

    #[cfg(unix)]
    pub(super) fn rename_override(source: &Path, destination: &Path) -> Option<io::Result<()>> {
        match RENAME_MODE.with(std::cell::Cell::get) {
            RenameMode::Native => None,
            RenameMode::Fallback => Some(rename_noreplace_fallback(source, destination)),
            RenameMode::CrossDevice => (source.parent() != destination.parent())
                .then(|| Err(io::Error::from(io::ErrorKind::CrossesDevices))),
        }
    }

    #[cfg(unix)]
    struct RenameModeGuard;

    #[cfg(unix)]
    impl RenameModeGuard {
        fn set(mode: RenameMode) -> Self {
            RENAME_MODE.with(|current| current.set(mode));
            Self
        }
    }

    #[cfg(unix)]
    impl Drop for RenameModeGuard {
        fn drop(&mut self) {
            RENAME_MODE.with(|current| current.set(RenameMode::Native));
        }
    }

    /// Stand-in for the platform Trash so tests never touch the real one.
    struct FakeTrash {
        directory: PathBuf,
        calls: Vec<(Vec<PathBuf>, TrashPurpose)>,
        fail_on: Option<PathBuf>,
        moved: usize,
    }

    thread_local! {
        static FAKE_TRASH: RefCell<Option<FakeTrash>> = const { RefCell::new(None) };
    }

    /// Without an installed fake the Trash is unavailable, which exercises the
    /// deletion fallback of Replace.
    pub(super) fn fake_trash(paths: &[PathBuf], purpose: TrashPurpose) -> io::Result<()> {
        FAKE_TRASH.with(|fake| {
            let mut fake = fake.borrow_mut();
            let Some(fake) = fake.as_mut() else {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "the Trash is unavailable in unit tests",
                ));
            };
            fake.calls.push((paths.to_vec(), purpose));
            for path in paths {
                if fake.fail_on.as_deref() == Some(path.as_path()) {
                    return Err(io::Error::other("simulated Trash failure"));
                }
                let name = path.file_name().unwrap().to_string_lossy();
                fs::rename(path, fake.directory.join(format!("{}-{name}", fake.moved)))?;
                fake.moved += 1;
            }
            Ok(())
        })
    }

    struct FakeTrashGuard;

    impl FakeTrashGuard {
        fn install(directory: &Path, fail_on: Option<PathBuf>) -> Self {
            fs::create_dir_all(directory).unwrap();
            FAKE_TRASH.with(|fake| {
                *fake.borrow_mut() = Some(FakeTrash {
                    directory: directory.to_path_buf(),
                    calls: Vec::new(),
                    fail_on,
                    moved: 0,
                });
            });
            Self
        }

        fn calls(&self) -> Vec<(Vec<PathBuf>, TrashPurpose)> {
            FAKE_TRASH.with(|fake| fake.borrow().as_ref().unwrap().calls.clone())
        }
    }

    impl Drop for FakeTrashGuard {
        fn drop(&mut self) {
            FAKE_TRASH.with(|fake| *fake.borrow_mut() = None);
        }
    }

    fn trash_request(sources: Vec<PathBuf>) -> FileOperationRequest {
        FileOperationRequest {
            kind: FileOperationKind::Trash,
            sources,
            destination: None,
            conflict_policy: ConflictPolicy::Error,
        }
    }

    fn explorie_leftovers(directory: &Path) -> Vec<String> {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".explorie-"))
            .collect()
    }

    fn request(
        kind: FileOperationKind,
        source: &Path,
        destination: &Path,
        conflict_policy: ConflictPolicy,
    ) -> FileOperationRequest {
        FileOperationRequest {
            kind,
            sources: vec![source.to_path_buf()],
            destination: Some(destination.to_path_buf()),
            conflict_policy,
        }
    }

    #[test]
    fn copy_directory_reports_progress_and_preserves_content() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir(&destination).unwrap();
        let source_file = source.join("nested/data.txt");
        fs::write(&source_file, b"content").unwrap();
        let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        OpenOptions::new()
            .write(true)
            .open(&source_file)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let mut progress = Vec::new();

        let result = perform_file_operation(
            request(
                FileOperationKind::Copy,
                &source,
                &destination,
                ConflictPolicy::Error,
            ),
            &AtomicBool::new(false),
            |update| progress.push(update),
        )
        .unwrap();

        assert_eq!(
            fs::read(destination.join("source/nested/data.txt")).unwrap(),
            b"content"
        );
        assert_eq!(result.processed_bytes, 7);
        assert_eq!(progress.last().unwrap().processed_bytes, 7);
        assert_eq!(
            fs::metadata(destination.join("source/nested/data.txt"))
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
        assert!(path_matches_snapshot(&result.targets[0], &result.target_snapshots[0]).unwrap());
        fs::write(
            destination.join("source/nested/data.txt"),
            b"changed contents",
        )
        .unwrap();
        assert!(!path_matches_snapshot(&result.targets[0], &result.target_snapshots[0]).unwrap());
    }

    #[test]
    fn partial_batch_failure_reports_completed_targets_for_unresolved_retry() {
        let temp = tempdir().unwrap();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        let destination = temp.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        fs::write(destination.join("second.txt"), b"existing").unwrap();
        let request = FileOperationRequest {
            kind: FileOperationKind::Copy,
            sources: vec![first.clone(), second.clone()],
            destination: Some(destination.clone()),
            conflict_policy: ConflictPolicy::Error,
        };

        let failure =
            perform_file_operation_report(request.clone(), &AtomicBool::new(false), |_| {})
                .unwrap_err();
        assert_eq!(failure.error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            failure.partial_result.targets,
            vec![destination.join("first.txt")]
        );
        assert_eq!(failure.partial_result.target_snapshots.len(), 1);
        assert_eq!(fs::read(destination.join("first.txt")).unwrap(), b"first");
        assert_eq!(
            fs::read(destination.join("second.txt")).unwrap(),
            b"existing"
        );

        fs::remove_file(destination.join("second.txt")).unwrap();
        let completed_sources = failure.partial_result.targets.len();
        let retry = FileOperationRequest {
            sources: request.sources[completed_sources..].to_vec(),
            ..request
        };
        let result = perform_file_operation(retry, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(result.targets, vec![destination.join("second.txt")]);
        assert_eq!(fs::read(destination.join("second.txt")).unwrap(), b"second");
    }

    #[test]
    fn cancelled_replace_leaves_existing_destination_untouched() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("item.txt");
        let destination = temp.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(destination.join("item.txt"), b"old").unwrap();

        let result = perform_file_operation(
            request(
                FileOperationKind::Copy,
                &source,
                &destination,
                ConflictPolicy::Replace,
            ),
            &AtomicBool::new(true),
            |_| {},
        );

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(fs::read(destination.join("item.txt")).unwrap(), b"old");
        assert_eq!(fs::read(source).unwrap(), b"new");
    }

    #[test]
    fn mid_copy_cancellation_removes_stage_and_preserves_source() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("large.bin");
        let destination = temp.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, vec![0x5a; COPY_BUFFER_SIZE * 8]).unwrap();
        let cancelled = AtomicBool::new(false);

        let result = perform_file_operation(
            request(
                FileOperationKind::Copy,
                &source,
                &destination,
                ConflictPolicy::Error,
            ),
            &cancelled,
            |progress| {
                if progress.processed_bytes > 0 {
                    cancelled.store(true, Ordering::Relaxed);
                }
            },
        );

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(
            fs::metadata(&source).unwrap().len(),
            (COPY_BUFFER_SIZE * 8) as u64
        );
        assert!(!destination.join("large.bin").exists());
        assert!(fs::read_dir(&destination).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("explorie-copy")
        }));
    }

    #[test]
    fn cancelled_move_replace_preserves_source_and_destination() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("item.txt");
        let destination = temp.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(destination.join("item.txt"), b"old").unwrap();

        let result = perform_file_operation(
            request(
                FileOperationKind::Move,
                &source,
                &destination,
                ConflictPolicy::Replace,
            ),
            &AtomicBool::new(true),
            |_| {},
        );

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(fs::read(source).unwrap(), b"new");
        assert_eq!(fs::read(destination.join("item.txt")).unwrap(), b"old");
    }

    #[test]
    fn move_rename_policy_never_overwrites_existing_file() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("item.txt");
        let destination = temp.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(destination.join("item.txt"), b"old").unwrap();

        let result = perform_file_operation(
            request(
                FileOperationKind::Move,
                &source,
                &destination,
                ConflictPolicy::Rename,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        assert!(!source.exists());
        assert_eq!(fs::read(destination.join("item.txt")).unwrap(), b"old");
        assert_eq!(
            fs::read(destination.join(kept("item.txt"))).unwrap(),
            b"new"
        );
        assert_eq!(result.targets, vec![destination.join(kept("item.txt"))]);
    }

    #[test]
    fn same_volume_move_replaces_transactionally() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("item.txt");
        let destination = temp.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(destination.join("item.txt"), b"old").unwrap();

        perform_file_operation(
            request(
                FileOperationKind::Move,
                &source,
                &destination,
                ConflictPolicy::Replace,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        assert!(!source.exists());
        assert_eq!(fs::read(destination.join("item.txt")).unwrap(), b"new");
    }

    #[test]
    fn rejects_destination_inside_source() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = source.join("nested");
        fs::create_dir_all(&destination).unwrap();

        let result = perform_file_operation(
            request(
                FileOperationKind::Copy,
                &source,
                &destination,
                ConflictPolicy::Error,
            ),
            &AtomicBool::new(false),
            |_| {},
        );

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn staged_verification_detects_same_size_content_corruption() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source.txt");
        let staged = temp.path().join("staged.txt");
        fs::write(&source, b"good").unwrap();
        fs::write(&staged, b"evil").unwrap();
        let plan = plan_source(&source).unwrap();

        let result = verify_staged_copy(&source, &staged, &plan, &AtomicBool::new(false));

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn failed_replace_restores_the_existing_destination() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("target.txt");
        let missing_stage = temp.path().join("missing-stage.txt");
        fs::write(&target, b"old").unwrap();

        let result = commit_stage(&missing_stage, &target, ConflictPolicy::Replace);

        assert!(result.is_err());
        assert_eq!(fs::read(target).unwrap(), b"old");
    }

    #[test]
    fn trash_moves_every_selected_item_in_one_request() {
        let temp = tempdir().unwrap();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        let trash = FakeTrashGuard::install(&temp.path().join("trash"), None);
        let mut progress = Vec::new();

        let result = perform_file_operation(
            trash_request(vec![first.clone(), second.clone()]),
            &AtomicBool::new(false),
            |update| progress.push(update),
        )
        .unwrap();

        assert_eq!(result.processed_entries, 2);
        assert!(!first.exists() && !second.exists());
        assert_eq!(
            trash.calls(),
            vec![(vec![first, second], TrashPurpose::UserRequest)]
        );
        let last = progress.last().unwrap();
        assert_eq!((last.processed_entries, last.total_entries), (2, 2));
    }

    #[test]
    fn trash_batches_large_selections() {
        let temp = tempdir().unwrap();
        let sources: Vec<PathBuf> = (0..TRASH_BATCH_SIZE + 3)
            .map(|index| {
                let path = temp.path().join(format!("{index}.txt"));
                fs::write(&path, b"x").unwrap();
                path
            })
            .collect();
        let trash = FakeTrashGuard::install(&temp.path().join("trash"), None);

        perform_file_operation(
            trash_request(sources.clone()),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        let calls = trash.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0.len(), TRASH_BATCH_SIZE);
        assert_eq!(calls[1].0, sources[TRASH_BATCH_SIZE..].to_vec());
    }

    #[test]
    fn trash_validates_every_source_before_moving_any() {
        let temp = tempdir().unwrap();
        let first = temp.path().join("first.txt");
        let missing = temp.path().join("missing.txt");
        let folder = temp.path().join("folder");
        fs::write(&first, b"first").unwrap();
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("child.txt"), b"child").unwrap();
        let trash = FakeTrashGuard::install(&temp.path().join("trash"), None);

        for sources in [
            vec![first.clone(), missing],
            vec![folder.clone(), folder.join("child.txt")],
        ] {
            assert!(
                perform_file_operation(trash_request(sources), &AtomicBool::new(false), |_| {})
                    .is_err()
            );
        }

        assert!(trash.calls().is_empty());
        assert_eq!(fs::read(first).unwrap(), b"first");
        assert!(folder.join("child.txt").exists());
    }

    #[test]
    fn partial_trash_failure_reports_what_moved_and_what_did_not() {
        let temp = tempdir().unwrap();
        let sources: Vec<PathBuf> = ["first.txt", "second.txt", "third.txt"]
            .into_iter()
            .map(|name| {
                let path = temp.path().join(name);
                fs::write(&path, name).unwrap();
                path
            })
            .collect();
        let _trash = FakeTrashGuard::install(&temp.path().join("trash"), Some(sources[1].clone()));

        let failure = perform_file_operation_report(
            trash_request(sources.clone()),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();

        let message = failure.error.to_string();
        assert!(
            message.contains("Moved 1 of 3 item(s) to the Trash"),
            "{message}"
        );
        assert!(
            message.contains("second.txt and 1 other item(s)"),
            "{message}"
        );
        assert!(message.contains("can be restored"), "{message}");
        assert_eq!(failure.partial_result.processed_entries, 1);
        assert!(!sources[0].exists());
        assert!(sources[1].exists() && sources[2].exists());
    }

    #[cfg(unix)]
    #[test]
    fn trash_accepts_folders_containing_symlinks_and_top_level_links() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let outside = temp.path().join("outside");
        let folder = temp.path().join("project");
        let link = temp.path().join("shortcut");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep.txt"), b"keep").unwrap();
        fs::create_dir_all(folder.join("node_modules")).unwrap();
        symlink(&outside, folder.join("node_modules/linked")).unwrap();
        symlink("missing-target", folder.join("dangling")).unwrap();
        symlink(&outside, &link).unwrap();
        let trash = FakeTrashGuard::install(&temp.path().join("trash"), None);

        perform_file_operation(
            trash_request(vec![folder.clone(), link.clone()]),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        assert_eq!(trash.calls().len(), 1);
        assert!(fs::symlink_metadata(&folder).is_err());
        assert!(fs::symlink_metadata(&link).is_err());
        assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn replace_moves_the_replaced_item_to_the_trash_under_its_name() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("item.txt");
        let destination = temp.path().join("destination");
        let trash_directory = temp.path().join("trash");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(destination.join("item.txt"), b"old").unwrap();
        let trash = FakeTrashGuard::install(&trash_directory, None);

        for kind in [FileOperationKind::Copy, FileOperationKind::Move] {
            if kind == FileOperationKind::Move {
                fs::write(destination.join("item.txt"), b"old").unwrap();
            }
            perform_file_operation(
                request(kind, &source, &destination, ConflictPolicy::Replace),
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
            assert_eq!(fs::read(destination.join("item.txt")).unwrap(), b"new");
        }

        let calls = trash.calls();
        assert_eq!(calls.len(), 2);
        for (paths, purpose) in &calls {
            assert_eq!(*purpose, TrashPurpose::ReplacedItem);
            assert_eq!(paths.len(), 1);
            assert_eq!(paths[0].file_name().unwrap(), "item.txt");
        }
        assert_eq!(
            fs::read(trash_directory.join("0-item.txt")).unwrap(),
            b"old"
        );
        assert_eq!(
            fs::read(trash_directory.join("1-item.txt")).unwrap(),
            b"old"
        );
        assert!(explorie_leftovers(&destination).is_empty());
    }

    #[test]
    fn replace_deletes_the_replaced_item_when_the_trash_is_unavailable() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("item");
        let destination = temp.path().join("destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested/new.txt"), b"new").unwrap();
        fs::create_dir_all(destination.join("item")).unwrap();
        fs::write(destination.join("item/old.txt"), b"old").unwrap();

        perform_file_operation(
            request(
                FileOperationKind::Copy,
                &source,
                &destination,
                ConflictPolicy::Replace,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        assert_eq!(
            fs::read(destination.join("item/nested/new.txt")).unwrap(),
            b"new"
        );
        assert!(!destination.join("item/old.txt").exists());
        assert!(explorie_leftovers(&destination).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn rename_fallback_never_replaces_existing_entries() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("file.txt"), b"file").unwrap();
        fs::write(root.join("occupied.txt"), b"occupied").unwrap();
        fs::create_dir(root.join("folder")).unwrap();
        fs::write(root.join("folder/inner.txt"), b"inner").unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        symlink("file.txt", root.join("link")).unwrap();

        for (source, occupied) in [
            ("file.txt", "occupied.txt"),
            ("folder", "empty"),
            ("link", "occupied.txt"),
        ] {
            let error =
                rename_noreplace_fallback(&root.join(source), &root.join(occupied)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{source}");
        }
        assert_eq!(fs::read(root.join("occupied.txt")).unwrap(), b"occupied");
        assert!(root.join("empty").is_dir());

        rename_noreplace_fallback(&root.join("file.txt"), &root.join("moved.txt")).unwrap();
        rename_noreplace_fallback(&root.join("folder"), &root.join("moved-folder")).unwrap();
        rename_noreplace_fallback(&root.join("link"), &root.join("moved-link")).unwrap();
        assert!(!root.join("file.txt").exists());
        assert_eq!(fs::read(root.join("moved.txt")).unwrap(), b"file");
        assert_eq!(
            fs::read(root.join("moved-folder/inner.txt")).unwrap(),
            b"inner"
        );
        assert_eq!(
            fs::read_link(root.join("moved-link")).unwrap(),
            Path::new("file.txt")
        );
    }

    #[cfg(unix)]
    #[test]
    fn operations_commit_without_an_exclusive_rename_primitive() {
        let _mode = RenameModeGuard::set(RenameMode::Fallback);
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested/data.txt"), b"content").unwrap();
        fs::write(temp.path().join("item.txt"), b"new").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("item.txt"), b"old").unwrap();
        let run = |kind, source: &Path, policy| {
            perform_file_operation(
                request(kind, source, &destination, policy),
                &AtomicBool::new(false),
                |_| {},
            )
        };

        run(FileOperationKind::Copy, &source, ConflictPolicy::Error).unwrap();
        assert_eq!(
            fs::read(destination.join("source/nested/data.txt")).unwrap(),
            b"content"
        );
        let conflict = run(FileOperationKind::Copy, &source, ConflictPolicy::Error).unwrap_err();
        assert_eq!(conflict.kind(), io::ErrorKind::AlreadyExists);
        let renamed = run(FileOperationKind::Move, &source, ConflictPolicy::Rename).unwrap();
        assert_eq!(renamed.targets, vec![destination.join(kept("source"))]);
        assert!(!source.exists());

        run(
            FileOperationKind::Copy,
            &temp.path().join("item.txt"),
            ConflictPolicy::Replace,
        )
        .unwrap();
        assert_eq!(fs::read(destination.join("item.txt")).unwrap(), b"new");
        run(
            FileOperationKind::Move,
            &temp.path().join("item.txt"),
            ConflictPolicy::Rename,
        )
        .unwrap();
        assert_eq!(
            fs::read(destination.join(kept("item.txt"))).unwrap(),
            b"new"
        );
        assert!(explorie_leftovers(&destination).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn copy_recreates_symlinks_without_following_them() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let outside = temp.path().join("outside");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"outside").unwrap();
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("real.txt"), b"real").unwrap();
        symlink("../real.txt", source.join("bin/relative")).unwrap();
        symlink(&outside, source.join("absolute-dir")).unwrap();
        symlink("missing", source.join("dangling")).unwrap();

        let result = perform_file_operation(
            request(
                FileOperationKind::Copy,
                &source,
                &destination,
                ConflictPolicy::Error,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        let copied = destination.join("source");
        for (link, target) in [
            ("bin/relative", PathBuf::from("../real.txt")),
            ("absolute-dir", outside.clone()),
            ("dangling", PathBuf::from("missing")),
        ] {
            let path = copied.join(link);
            assert!(
                fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(fs::read_link(&path).unwrap(), target);
        }
        assert_eq!(fs::read(copied.join("bin/relative")).unwrap(), b"real");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
        assert!(path_matches_snapshot(&result.targets[0], &result.target_snapshots[0]).unwrap());
        fs::remove_file(copied.join("dangling")).unwrap();
        symlink("elsewhere", copied.join("dangling")).unwrap();
        assert!(!path_matches_snapshot(&result.targets[0], &result.target_snapshots[0]).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn top_level_symlink_is_copied_and_moved_as_the_link() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let target_dir = temp.path().join("target-dir");
        let link = temp.path().join("link");
        let copies = temp.path().join("copies");
        let moved = temp.path().join("moved");
        fs::create_dir(&target_dir).unwrap();
        fs::write(target_dir.join("inside.txt"), b"inside").unwrap();
        fs::create_dir(&copies).unwrap();
        fs::create_dir(&moved).unwrap();
        symlink(&target_dir, &link).unwrap();

        perform_file_operation(
            request(
                FileOperationKind::Copy,
                &link,
                &copies,
                ConflictPolicy::Error,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        perform_file_operation(
            request(
                FileOperationKind::Move,
                &link,
                &moved,
                ConflictPolicy::Error,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        for copy in [copies.join("link"), moved.join("link")] {
            assert!(
                fs::symlink_metadata(&copy)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(fs::read_link(&copy).unwrap(), target_dir);
        }
        assert!(fs::symlink_metadata(&link).is_err());
        assert_eq!(fs::read_dir(&target_dir).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn same_volume_move_renames_folders_containing_symlinks_without_walking_them() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let source = temp.path().join("App.app");
        let destination = temp.path().join("Applications");
        fs::create_dir_all(source.join("Contents/Frameworks/Lib.framework/Versions/A")).unwrap();
        symlink(
            "Versions/A",
            source.join("Contents/Frameworks/Lib.framework/Current"),
        )
        .unwrap();
        fs::create_dir(&destination).unwrap();
        let mut progress = Vec::new();

        perform_file_operation(
            request(
                FileOperationKind::Move,
                &source,
                &destination,
                ConflictPolicy::Error,
            ),
            &AtomicBool::new(false),
            |update| progress.push(update),
        )
        .unwrap();

        assert!(!source.exists());
        assert_eq!(
            fs::read_link(destination.join("App.app/Contents/Frameworks/Lib.framework/Current"))
                .unwrap(),
            Path::new("Versions/A")
        );
        assert!(progress.iter().all(|update| update.total_entries == 1));
    }

    #[cfg(unix)]
    #[test]
    fn cross_volume_move_recreates_links_and_verifies_their_targets() {
        use std::os::unix::fs::symlink;

        let _mode = RenameModeGuard::set(RenameMode::CrossDevice);
        let temp = tempdir().unwrap();
        let source = temp.path().join("repo");
        let link = temp.path().join("top-link");
        let destination = temp.path().join("other-volume");
        fs::create_dir_all(source.join(".git")).unwrap();
        fs::write(source.join(".git/HEAD"), b"ref: main").unwrap();
        symlink(".git/HEAD", source.join("head-link")).unwrap();
        symlink("../elsewhere", &link).unwrap();
        fs::create_dir(&destination).unwrap();
        let mut progress = Vec::new();

        perform_file_operation(
            FileOperationRequest {
                kind: FileOperationKind::Move,
                sources: vec![source.clone(), link.clone()],
                destination: Some(destination.clone()),
                conflict_policy: ConflictPolicy::Error,
            },
            &AtomicBool::new(false),
            |update| progress.push(update),
        )
        .unwrap();

        assert!(!source.exists());
        assert!(fs::symlink_metadata(&link).is_err());
        assert_eq!(
            fs::read(destination.join("repo/.git/HEAD")).unwrap(),
            b"ref: main"
        );
        assert_eq!(
            fs::read_link(destination.join("repo/head-link")).unwrap(),
            Path::new(".git/HEAD")
        );
        assert_eq!(
            fs::read_link(destination.join("top-link")).unwrap(),
            Path::new("../elsewhere")
        );
        let last = progress.last().unwrap();
        assert_eq!(last.processed_entries, last.total_entries);
        assert_eq!(last.total_entries, 5);
        assert!(explorie_leftovers(temp.path()).is_empty());
        assert!(explorie_leftovers(&destination).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn staged_verification_detects_a_retargeted_link() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        let staged = temp.path().join("staged");
        symlink("expected", &source).unwrap();
        symlink("tampered", &staged).unwrap();
        let plan = plan_source(&source).unwrap();

        let result = verify_staged_copy(&source, &staged, &plan, &AtomicBool::new(false));

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_folders_copy_and_move_with_their_contents() {
        use std::os::unix::fs::PermissionsExt;

        struct RestoreWritable(Vec<PathBuf>);
        impl Drop for RestoreWritable {
            fn drop(&mut self) {
                for path in &self.0 {
                    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
                }
            }
        }

        let temp = tempdir().unwrap();
        let source = temp.path().join("locked");
        let copies = temp.path().join("copies");
        let moved = temp.path().join("moved");
        fs::create_dir_all(source.join("inner")).unwrap();
        fs::write(source.join("top.txt"), b"top").unwrap();
        fs::write(source.join("inner/deep.txt"), b"deep").unwrap();
        fs::create_dir(&copies).unwrap();
        fs::create_dir(&moved).unwrap();
        let _restore = RestoreWritable(vec![
            source.join("inner"),
            source.clone(),
            copies.join("locked/inner"),
            copies.join("locked"),
            moved.join("locked/inner"),
            moved.join("locked"),
        ]);
        for directory in [source.join("inner"), source.clone()] {
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();
        }

        perform_file_operation(
            request(
                FileOperationKind::Copy,
                &source,
                &copies,
                ConflictPolicy::Error,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        {
            let _mode = RenameModeGuard::set(RenameMode::CrossDevice);
            perform_file_operation(
                request(
                    FileOperationKind::Move,
                    &source,
                    &moved,
                    ConflictPolicy::Error,
                ),
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        }

        assert!(!source.exists());
        for root in [copies.join("locked"), moved.join("locked")] {
            assert_eq!(fs::read(root.join("top.txt")).unwrap(), b"top");
            assert_eq!(fs::read(root.join("inner/deep.txt")).unwrap(), b"deep");
            for directory in [root.join("inner"), root.clone()] {
                let mode = fs::metadata(&directory).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o555, "{}", directory.display());
            }
        }
        assert!(explorie_leftovers(temp.path()).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn conflict_detection_matches_composed_and_decomposed_names() {
        let temp = tempdir().unwrap();
        let composed = "caf\u{e9}.txt";
        let decomposed = "cafe\u{301}.txt";
        let source_dir = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source_dir).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source_dir.join(composed), b"new").unwrap();
        fs::write(destination.join(decomposed), b"existing").unwrap();
        let source = source_dir.join(composed);

        let conflict = perform_file_operation(
            request(
                FileOperationKind::Copy,
                &source,
                &destination,
                ConflictPolicy::Error,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(conflict.kind(), io::ErrorKind::AlreadyExists);

        let renamed = perform_file_operation(
            request(
                FileOperationKind::Move,
                &source,
                &destination,
                ConflictPolicy::Rename,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(renamed.targets, vec![destination.join("caf\u{e9} 2.txt")]);
        assert_eq!(fs::read(destination.join(decomposed)).unwrap(), b"existing");
    }

    #[cfg(windows)]
    #[test]
    fn windows_copy_preserves_alternate_data_streams() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source.txt");
        let source_stream = PathBuf::from(format!("{}:explorie", source.display()));
        let destination = temp.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"primary").unwrap();
        if fs::write(&source_stream, b"alternate").is_err() {
            eprintln!("skipping alternate stream test: filesystem does not support ADS");
            return;
        }

        perform_file_operation(
            request(
                FileOperationKind::Copy,
                &source,
                &destination,
                ConflictPolicy::Error,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        let copied_stream = destination.join("source.txt:explorie");
        assert_eq!(fs::read(copied_stream).unwrap(), b"alternate");
    }

    #[cfg(windows)]
    #[test]
    fn cross_volume_move_preserves_content_and_streams_when_configured() {
        let Some(other_volume) = std::env::var_os("EXPLORIE_CROSS_VOLUME_TEST_DIR") else {
            eprintln!("skipping cross-volume test: EXPLORIE_CROSS_VOLUME_TEST_DIR is unset");
            return;
        };
        let source_root = tempdir().unwrap();
        let destination_root = tempfile::Builder::new()
            .prefix("explorie-cross-volume-")
            .tempdir_in(other_volume)
            .unwrap();
        let source = source_root.path().join("source.txt");
        fs::write(&source, b"primary").unwrap();
        let source_stream = PathBuf::from(format!("{}:explorie", source.display()));
        fs::write(&source_stream, b"alternate").unwrap();

        let result = perform_file_operation(
            request(
                FileOperationKind::Move,
                &source,
                destination_root.path(),
                ConflictPolicy::Error,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        let target = &result.targets[0];
        assert!(!source.exists());
        assert_eq!(fs::read(target).unwrap(), b"primary");
        assert_eq!(
            fs::read(PathBuf::from(format!("{}:explorie", target.display()))).unwrap(),
            b"alternate"
        );
    }

    #[cfg(windows)]
    #[test]
    fn cross_volume_move_fails_closed_when_metadata_cannot_be_preserved() {
        let Some(other_volume) = std::env::var_os("EXPLORIE_UNSUPPORTED_VOLUME_TEST_DIR") else {
            eprintln!(
                "skipping unsupported-volume test: EXPLORIE_UNSUPPORTED_VOLUME_TEST_DIR is unset"
            );
            return;
        };
        let source_root = tempdir().unwrap();
        let destination_root = tempfile::Builder::new()
            .prefix("explorie-unsupported-volume-")
            .tempdir_in(other_volume)
            .unwrap();
        let source = source_root.path().join("source.txt");
        fs::write(&source, b"primary").unwrap();
        fs::write(
            PathBuf::from(format!("{}:explorie", source.display())),
            b"alternate",
        )
        .unwrap();

        let result = perform_file_operation(
            request(
                FileOperationKind::Move,
                &source,
                destination_root.path(),
                ConflictPolicy::Error,
            ),
            &AtomicBool::new(false),
            |_| {},
        );

        assert!(result.is_err());
        assert_eq!(fs::read(&source).unwrap(), b"primary");
        assert!(!destination_root.path().join("source.txt").exists());
    }

    #[cfg(windows)]
    #[test]
    fn cross_volume_directory_move_preserves_streams_when_configured() {
        let Some(other_volume) = std::env::var_os("EXPLORIE_CROSS_VOLUME_TEST_DIR") else {
            eprintln!("skipping cross-volume test: EXPLORIE_CROSS_VOLUME_TEST_DIR is unset");
            return;
        };
        let source_root = tempdir().unwrap();
        let destination_root = tempfile::Builder::new()
            .prefix("explorie-cross-volume-directory-")
            .tempdir_in(other_volume)
            .unwrap();
        let source = source_root.path().join("folder");
        fs::create_dir(&source).unwrap();
        let nested = source.join("nested.txt");
        fs::write(&nested, b"primary").unwrap();
        fs::write(
            PathBuf::from(format!("{}:directory", source.display())),
            b"directory stream",
        )
        .unwrap();
        fs::write(
            PathBuf::from(format!("{}:file", nested.display())),
            b"file stream",
        )
        .unwrap();

        let result = perform_file_operation(
            request(
                FileOperationKind::Move,
                &source,
                destination_root.path(),
                ConflictPolicy::Error,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        let target = &result.targets[0];
        assert!(!source.exists());
        assert_eq!(fs::read(target.join("nested.txt")).unwrap(), b"primary");
        assert_eq!(
            fs::read(PathBuf::from(format!("{}:directory", target.display()))).unwrap(),
            b"directory stream"
        );
        assert_eq!(
            fs::read(PathBuf::from(format!(
                "{}:file",
                target.join("nested.txt").display()
            )))
            .unwrap(),
            b"file stream"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_copy_preserves_extended_attributes() {
        use std::process::Command;

        let temp = tempdir().unwrap();
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"primary").unwrap();
        let status = Command::new("xattr")
            .arg("-w")
            .arg("com.omershatz.explorie.test")
            .arg("metadata")
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());

        perform_file_operation(
            request(
                FileOperationKind::Copy,
                &source,
                &destination,
                ConflictPolicy::Error,
            ),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        let output = Command::new("xattr")
            .arg("-p")
            .arg("com.omershatz.explorie.test")
            .arg(destination.join("source.txt"))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "metadata");
    }
}
