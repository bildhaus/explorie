use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use explorie_native_services::{
    FileOperationEvent, FileOperationProgress, FileOperationRequest, FileOperationResult,
    FileOperationState, FileTreeSnapshot,
};

const OPERATION_HISTORY_LIMIT: usize = 50;
const UNDO_HISTORY_LIMIT: usize = 50;
const UNDO_HISTORY_BYTES: usize = 24 * 1024 * 1024;
pub const UNDO_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Minimum spacing between redraws caused only by transfer progress (~10 Hz).
pub const PROGRESS_REDRAW_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationStatus {
    Running,
    Completed,
    Cancelled,
    Failed,
    /// Stopped at a destination conflict the user is being asked about; the
    /// record settles once they choose Skip, Replace or Keep Both.
    NeedsDecision,
    /// Every remaining item was skipped at a conflict prompt and nothing was
    /// transferred.
    Skipped,
}

impl OperationStatus {
    /// Whether the operation has ended for good (not running or waiting for
    /// the user), so it can be cleared from the history.
    pub fn is_settled(self) -> bool {
        !matches!(self, Self::Running | Self::NeedsDecision)
    }
}

#[derive(Clone, Debug)]
pub struct OperationRecord {
    id: String,
    request: FileOperationRequest,
    status: OperationStatus,
    progress: Option<FileOperationProgress>,
    result: Option<FileOperationResult>,
    retryable_sources: Vec<PathBuf>,
    error: Option<String>,
    undo_recorded: bool,
    /// Items in the user's operation, which outlives the native job when a
    /// conflict resolution continues it under a new job.
    total_items: usize,
    /// Items earlier jobs of this operation already transferred.
    completed_items: usize,
    /// Items the user skipped at conflict prompts.
    skipped_items: usize,
}

impl OperationRecord {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn request(&self) -> &FileOperationRequest {
        &self.request
    }

    pub fn status(&self) -> OperationStatus {
        self.status
    }

    pub fn progress(&self) -> Option<&FileOperationProgress> {
        self.progress.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn retryable_count(&self) -> usize {
        self.retryable_sources.len()
    }

    pub fn total_items(&self) -> usize {
        self.total_items
    }

    pub fn skipped_items(&self) -> usize {
        self.skipped_items
    }

    /// Items transferred so far by this operation, across its jobs.
    fn transferred_items(&self) -> usize {
        self.completed_items
            + self
                .result
                .as_ref()
                .map_or(0, |result| result.targets.len())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressRedraw {
    /// Redraw immediately.
    Now,
    /// Skip this redraw and schedule a single catch-up redraw after the delay.
    After(Duration),
    /// A catch-up redraw is already scheduled and will show this update.
    Pending,
}

/// Coalesces progress-only redraws, which arrive once per copied chunk, while
/// guaranteeing the latest progress is drawn within one interval.
#[derive(Debug, Default)]
pub struct ProgressThrottle {
    last_redraw: Option<Instant>,
    catch_up_scheduled: bool,
}

impl ProgressThrottle {
    pub fn progress(&mut self, now: Instant) -> ProgressRedraw {
        if self.catch_up_scheduled {
            return ProgressRedraw::Pending;
        }
        match self.last_redraw {
            Some(last) if now.saturating_duration_since(last) < PROGRESS_REDRAW_INTERVAL => {
                self.catch_up_scheduled = true;
                ProgressRedraw::After(
                    PROGRESS_REDRAW_INTERVAL - now.saturating_duration_since(last),
                )
            }
            _ => {
                self.last_redraw = Some(now);
                ProgressRedraw::Now
            }
        }
    }

    pub fn caught_up(&mut self, now: Instant) {
        self.catch_up_scheduled = false;
        self.last_redraw = Some(now);
    }
}

#[derive(Debug, Default)]
pub struct OperationQueue {
    operations: Vec<OperationRecord>,
    progress_throttle: ProgressThrottle,
}

impl OperationQueue {
    pub fn progress_throttle(&mut self) -> &mut ProgressThrottle {
        &mut self.progress_throttle
    }

    pub fn track(&mut self, id: String, request: FileOperationRequest) {
        self.operations.retain(|operation| operation.id != id);
        let total_items = request.sources.len();
        self.operations.push(OperationRecord {
            id,
            request,
            status: OperationStatus::Running,
            progress: None,
            result: None,
            retryable_sources: Vec::new(),
            error: None,
            undo_recorded: false,
            total_items,
            completed_items: 0,
            skipped_items: 0,
        });
        while self.operations.len() > OPERATION_HISTORY_LIMIT {
            let Some(index) = self
                .operations
                .iter()
                .position(|operation| operation.status.is_settled())
            else {
                break;
            };
            self.operations.remove(index);
        }
    }

    pub fn apply(&mut self, event: FileOperationEvent) -> bool {
        let Some(operation) = self
            .operations
            .iter_mut()
            .find(|operation| operation.id == event.job_id)
        else {
            return false;
        };
        if let Some(progress) = event.progress {
            operation.progress = Some(progress);
        }
        operation.status = match event.state {
            FileOperationState::Running => OperationStatus::Running,
            FileOperationState::Completed => OperationStatus::Completed,
            FileOperationState::Cancelled => OperationStatus::Cancelled,
            FileOperationState::Failed => OperationStatus::Failed,
        };
        operation.result = event.result;
        operation.retryable_sources = event.retryable_sources;
        operation.error = event.error.map(|error| error.to_string());
        true
    }

    pub fn operations(&self) -> &[OperationRecord] {
        &self.operations
    }

    #[cfg(test)]
    pub fn latest(&self) -> Option<&OperationRecord> {
        self.operations.last()
    }

    pub fn latest_running_id(&self) -> Option<&str> {
        self.operations
            .iter()
            .rev()
            .find(|operation| operation.status == OperationStatus::Running)
            .map(OperationRecord::id)
    }

    pub fn active_count(&self) -> usize {
        self.operations
            .iter()
            .filter(|operation| operation.status == OperationStatus::Running)
            .count()
    }

    pub fn latest_retryable_id(&self) -> Option<&str> {
        self.operations
            .iter()
            .rev()
            .find(|operation| {
                !operation.retryable_sources.is_empty()
                    && operation.request.kind != explorie_native_services::FileOperationKind::Trash
            })
            .map(OperationRecord::id)
    }

    pub fn retry_request(&self, id: &str) -> Option<FileOperationRequest> {
        let operation = self
            .operations
            .iter()
            .find(|operation| operation.id == id)?;
        if operation.retryable_sources.is_empty()
            || operation.request.kind == explorie_native_services::FileOperationKind::Trash
        {
            return None;
        }
        let mut request = operation.request.clone();
        request.sources = operation.retryable_sources.clone();
        Some(request)
    }

    pub fn retryable_count(&self, id: &str) -> usize {
        self.operations
            .iter()
            .find(|operation| operation.id == id)
            .map_or(0, |operation| operation.retryable_sources.len())
    }

    pub fn mark_retry_started(&mut self, id: &str) {
        if let Some(operation) = self
            .operations
            .iter_mut()
            .find(|operation| operation.id == id)
        {
            operation.retryable_sources.clear();
        }
    }

    /// Show a job stopped at a destination conflict as waiting for the
    /// user's decision instead of as a failure.
    pub fn mark_needs_decision(&mut self, id: &str) -> bool {
        let Some(operation) = self
            .operations
            .iter_mut()
            .find(|operation| operation.id == id)
        else {
            return false;
        };
        operation.status = OperationStatus::NeedsDecision;
        operation.error = None;
        true
    }

    /// Settle an operation waiting at a conflict without transferring its
    /// `unresolved` remaining items: skipped, or cancelled with Cancel All.
    /// An operation that transferred nothing at all reads as Skipped.
    pub fn settle_decision(&mut self, id: &str, unresolved: usize, cancelled: bool) -> bool {
        let Some(operation) = self
            .operations
            .iter_mut()
            .find(|operation| operation.id == id)
        else {
            return false;
        };
        operation.retryable_sources.clear();
        operation.error = None;
        operation.skipped_items += unresolved;
        operation.status = if cancelled {
            OperationStatus::Cancelled
        } else if operation.transferred_items() == 0 {
            OperationStatus::Skipped
        } else {
            OperationStatus::Completed
        };
        true
    }

    /// Continue the operation `previous_id` under the job `next_id` started to
    /// carry out a conflict decision: the new job takes the previous record's
    /// place and its counts, so the history shows one operation with the
    /// outcome of the decision rather than a failure plus a second entry.
    pub fn supersede(&mut self, previous_id: &str, next_id: &str, skipped: usize) -> bool {
        let Some(previous_index) = self
            .operations
            .iter()
            .position(|operation| operation.id == previous_id)
        else {
            return false;
        };
        let Some(next_index) = self
            .operations
            .iter()
            .position(|operation| operation.id == next_id)
        else {
            return false;
        };
        if previous_index == next_index {
            return false;
        }
        let mut next = self.operations.remove(next_index);
        let previous_index = if next_index < previous_index {
            previous_index - 1
        } else {
            previous_index
        };
        let previous = &self.operations[previous_index];
        next.total_items = previous.total_items;
        next.completed_items = previous.transferred_items();
        next.skipped_items = previous.skipped_items + skipped;
        self.operations[previous_index] = next;
        true
    }

    pub fn clear_completed(&mut self) {
        self.operations
            .retain(|operation| !operation.status.is_settled());
    }

    pub fn remove_finished(&mut self, id: &str) -> bool {
        let Some(index) = self
            .operations
            .iter()
            .position(|operation| operation.id == id)
        else {
            return false;
        };
        if !self.operations[index].status.is_settled() {
            return false;
        }
        self.operations.remove(index);
        true
    }

    pub fn take_undo_record(&mut self, id: &str) -> Option<UndoRecord> {
        let operation = self
            .operations
            .iter_mut()
            .find(|operation| operation.id == id)?;
        if operation.undo_recorded || operation.status == OperationStatus::Running {
            return None;
        }
        operation.undo_recorded = true;
        let result = operation.result.as_ref()?.clone();
        if result.targets.is_empty() {
            return None;
        }
        let mut request = operation.request.clone();
        request.sources.truncate(result.targets.len());
        UndoRecord::from_file_operation(request, result)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardKind {
    Copy,
    Cut,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardState {
    pub kind: ClipboardKind,
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CreatedKind {
    Folder,
    Note,
    WebsiteLink { url: String },
}

#[derive(Clone, Debug)]
pub enum UndoAction {
    Copy {
        request: FileOperationRequest,
        targets: Vec<PathBuf>,
        snapshots: Vec<FileTreeSnapshot>,
    },
    Move {
        request: FileOperationRequest,
        pairs: Vec<(PathBuf, PathBuf)>,
    },
    Create {
        kind: CreatedKind,
        path: PathBuf,
    },
    Rename {
        before: PathBuf,
        after: PathBuf,
    },
    BatchRename {
        pairs: Vec<(PathBuf, PathBuf)>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PathFingerprint {
    kind: u8,
    len: u64,
    modified_ns: Option<u128>,
    platform_id: Option<(u64, u64)>,
}

#[derive(Clone, Debug)]
struct ExpectedPathState {
    path: PathBuf,
    fingerprint: Option<PathFingerprint>,
}

fn path_fingerprint(path: &Path) -> Option<PathFingerprint> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let kind = if metadata.file_type().is_symlink() {
        2
    } else if metadata.is_dir() {
        1
    } else {
        0
    };
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
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
    Some(PathFingerprint {
        kind,
        len: metadata.len(),
        modified_ns,
        platform_id,
    })
}

fn expected_path(path: PathBuf) -> ExpectedPathState {
    ExpectedPathState {
        fingerprint: path_fingerprint(&path),
        path,
    }
}

fn undo_paths(action: &UndoAction) -> Vec<PathBuf> {
    match action {
        UndoAction::Copy { targets, .. } => targets.clone(),
        UndoAction::Move { pairs, .. } => pairs.iter().map(|(_, target)| target.clone()).collect(),
        UndoAction::Create { path, .. } => vec![path.clone()],
        UndoAction::Rename { after, .. } => vec![after.clone()],
        UndoAction::BatchRename { pairs } => pairs.iter().map(|(_, after)| after.clone()).collect(),
    }
}

fn redo_paths(action: &UndoAction) -> Vec<PathBuf> {
    match action {
        UndoAction::Copy {
            request, targets, ..
        } => request
            .sources
            .iter()
            .cloned()
            .chain(targets.iter().cloned())
            .collect(),
        UndoAction::Move { pairs, .. } | UndoAction::BatchRename { pairs } => pairs
            .iter()
            .flat_map(|(before, after)| [before.clone(), after.clone()])
            .collect(),
        UndoAction::Create { path, .. } => vec![path.clone()],
        UndoAction::Rename { before, after } => vec![before.clone(), after.clone()],
    }
}

#[derive(Clone, Debug)]
pub struct UndoRecord {
    description: String,
    created_at: SystemTime,
    bytes: usize,
    expected_paths: Vec<ExpectedPathState>,
    pub action: UndoAction,
}

impl UndoRecord {
    pub fn from_file_operation(
        request: FileOperationRequest,
        result: FileOperationResult,
    ) -> Option<Self> {
        if request.conflict_policy == explorie_native_services::ConflictPolicy::Replace {
            return None;
        }
        let count = request.sources.len();
        let (description, action) = match request.kind {
            explorie_native_services::FileOperationKind::Copy if !result.targets.is_empty() => (
                format!("Copy {count} item(s)"),
                UndoAction::Copy {
                    request,
                    targets: result.targets,
                    snapshots: result.target_snapshots,
                },
            ),
            explorie_native_services::FileOperationKind::Move
                if result.targets.len() == request.sources.len() =>
            {
                let pairs = request
                    .sources
                    .iter()
                    .cloned()
                    .zip(result.targets)
                    .collect();
                (
                    format!("Move {count} item(s)"),
                    UndoAction::Move { request, pairs },
                )
            }
            explorie_native_services::FileOperationKind::Copy
            | explorie_native_services::FileOperationKind::Move
            | explorie_native_services::FileOperationKind::Trash => return None,
        };
        Some(Self::new(description, action))
    }

    pub fn created(kind: CreatedKind, path: PathBuf) -> Self {
        let label = match &kind {
            CreatedKind::Folder => "folder",
            CreatedKind::Note => "note",
            CreatedKind::WebsiteLink { .. } => "website link",
        };
        Self::new(format!("Create {label}"), UndoAction::Create { kind, path })
    }

    pub fn renamed(before: PathBuf, after: PathBuf) -> Self {
        Self::new(
            "Rename item".to_string(),
            UndoAction::Rename { before, after },
        )
    }

    pub fn batch_renamed(pairs: Vec<(PathBuf, PathBuf)>) -> Self {
        let count = pairs.len();
        Self::new(
            format!("Rename {count} items"),
            UndoAction::BatchRename { pairs },
        )
    }

    fn new(description: String, action: UndoAction) -> Self {
        let bytes = description.len() + action_path_bytes(&action);
        Self {
            description,
            created_at: SystemTime::now(),
            bytes,
            expected_paths: undo_paths(&action).into_iter().map(expected_path).collect(),
            action,
        }
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn validate_expected_paths(&self) -> Result<(), String> {
        for expected in &self.expected_paths {
            let actual = path_fingerprint(&expected.path);
            if actual != expected.fingerprint {
                return Err(format!(
                    "{} changed after the operation; refusing to modify it",
                    expected.path.display()
                ));
            }
        }
        Ok(())
    }

    pub fn capture_undo_paths(&mut self) {
        self.expected_paths = undo_paths(&self.action)
            .into_iter()
            .map(expected_path)
            .collect();
    }

    pub fn capture_redo_paths(&mut self) {
        self.expected_paths = redo_paths(&self.action)
            .into_iter()
            .map(expected_path)
            .collect();
    }

    fn expired_at(&self, now: SystemTime, timeout: Duration) -> bool {
        now.duration_since(self.created_at)
            .is_ok_and(|elapsed| elapsed >= timeout)
    }
}

#[derive(Debug)]
pub struct UndoLedger {
    undo: Vec<UndoRecord>,
    redo: Vec<UndoRecord>,
    processing: bool,
    timeout: Duration,
}

impl Default for UndoLedger {
    fn default() -> Self {
        Self::with_timeout(UNDO_TIMEOUT)
    }
}

impl UndoLedger {
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            processing: false,
            timeout,
        }
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn push(&mut self, record: UndoRecord) {
        self.undo.push(record);
        self.redo.clear();
        trim_undo_stack(&mut self.undo);
    }

    pub fn can_undo(&self, now: SystemTime) -> bool {
        !self.processing
            && self
                .undo
                .last()
                .is_some_and(|record| !record.expired_at(now, self.timeout))
    }

    pub fn can_redo(&self) -> bool {
        !self.processing && !self.redo.is_empty()
    }

    pub fn is_processing(&self) -> bool {
        self.processing
    }

    pub fn begin_undo(&mut self, now: SystemTime) -> Option<UndoRecord> {
        self.prune_expired(now);
        if self.processing {
            return None;
        }
        let record = self.undo.pop()?;
        self.processing = true;
        Some(record)
    }

    pub fn begin_redo(&mut self) -> Option<UndoRecord> {
        if self.processing {
            return None;
        }
        let record = self.redo.pop()?;
        self.processing = true;
        Some(record)
    }

    pub fn finish_undo(&mut self, record: UndoRecord, succeeded: bool) {
        self.processing = false;
        if succeeded {
            self.redo.push(record);
            trim_undo_stack(&mut self.redo);
        } else {
            self.undo.push(record);
            trim_undo_stack(&mut self.undo);
        }
    }

    pub fn finish_redo(&mut self, record: UndoRecord, succeeded: bool) {
        self.processing = false;
        if succeeded {
            self.undo.push(record);
            trim_undo_stack(&mut self.undo);
        } else {
            self.redo.push(record);
            trim_undo_stack(&mut self.redo);
        }
    }

    pub fn prune_expired(&mut self, now: SystemTime) {
        self.undo
            .retain(|record| !record.expired_at(now, self.timeout));
    }
}

fn action_path_bytes(action: &UndoAction) -> usize {
    let path_bytes = |path: &PathBuf| path.as_os_str().to_string_lossy().len();
    match action {
        UndoAction::Copy {
            request,
            targets,
            snapshots,
        } => {
            request.sources.iter().map(&path_bytes).sum::<usize>()
                + request.destination.as_ref().map_or(0, &path_bytes)
                + targets.iter().map(path_bytes).sum::<usize>()
                + snapshots
                    .iter()
                    .map(FileTreeSnapshot::estimated_bytes)
                    .sum::<usize>()
        }
        UndoAction::Move { request, pairs } => {
            request.sources.iter().map(&path_bytes).sum::<usize>()
                + request.destination.as_ref().map_or(0, &path_bytes)
                + pairs
                    .iter()
                    .map(|(source, target)| path_bytes(source) + path_bytes(target))
                    .sum::<usize>()
        }
        UndoAction::Create { kind, path } => {
            path_bytes(path)
                + match kind {
                    CreatedKind::WebsiteLink { url } => url.len(),
                    CreatedKind::Folder | CreatedKind::Note => 0,
                }
        }
        UndoAction::Rename { before, after } => path_bytes(before) + path_bytes(after),
        UndoAction::BatchRename { pairs } => pairs
            .iter()
            .map(|(before, after)| path_bytes(before) + path_bytes(after))
            .sum(),
    }
}

fn trim_undo_stack(stack: &mut Vec<UndoRecord>) {
    while stack.len() > UNDO_HISTORY_LIMIT {
        stack.remove(0);
    }
    let mut bytes = stack.iter().map(|record| record.bytes).sum::<usize>();
    while bytes > UNDO_HISTORY_BYTES && !stack.is_empty() {
        bytes = bytes.saturating_sub(stack.remove(0).bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorie_native_services::{ConflictPolicy, ErrorCode, FileOperationKind, ServiceError};

    fn request() -> FileOperationRequest {
        FileOperationRequest {
            kind: FileOperationKind::Copy,
            sources: vec![PathBuf::from("source")],
            destination: Some(PathBuf::from("destination")),
            conflict_policy: ConflictPolicy::Rename,
        }
    }

    #[test]
    fn progress_redraws_are_coalesced_to_the_interval_with_one_catch_up() {
        let mut throttle = ProgressThrottle::default();
        let start = Instant::now();
        assert_eq!(throttle.progress(start), ProgressRedraw::Now);
        assert_eq!(
            throttle.progress(start + Duration::from_millis(30)),
            ProgressRedraw::After(Duration::from_millis(70))
        );
        for offset in [40, 60, 99] {
            assert_eq!(
                throttle.progress(start + Duration::from_millis(offset)),
                ProgressRedraw::Pending
            );
        }
        throttle.caught_up(start + Duration::from_millis(100));
        assert!(matches!(
            throttle.progress(start + Duration::from_millis(150)),
            ProgressRedraw::After(_)
        ));
        throttle.caught_up(start + Duration::from_millis(200));
        assert_eq!(
            throttle.progress(start + Duration::from_millis(300)),
            ProgressRedraw::Now
        );
    }

    #[test]
    fn queue_tracks_aggregate_progress_completion_and_unknown_events() {
        let mut queue = OperationQueue::default();
        queue.track("job".into(), request());
        assert_eq!(queue.active_count(), 1);
        assert!(queue.apply(FileOperationEvent {
            job_id: "job".into(),
            state: FileOperationState::Running,
            progress: Some(FileOperationProgress {
                processed_entries: 2,
                total_entries: 4,
                processed_bytes: 10,
                total_bytes: 20,
                current_path: Some(PathBuf::from("source")),
            }),
            result: None,
            retryable_sources: Vec::new(),
            error: None,
        }));
        assert_eq!(
            queue.operations()[0].progress().unwrap().processed_bytes,
            10
        );
        assert!(queue.apply(FileOperationEvent {
            job_id: "job".into(),
            state: FileOperationState::Completed,
            progress: None,
            result: Some(FileOperationResult {
                processed_entries: 4,
                processed_bytes: 20,
                targets: vec![PathBuf::from("destination/source")],
                target_snapshots: Vec::new(),
            }),
            retryable_sources: Vec::new(),
            error: None,
        }));
        assert_eq!(queue.active_count(), 0);
        assert_eq!(queue.operations()[0].status(), OperationStatus::Completed);
        assert!(!queue.apply(FileOperationEvent {
            job_id: "unknown".into(),
            state: FileOperationState::Cancelled,
            progress: None,
            result: None,
            retryable_sources: Vec::new(),
            error: None,
        }));
    }

    fn conflict_event(
        job_id: &str,
        transferred: &[&str],
        unresolved: &[&str],
    ) -> FileOperationEvent {
        FileOperationEvent {
            job_id: job_id.into(),
            state: FileOperationState::Failed,
            progress: None,
            result: Some(FileOperationResult {
                processed_entries: transferred.len() as u64,
                processed_bytes: 0,
                targets: transferred.iter().map(PathBuf::from).collect(),
                target_snapshots: Vec::new(),
            }),
            retryable_sources: unresolved.iter().map(PathBuf::from).collect(),
            error: Some(ServiceError::new(
                ErrorCode::Conflict,
                "destination already exists",
            )),
        }
    }

    #[test]
    fn conflict_decisions_settle_one_history_entry_with_their_outcome() {
        let mut queue = OperationQueue::default();
        let mut three = request();
        three.sources = vec!["a".into(), "b".into(), "c".into()];
        queue.track("job-1".into(), three);
        assert!(queue.apply(conflict_event("job-1", &["destination/a"], &["b", "c"])));
        assert!(queue.mark_needs_decision("job-1"));
        let waiting = &queue.operations()[0];
        assert_eq!(waiting.status(), OperationStatus::NeedsDecision);
        assert_eq!(waiting.error(), None);
        // The Retry shortcut can still try again, e.g. after the user moved
        // the existing file away; the panel offers the prompt instead.
        assert_eq!(queue.latest_retryable_id(), Some("job-1"));
        // Waiting for the user is neither running nor finished.
        assert_eq!(queue.active_count(), 0);
        queue.clear_completed();
        assert!(!queue.remove_finished("job-1"));
        assert_eq!(queue.operations().len(), 1);

        // Skipping "b" continues with "c" under a new job in the same entry.
        let mut rest = request();
        rest.sources = vec!["c".into()];
        queue.track("job-2".into(), rest);
        assert!(queue.supersede("job-1", "job-2", 1));
        assert_eq!(queue.operations().len(), 1);
        let continued = &queue.operations()[0];
        assert_eq!(continued.id(), "job-2");
        assert_eq!(continued.status(), OperationStatus::Running);
        assert_eq!(continued.total_items(), 3);
        assert_eq!(continued.skipped_items(), 1);

        // Skipping the last conflict still completes: "a" was transferred.
        assert!(queue.apply(conflict_event("job-2", &[], &["c"])));
        assert!(queue.mark_needs_decision("job-2"));
        assert!(queue.settle_decision("job-2", 1, false));
        let settled = &queue.operations()[0];
        assert_eq!(settled.status(), OperationStatus::Completed);
        assert_eq!(settled.skipped_items(), 2);
        assert_eq!(settled.retryable_count(), 0);

        // An operation whose only item is skipped reads as Skipped, and
        // Cancel All reads as Cancelled.
        queue.track("job-3".into(), request());
        assert!(queue.apply(conflict_event("job-3", &[], &["source"])));
        assert!(queue.settle_decision("job-3", 1, false));
        assert_eq!(queue.operations()[1].status(), OperationStatus::Skipped);
        queue.track("job-4".into(), request());
        assert!(queue.apply(conflict_event("job-4", &[], &["source"])));
        assert!(queue.settle_decision("job-4", 1, true));
        assert_eq!(queue.operations()[2].status(), OperationStatus::Cancelled);
        queue.clear_completed();
        assert!(queue.operations().is_empty());
    }

    #[test]
    fn completed_non_replacing_jobs_create_bounded_undo_records_once() {
        let mut queue = OperationQueue::default();
        queue.track("job".into(), request());
        assert!(queue.apply(FileOperationEvent {
            job_id: "job".into(),
            state: FileOperationState::Completed,
            progress: None,
            result: Some(FileOperationResult {
                processed_entries: 1,
                processed_bytes: 4,
                targets: vec![PathBuf::from("destination/source")],
                target_snapshots: Vec::new(),
            }),
            retryable_sources: Vec::new(),
            error: None,
        }));

        let record = queue.take_undo_record("job").expect("undo record");
        assert_eq!(record.description(), "Copy 1 item(s)");
        assert!(matches!(record.action, UndoAction::Copy { .. }));
        assert!(queue.take_undo_record("job").is_none());

        let mut replacing = request();
        replacing.conflict_policy = ConflictPolicy::Replace;
        assert!(
            UndoRecord::from_file_operation(
                replacing,
                FileOperationResult {
                    processed_entries: 1,
                    processed_bytes: 4,
                    targets: vec![PathBuf::from("destination/source")],
                    target_snapshots: Vec::new(),
                },
            )
            .is_none()
        );
    }

    #[test]
    fn undo_ledger_expires_records_and_clears_redo_on_new_mutation() {
        let now = SystemTime::now();
        let mut ledger = UndoLedger::default();
        let first = UndoRecord::created(CreatedKind::Folder, PathBuf::from("first"));
        ledger.push(first);
        let undone = ledger.begin_undo(now).expect("undoable record");
        ledger.finish_undo(undone, true);
        assert!(ledger.can_redo());

        ledger.push(UndoRecord::created(
            CreatedKind::Note,
            PathBuf::from("second.md"),
        ));
        assert!(!ledger.can_redo());
        ledger.undo.last_mut().unwrap().created_at = now - UNDO_TIMEOUT;
        assert!(!ledger.can_undo(now));
        assert!(ledger.begin_undo(now).is_none());
    }

    #[test]
    fn failed_undo_and_redo_return_records_to_their_original_stack() {
        let now = SystemTime::now();
        let mut ledger = UndoLedger::default();
        ledger.push(UndoRecord::renamed(
            PathBuf::from("before"),
            PathBuf::from("after"),
        ));
        let undo = ledger.begin_undo(now).unwrap();
        ledger.finish_undo(undo, false);
        assert!(ledger.can_undo(now));

        let undo = ledger.begin_undo(now).unwrap();
        ledger.finish_undo(undo, true);
        let redo = ledger.begin_redo().unwrap();
        ledger.finish_redo(redo, false);
        assert!(ledger.can_redo());
    }

    #[test]
    fn history_limit_never_discards_a_running_job() {
        let mut queue = OperationQueue::default();
        for index in 0..=OPERATION_HISTORY_LIMIT {
            queue.track(format!("job-{index}"), request());
        }
        assert_eq!(queue.operations().len(), OPERATION_HISTORY_LIMIT + 1);
        assert_eq!(queue.active_count(), OPERATION_HISTORY_LIMIT + 1);

        assert!(queue.apply(FileOperationEvent {
            job_id: "job-0".into(),
            state: FileOperationState::Completed,
            progress: None,
            result: Some(FileOperationResult {
                processed_entries: 1,
                processed_bytes: 1,
                targets: vec![PathBuf::from("destination/source")],
                target_snapshots: Vec::new(),
            }),
            retryable_sources: Vec::new(),
            error: None,
        }));
        queue.track("job-next".into(), request());
        assert_eq!(queue.operations().len(), OPERATION_HISTORY_LIMIT + 1);
        assert!(
            queue
                .operations()
                .iter()
                .all(|operation| operation.id() != "job-0")
        );
        assert_eq!(queue.active_count(), OPERATION_HISTORY_LIMIT + 1);
    }

    #[test]
    fn partial_failures_retry_only_unresolved_sources_and_keep_completed_undo() {
        let mut batch = request();
        batch.sources = vec![PathBuf::from("first"), PathBuf::from("second")];
        let mut queue = OperationQueue::default();
        queue.track("batch".into(), batch);
        assert!(queue.apply(FileOperationEvent {
            job_id: "batch".into(),
            state: FileOperationState::Failed,
            progress: None,
            result: Some(FileOperationResult {
                processed_entries: 1,
                processed_bytes: 5,
                targets: vec![PathBuf::from("destination/first")],
                target_snapshots: Vec::new(),
            }),
            retryable_sources: vec![PathBuf::from("second")],
            error: Some(ServiceError::new(ErrorCode::Conflict, "destination exists")),
        }));

        let retry = queue.retry_request("batch").expect("retry request");
        assert_eq!(retry.sources, vec![PathBuf::from("second")]);
        let undo = queue
            .take_undo_record("batch")
            .expect("completed prefix undo");
        match undo.action {
            UndoAction::Copy {
                request, targets, ..
            } => {
                assert_eq!(request.sources, vec![PathBuf::from("first")]);
                assert_eq!(targets, vec![PathBuf::from("destination/first")]);
            }
            other => panic!("unexpected undo action: {other:?}"),
        }
        queue.mark_retry_started("batch");
        assert!(queue.retry_request("batch").is_none());
    }

    #[test]
    fn trash_is_never_retryable_even_if_a_misbehaving_host_marks_a_source() {
        let mut trash = request();
        trash.kind = FileOperationKind::Trash;
        trash.destination = None;
        let mut queue = OperationQueue::default();
        queue.track("trash".into(), trash);
        assert!(queue.apply(FileOperationEvent {
            job_id: "trash".into(),
            state: FileOperationState::Failed,
            progress: None,
            result: Some(FileOperationResult {
                processed_entries: 0,
                processed_bytes: 0,
                targets: Vec::new(),
                target_snapshots: Vec::new(),
            }),
            retryable_sources: vec![PathBuf::from("source")],
            error: Some(ServiceError::new(ErrorCode::Io, "trash failed")),
        }));
        assert!(queue.retry_request("trash").is_none());
    }
}
