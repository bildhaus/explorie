use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use explorie_native_services::{ConflictPolicy, FileOperationKind, FileOperationRequest};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const JOURNAL_FILE: &str = "operation-recovery-v1.json";
const JOURNAL_VERSION: u32 = 1;
/// Allowance for filesystems that store creation times coarsely.
const CREATION_TIME_SLACK: Duration = Duration::from_secs(2);
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);
/// Tags journal entries with the process that recorded them. Only one Explorie
/// process runs at a time, so entries from any other instance belong to a run
/// that has ended.
static PROCESS_INSTANCE: LazyLock<String> = LazyLock::new(|| Uuid::new_v4().to_string());

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryDisposition {
    SafeToRetry,
    CompletedMove,
    NeedsReview,
}

/// A hidden item where an interrupted operation may have left the only copy of
/// user data. Core names these `.explorie-<purpose>-<uuid>` next to where the
/// data belongs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryArtifactKind {
    /// A move's source, set aside while its cross-volume copy was verified.
    SetAsideSource,
    /// A replacing move's source, renamed into the destination folder.
    StagedSource,
    /// The destination item that a replacing operation set aside.
    ReplacedItem,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecoveryArtifact {
    kind: RecoveryArtifactKind,
    path: PathBuf,
    original: PathBuf,
    restorable: bool,
}

impl RecoveryArtifact {
    pub(crate) fn kind(&self) -> RecoveryArtifactKind {
        self.kind
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn original(&self) -> &Path {
        &self.original
    }

    /// True when this item is the only candidate for its original location and
    /// that location is free, so renaming it back cannot pick the wrong item or
    /// overwrite anything.
    pub(crate) fn restorable(&self) -> bool {
        self.restorable
    }
}

#[derive(Clone, Debug)]
pub(crate) struct InterruptedOperation {
    id: String,
    request: FileOperationRequest,
    disposition: RecoveryDisposition,
    /// Staging copies this operation created, captured when the journal was
    /// opened at launch, before any operation of this process could stage.
    stages: Vec<PathBuf>,
    artifacts: Vec<RecoveryArtifact>,
}

impl InterruptedOperation {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn request(&self) -> &FileOperationRequest {
        &self.request
    }

    pub(crate) fn disposition(&self) -> RecoveryDisposition {
        self.disposition
    }

    pub(crate) fn artifacts(&self) -> &[RecoveryArtifact] {
        &self.artifacts
    }

    pub(crate) fn prepare_retry(&self) -> io::Result<()> {
        if self.disposition != RecoveryDisposition::SafeToRetry {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "interrupted operation is not safe to retry",
            ));
        }
        let source_is_dir = fs::symlink_metadata(&self.request.sources[0])?.is_dir();
        for stage in &self.stages {
            // A partial copy always has its source's file type; anything else
            // is not this operation's stage.
            match fs::symlink_metadata(stage) {
                Ok(metadata)
                    if !metadata.file_type().is_symlink() && metadata.is_dir() == source_is_dir =>
                {
                    remove_path(stage)?;
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn same_state(&self, other: &Self) -> bool {
        self.id == other.id
            && self.disposition == other.disposition
            && self.artifacts == other.artifacts
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct JournalEntry {
    id: String,
    request: FileOperationRequest,
    /// Process instance that recorded the entry; absent in older journals,
    /// whose entries all predate the current run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recorded_at: Option<SystemTime>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct JournalSnapshot {
    version: u32,
    entries: Vec<JournalEntry>,
}

/// The process-wide interrupted-operation journal. Clones share one state, so
/// every window reads and writes the same entries.
#[derive(Clone)]
pub(crate) struct OperationRecoveryStore {
    state: Arc<Mutex<OperationRecoveryState>>,
}

struct OperationRecoveryState {
    path: PathBuf,
    instance: String,
    entries: Vec<JournalEntry>,
    /// Entries from previous runs that still await a decision.
    interrupted: Vec<InterruptedOperation>,
}

impl OperationRecoveryStore {
    pub(crate) fn open(
        config_dir: &Path,
    ) -> (Option<Self>, Vec<InterruptedOperation>, Option<String>) {
        Self::open_as(config_dir, PROCESS_INSTANCE.clone())
    }

    /// Opens the journal as a different process would: entries recorded here
    /// look interrupted to this process's stores, and vice versa.
    #[cfg(test)]
    pub(crate) fn open_as_other_run(
        config_dir: &Path,
    ) -> (Option<Self>, Vec<InterruptedOperation>, Option<String>) {
        Self::open_as(config_dir, Uuid::new_v4().to_string())
    }

    fn open_as(
        config_dir: &Path,
        instance: String,
    ) -> (Option<Self>, Vec<InterruptedOperation>, Option<String>) {
        let path = config_dir.join(JOURNAL_FILE);
        let entries = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<JournalSnapshot>(&bytes) {
                Ok(snapshot) if snapshot.version == JOURNAL_VERSION => snapshot.entries,
                Ok(snapshot) => {
                    return (
                        None,
                        Vec::new(),
                        Some(format!(
                            "Interrupted-operation recovery unavailable; unsupported journal version {} was preserved",
                            snapshot.version
                        )),
                    );
                }
                Err(error) => {
                    return (
                        None,
                        Vec::new(),
                        Some(format!(
                            "Interrupted-operation recovery unavailable; the existing journal was preserved: {error}"
                        )),
                    );
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return (
                    None,
                    Vec::new(),
                    Some(format!(
                        "Interrupted-operation recovery unavailable: {error}"
                    )),
                );
            }
        };
        let interrupted = classify_entries(
            entries
                .iter()
                .filter(|entry| entry.owner.as_deref() != Some(instance.as_str())),
        );
        (
            Some(Self {
                state: Arc::new(Mutex::new(OperationRecoveryState {
                    path,
                    instance,
                    entries,
                    interrupted: interrupted.clone(),
                })),
            }),
            interrupted,
            None,
        )
    }

    fn lock(&self) -> MutexGuard<'_, OperationRecoveryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn record(&self, request: &FileOperationRequest) -> io::Result<Vec<String>> {
        if request.kind == FileOperationKind::Trash {
            return Ok(Vec::new());
        }
        let mut state = self.lock();
        let previous_len = state.entries.len();
        let owner = Some(state.instance.clone());
        let recorded_at = Some(SystemTime::now());
        let ids = request
            .sources
            .iter()
            .map(|source| {
                let id = Uuid::new_v4().to_string();
                let mut request = request.clone();
                request.sources = vec![source.clone()];
                state.entries.push(JournalEntry {
                    id: id.clone(),
                    request,
                    owner: owner.clone(),
                    recorded_at,
                });
                id
            })
            .collect::<Vec<_>>();
        if let Err(error) = state.persist() {
            state.entries.truncate(previous_len);
            return Err(error);
        }
        Ok(ids)
    }

    pub(crate) fn remove(&self, ids: &[String]) -> io::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut state = self.lock();
        let previous = state.entries.clone();
        state
            .entries
            .retain(|entry| !ids.iter().any(|id| id == &entry.id));
        if state.entries.len() != previous.len()
            && let Err(error) = state.persist()
        {
            state.entries = previous;
            return Err(error);
        }
        state
            .interrupted
            .retain(|operation| !ids.iter().any(|id| id == &operation.id));
        Ok(())
    }

    pub(crate) fn interrupted(&self) -> Vec<InterruptedOperation> {
        self.lock().interrupted.clone()
    }

    /// Returns the current interrupted operations when they differ from
    /// `current`, so windows can follow decisions made in other windows.
    pub(crate) fn interrupted_if_changed(
        &self,
        current: &[InterruptedOperation],
    ) -> Option<Vec<InterruptedOperation>> {
        let state = self.lock();
        let unchanged = state.interrupted.len() == current.len()
            && state
                .interrupted
                .iter()
                .zip(current)
                .all(|(shared, local)| shared.same_state(local));
        (!unchanged).then(|| state.interrupted.clone())
    }

    /// Claims a safe-to-retry operation so no other window can start it too,
    /// and re-stamps its entry for this run so a crash during the retry is
    /// recovered on the next launch. Returns `None` when it is no longer
    /// awaiting a decision.
    pub(crate) fn claim_for_retry(&self, id: &str) -> io::Result<Option<InterruptedOperation>> {
        let mut state = self.lock();
        let Some(index) = state.interrupted.iter().position(|operation| {
            operation.id == id && operation.disposition == RecoveryDisposition::SafeToRetry
        }) else {
            return Ok(None);
        };
        let Some(entry_index) = state.entries.iter().position(|entry| entry.id == id) else {
            state.interrupted.remove(index);
            return Ok(None);
        };
        let previous = state.entries[entry_index].clone();
        let owner = Some(state.instance.clone());
        let entry = &mut state.entries[entry_index];
        entry.owner = owner;
        entry.recorded_at = Some(SystemTime::now());
        if let Err(error) = state.persist() {
            state.entries[entry_index] = previous;
            return Err(error);
        }
        Ok(Some(state.interrupted.remove(index)))
    }

    /// Returns a claimed operation that could not be retried.
    pub(crate) fn release(&self, operation: InterruptedOperation) {
        let mut state = self.lock();
        if state.entries.iter().any(|entry| entry.id == operation.id)
            && !state
                .interrupted
                .iter()
                .any(|candidate| candidate.id == operation.id)
        {
            state.interrupted.push(operation);
        }
    }

    /// Renames an operation's restorable items back to where they came from,
    /// then re-evaluates the operation. Returns how many items were restored
    /// and the first failure, if any.
    pub(crate) fn restore_artifacts(&self, id: &str) -> (usize, Option<io::Error>) {
        let mut state = self.lock();
        let Some(operation) = state
            .interrupted
            .iter_mut()
            .find(|operation| operation.id == id)
        else {
            return (0, None);
        };
        let mut restored = 0;
        let mut failure = None;
        for artifact in operation
            .artifacts
            .iter()
            .filter(|artifact| artifact.restorable)
        {
            match explorie_core::rename_noreplace(&artifact.path, &artifact.original) {
                Ok(()) => restored += 1,
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        operation
            .artifacts
            .retain(|artifact| fs::symlink_metadata(&artifact.path).is_ok());
        for artifact in &mut operation.artifacts {
            artifact.restorable &= matches!(artifact.original.try_exists(), Ok(false));
        }
        // Restoring a set-aside source can make the operation safe to retry.
        operation.disposition = classify_request(&operation.request);
        (restored, failure)
    }
}

impl OperationRecoveryState {
    fn persist(&self) -> io::Result<()> {
        if self.entries.is_empty() {
            return match fs::remove_file(&self.path) {
                Ok(()) => sync_parent_directory(&self.path),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            };
        }
        let bytes = serde_json::to_vec_pretty(&JournalSnapshot {
            version: JOURNAL_VERSION,
            entries: self.entries.clone(),
        })
        .map_err(io::Error::other)?;
        atomic_write(&self.path, &bytes)
    }
}

type StagingListing = Vec<(PathBuf, Option<SystemTime>)>;

/// Directory scans shared by every entry classified at launch.
#[derive(Default)]
struct StagingListings(HashMap<(PathBuf, &'static str), StagingListing>);

impl StagingListings {
    fn get(&mut self, directory: &Path, purpose: &'static str) -> &StagingListing {
        self.0
            .entry((directory.to_path_buf(), purpose))
            .or_insert_with(|| staging_entries(directory, purpose))
    }
}

/// Lists core's `.explorie-<purpose>-<uuid>` items in `directory` with their
/// creation times.
fn staging_entries(directory: &Path, purpose: &str) -> StagingListing {
    let prefix = format!(".explorie-{purpose}-");
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let suffix = name.to_str()?.strip_prefix(&prefix)?;
            Uuid::parse_str(suffix).ok()?;
            let path = entry.path();
            let created = fs::symlink_metadata(&path)
                .and_then(|metadata| metadata.created())
                .ok();
            Some((path, created))
        })
        .collect()
}

fn classify_entries<'a>(
    entries: impl Iterator<Item = &'a JournalEntry>,
) -> Vec<InterruptedOperation> {
    let mut listings = StagingListings::default();
    let mut operations = entries
        .map(|entry| {
            let (stages, artifacts) = scan_entry(entry, &mut listings);
            InterruptedOperation {
                id: entry.id.clone(),
                request: entry.request.clone(),
                disposition: classify_request(&entry.request),
                stages,
                artifacts,
            }
        })
        .collect::<Vec<_>>();
    // Artifact names carry no owner. An item claimed by several operations, or
    // one of several candidates for the same operation, is shown with its
    // location but never restored automatically.
    let mut claims = HashMap::<PathBuf, usize>::new();
    for artifact in operations.iter().flat_map(|operation| &operation.artifacts) {
        *claims.entry(artifact.path.clone()).or_default() += 1;
    }
    for operation in &mut operations {
        let kinds = operation
            .artifacts
            .iter()
            .map(|artifact| artifact.kind)
            .collect::<Vec<_>>();
        for artifact in &mut operation.artifacts {
            artifact.restorable = kinds.iter().filter(|kind| **kind == artifact.kind).count() == 1
                && claims[&artifact.path] == 1
                && matches!(artifact.original.try_exists(), Ok(false));
        }
    }
    operations
}

/// Finds the staging copies an entry created and any hidden items holding data
/// it had moved out of the way.
fn scan_entry(
    entry: &JournalEntry,
    listings: &mut StagingListings,
) -> (Vec<PathBuf>, Vec<RecoveryArtifact>) {
    let request = &entry.request;
    let ([source], Some(destination)) = (request.sources.as_slice(), request.destination.as_ref())
    else {
        return (Vec::new(), Vec::new());
    };
    let Some(name) = source.file_name() else {
        return (Vec::new(), Vec::new());
    };
    let purpose = match request.kind {
        FileOperationKind::Copy => "copy",
        FileOperationKind::Move => "move",
        FileOperationKind::Trash => return (Vec::new(), Vec::new()),
    };
    let target = destination.join(name);
    // Staging copies are created after the entry was journaled, while renamed
    // user data keeps its original creation time. Without both times, nothing
    // is treated as this entry's disposable stage.
    let created_after_recording = |created: Option<SystemTime>| match (created, entry.recorded_at) {
        (Some(created), Some(recorded)) => created + CREATION_TIME_SLACK >= recorded,
        _ => false,
    };
    let stages = listings
        .get(destination, purpose)
        .iter()
        .filter(|(_, created)| created_after_recording(*created))
        .map(|(path, _)| path.clone())
        .collect();

    let source_missing = matches!(source.try_exists(), Ok(false));
    let target_missing = matches!(target.try_exists(), Ok(false));
    let replacing = request.conflict_policy == ConflictPolicy::Replace;
    let artifact = |kind, path: &PathBuf, original: &Path| RecoveryArtifact {
        kind,
        path: path.clone(),
        original: original.to_path_buf(),
        restorable: false,
    };
    let mut artifacts = Vec::new();
    let backups = if replacing {
        listings.get(destination, "backup").clone()
    } else {
        Vec::new()
    };
    if request.kind == FileOperationKind::Move && source_missing {
        let set_aside = source
            .parent()
            .map(|parent| listings.get(parent, "source").clone())
            .unwrap_or_default();
        if !set_aside.is_empty() {
            // Once the target exists the move committed and a set-aside
            // source is a leftover duplicate, not the only copy.
            if target_missing {
                artifacts.extend(
                    set_aside.iter().map(|(path, _)| {
                        artifact(RecoveryArtifactKind::SetAsideSource, path, source)
                    }),
                );
            }
        } else if replacing && (target_missing || backups.is_empty()) {
            artifacts.extend(
                listings
                    .get(destination, "move")
                    .iter()
                    .filter(|(_, created)| !created_after_recording(*created))
                    .map(|(path, _)| artifact(RecoveryArtifactKind::StagedSource, path, source)),
            );
        }
    }
    if target_missing {
        artifacts.extend(
            backups
                .iter()
                .map(|(path, _)| artifact(RecoveryArtifactKind::ReplacedItem, path, &target)),
        );
    }
    (stages, artifacts)
}

fn remove_path(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn classify_request(request: &FileOperationRequest) -> RecoveryDisposition {
    if request.sources.len() != 1
        || request.conflict_policy != ConflictPolicy::Error
        || !matches!(
            request.kind,
            FileOperationKind::Copy | FileOperationKind::Move
        )
    {
        return RecoveryDisposition::NeedsReview;
    }
    let source = &request.sources[0];
    let Some(destination) = request.destination.as_ref() else {
        return RecoveryDisposition::NeedsReview;
    };
    let Some(name) = source.file_name() else {
        return RecoveryDisposition::NeedsReview;
    };
    let target = destination.join(name);
    let Ok(source_exists) = source.try_exists() else {
        return RecoveryDisposition::NeedsReview;
    };
    let Ok(target_exists) = target.try_exists() else {
        return RecoveryDisposition::NeedsReview;
    };
    match (request.kind, source_exists, target_exists) {
        (FileOperationKind::Copy | FileOperationKind::Move, true, false) => {
            RecoveryDisposition::SafeToRetry
        }
        (FileOperationKind::Move, false, true) => RecoveryDisposition::CompletedMove,
        _ => RecoveryDisposition::NeedsReview,
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "journal path has no parent"))?;
    fs::create_dir_all(parent)?;
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy(),
        std::process::id(),
        counter
    ));
    let result = (|| {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        replace_file(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    sync_parent_directory(path)
}

/// Makes a rename or removal in the journal's folder durable, so a crash
/// cannot bring back a stale journal or lose a new one.
#[cfg(not(windows))]
fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "journal path has no parent"))?;
    File::open(parent)?.sync_all()
}

/// `MoveFileExW` with `MOVEFILE_WRITE_THROUGH` already flushes the rename.
#[cfg(windows)]
fn sync_parent_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temp: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temp, destination)
}

#[cfg(windows)]
fn replace_file(temp: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let temp = temp
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let moved = unsafe {
        MoveFileExW(
            temp.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        kind: FileOperationKind,
        source: PathBuf,
        destination: PathBuf,
    ) -> FileOperationRequest {
        FileOperationRequest {
            kind,
            sources: vec![source],
            destination: Some(destination),
            conflict_policy: ConflictPolicy::Error,
        }
    }

    fn fixture(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("explorie-operation-{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn stage_name(purpose: &str) -> String {
        format!(".explorie-{purpose}-{}", Uuid::new_v4())
    }

    fn created(path: &Path) -> SystemTime {
        fs::symlink_metadata(path).unwrap().created().unwrap()
    }

    /// Writes a journal as a crashed run would have left it.
    fn write_previous_run(
        root: &Path,
        entries: Vec<(FileOperationRequest, SystemTime)>,
    ) -> Vec<String> {
        let entries = entries
            .into_iter()
            .map(|(request, recorded_at)| JournalEntry {
                id: Uuid::new_v4().to_string(),
                request,
                owner: Some("crashed-run".to_string()),
                recorded_at: Some(recorded_at),
            })
            .collect::<Vec<_>>();
        let ids = entries.iter().map(|entry| entry.id.clone()).collect();
        fs::write(
            root.join(JOURNAL_FILE),
            serde_json::to_vec(&JournalSnapshot {
                version: JOURNAL_VERSION,
                entries,
            })
            .unwrap(),
        )
        .unwrap();
        ids
    }

    #[test]
    fn journal_is_atomic_per_source_and_removed_when_empty() {
        let root = fixture("journal");
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        let (store, interrupted, warning) = OperationRecoveryStore::open(&root);
        assert!(warning.is_none());
        assert!(interrupted.is_empty());
        let store = store.unwrap();
        let mut operation = request(FileOperationKind::Copy, source.join("first"), destination);
        operation.sources.push(source.join("second"));
        let ids = store.record(&operation).unwrap();
        assert_eq!(ids.len(), 2);

        let (_, reopened, warning) = OperationRecoveryStore::open_as_other_run(&root);
        assert!(warning.is_none());
        assert_eq!(reopened.len(), 2);
        assert!(
            reopened
                .iter()
                .all(|entry| entry.request.sources.len() == 1)
        );

        store.remove(&ids[..1]).unwrap();
        let (_, reopened, _) = OperationRecoveryStore::open_as_other_run(&root);
        assert_eq!(reopened.len(), 1);
        store.remove(&ids[1..]).unwrap();
        assert!(!root.join(JOURNAL_FILE).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_entries_from_previous_runs_are_interrupted() {
        let root = fixture("owner");
        let destination = root.join("destination");
        fs::create_dir_all(&destination).unwrap();
        let source = root.join("item.txt");
        fs::write(&source, b"item").unwrap();
        let (store, _, _) = OperationRecoveryStore::open(&root);
        let store = store.unwrap();
        store
            .record(&request(
                FileOperationKind::Copy,
                source.clone(),
                destination.clone(),
            ))
            .unwrap();

        // A running operation of this process is never offered for recovery.
        let (same_run, interrupted, _) = OperationRecoveryStore::open(&root);
        assert!(interrupted.is_empty());
        assert!(same_run.unwrap().interrupted().is_empty());
        let (_, interrupted, _) = OperationRecoveryStore::open_as_other_run(&root);
        assert_eq!(interrupted.len(), 1);

        // Journals written before entries carried an owner predate this run.
        fs::write(
            root.join(JOURNAL_FILE),
            serde_json::to_vec(&serde_json::json!({
                "version": JOURNAL_VERSION,
                "entries": [{
                    "id": "legacy",
                    "request": request(FileOperationKind::Copy, source, destination),
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        let (_, interrupted, warning) = OperationRecoveryStore::open(&root);
        assert!(warning.is_none());
        assert_eq!(interrupted.len(), 1);
        assert_eq!(
            interrupted[0].disposition(),
            RecoveryDisposition::SafeToRetry
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_classification_is_conservative() {
        let root = fixture("reconcile");
        let source_dir = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&destination).unwrap();
        let source = source_dir.join("item.txt");
        fs::write(&source, b"source").unwrap();

        let recorded_at = SystemTime::now();
        let disposable = destination.join(stage_name("copy"));
        let unrelated = destination.join(".explorie-copy-not-a-uuid");
        fs::write(&disposable, b"sou").unwrap();
        fs::create_dir(&unrelated).unwrap();

        let entry = JournalEntry {
            id: Uuid::new_v4().to_string(),
            request: request(FileOperationKind::Copy, source.clone(), destination.clone()),
            owner: None,
            recorded_at: Some(recorded_at),
        };
        let interrupted = classify_entries(std::iter::once(&entry)).remove(0);
        assert_eq!(interrupted.disposition(), RecoveryDisposition::SafeToRetry);
        interrupted.prepare_retry().unwrap();
        assert!(!disposable.exists());
        assert!(unrelated.exists());

        assert_eq!(
            classify_request(&request(
                FileOperationKind::Copy,
                source.clone(),
                destination.clone()
            )),
            RecoveryDisposition::SafeToRetry
        );
        fs::write(destination.join("item.txt"), b"target").unwrap();
        assert_eq!(
            classify_request(&request(
                FileOperationKind::Copy,
                source.clone(),
                destination.clone()
            )),
            RecoveryDisposition::NeedsReview
        );
        fs::remove_file(&source).unwrap();
        assert_eq!(
            classify_request(&request(
                FileOperationKind::Move,
                source.clone(),
                destination.clone()
            )),
            RecoveryDisposition::CompletedMove
        );

        let mut renamed = request(FileOperationKind::Move, source, destination);
        renamed.conflict_policy = ConflictPolicy::Rename;
        assert_eq!(classify_request(&renamed), RecoveryDisposition::NeedsReview);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retry_removes_only_stages_created_by_that_operation() {
        let root = fixture("stages");
        let destination = root.join("destination");
        fs::create_dir_all(&destination).unwrap();
        let source = root.join("folder");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a.txt"), b"a").unwrap();

        // Left by an older operation before this one was journaled.
        let older = destination.join(stage_name("copy"));
        fs::create_dir(&older).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let own = destination.join(stage_name("copy"));
        fs::create_dir(&own).unwrap();
        fs::write(own.join("a.txt"), b"a").unwrap();
        // A folder copy never stages a plain file.
        let foreign_kind = destination.join(stage_name("copy"));
        fs::write(&foreign_kind, b"not a folder").unwrap();
        let recorded_at = created(&older) + CREATION_TIME_SLACK + Duration::from_millis(15);
        assert!(created(&own) + CREATION_TIME_SLACK >= recorded_at);
        write_previous_run(
            &root,
            vec![(
                request(FileOperationKind::Copy, source, destination.clone()),
                recorded_at,
            )],
        );

        let (store, interrupted, _) = OperationRecoveryStore::open(&root);
        let store = store.unwrap();
        assert_eq!(interrupted.len(), 1);
        // Staged by an operation of this run after the journal was opened.
        let live = destination.join(stage_name("copy"));
        fs::create_dir(&live).unwrap();

        let operation = store.claim_for_retry(interrupted[0].id()).unwrap().unwrap();
        operation.prepare_retry().unwrap();
        assert!(!own.exists());
        assert!(older.exists());
        assert!(foreign_kind.exists());
        assert!(live.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retry_claims_are_exclusive_and_visible_to_every_clone() {
        let root = fixture("claims");
        let destination = root.join("destination");
        fs::create_dir_all(&destination).unwrap();
        let source = root.join("item.txt");
        fs::write(&source, b"item").unwrap();
        let ids = write_previous_run(
            &root,
            vec![(
                request(FileOperationKind::Copy, source, destination),
                SystemTime::now(),
            )],
        );
        let (store, interrupted, _) = OperationRecoveryStore::open(&root);
        let first_window = store.unwrap();
        let second_window = first_window.clone();

        let claimed = first_window.claim_for_retry(&ids[0]).unwrap().unwrap();
        assert!(second_window.claim_for_retry(&ids[0]).unwrap().is_none());
        assert!(
            second_window
                .interrupted_if_changed(&interrupted)
                .unwrap()
                .is_empty()
        );
        // The claim re-stamps the entry for this run, so it is recovered again
        // if this process crashes during the retry.
        let (_, same_run, _) = OperationRecoveryStore::open(&root);
        assert!(same_run.is_empty());
        let (_, next_run, _) = OperationRecoveryStore::open_as_other_run(&root);
        assert_eq!(next_run.len(), 1);

        first_window.release(claimed);
        assert_eq!(second_window.interrupted().len(), 1);
        second_window.remove(&ids).unwrap();
        assert!(first_window.interrupted().is_empty());
        assert!(!root.join(JOURNAL_FILE).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn set_aside_source_of_a_cross_volume_move_is_restored_and_then_retryable() {
        let root = fixture("set-aside");
        let source_dir = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&destination).unwrap();
        let source = source_dir.join("item.txt");
        let recorded_at = SystemTime::now();
        // The verified copy was staged and the source set aside, then the
        // process died before committing.
        let stage = destination.join(stage_name("move"));
        fs::write(&stage, b"only copy").unwrap();
        let set_aside = source_dir.join(stage_name("source"));
        fs::write(&set_aside, b"only copy").unwrap();
        let ids = write_previous_run(
            &root,
            vec![(
                request(FileOperationKind::Move, source.clone(), destination.clone()),
                recorded_at,
            )],
        );

        let (store, interrupted, _) = OperationRecoveryStore::open(&root);
        let store = store.unwrap();
        assert_eq!(
            interrupted[0].disposition(),
            RecoveryDisposition::NeedsReview
        );
        assert_eq!(
            interrupted[0].artifacts(),
            [RecoveryArtifact {
                kind: RecoveryArtifactKind::SetAsideSource,
                path: set_aside.clone(),
                original: source.clone(),
                restorable: true,
            }]
        );

        assert_eq!(store.restore_artifacts(&ids[0]).0, 1);
        assert_eq!(fs::read(&source).unwrap(), b"only copy");
        assert!(!set_aside.exists());
        let restored = store.interrupted().remove(0);
        assert!(restored.artifacts().is_empty());
        assert_eq!(restored.disposition(), RecoveryDisposition::SafeToRetry);
        store
            .claim_for_retry(&ids[0])
            .unwrap()
            .unwrap()
            .prepare_retry()
            .unwrap();
        assert!(!stage.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn item_set_aside_by_replace_is_restored_only_while_its_name_is_free() {
        let root = fixture("replace");
        let destination = root.join("destination");
        fs::create_dir_all(&destination).unwrap();
        let source = root.join("item.txt");
        fs::write(&source, b"new").unwrap();
        let target = destination.join("item.txt");
        let backup = destination.join(stage_name("backup"));
        fs::write(&backup, b"old").unwrap();
        let mut replace = request(FileOperationKind::Copy, source, destination);
        replace.conflict_policy = ConflictPolicy::Replace;
        let ids = write_previous_run(&root, vec![(replace, SystemTime::now())]);

        let (store, interrupted, _) = OperationRecoveryStore::open(&root);
        let store = store.unwrap();
        assert_eq!(interrupted[0].artifacts().len(), 1);
        assert_eq!(interrupted[0].artifacts()[0].original(), target);
        assert!(interrupted[0].artifacts()[0].restorable());
        // Something took the name after launch: nothing is overwritten and the
        // location stays listed for review.
        fs::write(&target, b"someone else").unwrap();
        let (restored, failure) = store.restore_artifacts(&ids[0]);
        assert_eq!(restored, 0);
        assert_eq!(failure.unwrap().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&target).unwrap(), b"someone else");
        let remaining = store.interrupted().remove(0);
        assert_eq!(remaining.artifacts()[0].path(), backup);
        assert!(!remaining.artifacts()[0].restorable());

        fs::remove_file(&target).unwrap();
        let (_, interrupted, _) = OperationRecoveryStore::open_as_other_run(&root);
        assert!(interrupted[0].artifacts()[0].restorable());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_source_of_a_replacing_move_survives_other_retries_and_is_restored() {
        let root = fixture("staged-source");
        let source_dir = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&destination).unwrap();
        // A same-volume replacing move renamed its source into the
        // destination, keeping the source's original creation time.
        let staged = destination.join(stage_name("move"));
        fs::write(&staged, b"moved data").unwrap();
        let recorded_at = created(&staged) + CREATION_TIME_SLACK + Duration::from_secs(1);
        let replaced_source = source_dir.join("replaced.txt");
        fs::write(destination.join("replaced.txt"), b"old target").unwrap();
        let mut replacing = request(
            FileOperationKind::Move,
            replaced_source.clone(),
            destination.clone(),
        );
        replacing.conflict_policy = ConflictPolicy::Replace;
        // A second operation into the same folder that is safe to retry.
        let retry_source = source_dir.join("retry.txt");
        fs::write(&retry_source, b"retry").unwrap();
        let ids = write_previous_run(
            &root,
            vec![
                (replacing, recorded_at),
                (
                    request(FileOperationKind::Move, retry_source, destination),
                    recorded_at,
                ),
            ],
        );

        let (store, interrupted, _) = OperationRecoveryStore::open(&root);
        let store = store.unwrap();
        assert_eq!(
            interrupted[1].disposition(),
            RecoveryDisposition::SafeToRetry
        );
        store
            .claim_for_retry(&ids[1])
            .unwrap()
            .unwrap()
            .prepare_retry()
            .unwrap();
        assert!(staged.exists());

        let artifacts = interrupted[0].artifacts();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].kind(), RecoveryArtifactKind::StagedSource);
        assert!(artifacts[0].restorable());
        assert_eq!(store.restore_artifacts(&ids[0]).0, 1);
        assert_eq!(fs::read(&replaced_source).unwrap(), b"moved data");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ambiguous_hidden_items_are_listed_but_never_restored() {
        let root = fixture("ambiguous");
        let source_dir = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&destination).unwrap();
        let set_aside = source_dir.join(stage_name("source"));
        fs::write(&set_aside, b"whose?").unwrap();
        let now = SystemTime::now();
        write_previous_run(
            &root,
            vec![
                (
                    request(
                        FileOperationKind::Move,
                        source_dir.join("first.txt"),
                        destination.clone(),
                    ),
                    now,
                ),
                (
                    request(
                        FileOperationKind::Move,
                        source_dir.join("second.txt"),
                        destination,
                    ),
                    now,
                ),
            ],
        );

        let (_, interrupted, _) = OperationRecoveryStore::open(&root);
        assert_eq!(interrupted.len(), 2);
        for operation in &interrupted {
            assert_eq!(operation.artifacts().len(), 1);
            assert_eq!(operation.artifacts()[0].path(), set_aside);
            assert!(!operation.artifacts()[0].restorable());
        }

        // Two candidates for one operation are just as ambiguous.
        fs::write(source_dir.join(stage_name("source")), b"or this?").unwrap();
        let (_, interrupted, _) = OperationRecoveryStore::open(&root);
        assert!(
            interrupted
                .iter()
                .flat_map(InterruptedOperation::artifacts)
                .all(|artifact| !artifact.restorable())
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_journal_is_preserved_and_disables_writes() {
        let root = fixture("corrupt");
        let journal = root.join(JOURNAL_FILE);
        fs::write(&journal, b"not json").unwrap();
        let (store, interrupted, warning) = OperationRecoveryStore::open(&root);
        assert!(store.is_none());
        assert!(interrupted.is_empty());
        assert!(warning.is_some());
        assert_eq!(fs::read(&journal).unwrap(), b"not json");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn destructive_operations_are_never_journaled() {
        let root =
            std::env::temp_dir().join(format!("explorie-operation-trash-{}", Uuid::new_v4()));
        let (store, _, _) = OperationRecoveryStore::open(&root);
        let store = store.unwrap();
        let trash = FileOperationRequest {
            kind: FileOperationKind::Trash,
            sources: vec![root.join("item")],
            destination: None,
            conflict_policy: ConflictPolicy::Error,
        };
        assert!(store.record(&trash).unwrap().is_empty());
        assert!(!root.join(JOURNAL_FILE).exists());
    }
}
