//! Process-wide state shared by every window.

use crate::*;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum WatcherDisposition {
    Refresh,
    Stop(String),
    Ignore,
}

pub(crate) struct PersistenceStores {
    pub(crate) session: Option<SessionStore>,
    pub(crate) settings: Option<SettingsStore>,
    pub(crate) workspaces: Option<WorkspaceStore>,
    pub(crate) operation_recovery: Option<OperationRecoveryStore>,
    pub(crate) interrupted_operations: Vec<InterruptedOperation>,
    pub(crate) window_lifetime: Option<WindowLifetime>,
}

#[derive(Clone)]
pub(crate) struct SharedApplicationState {
    pub(crate) settings: AppSettings,
    pub(crate) workspaces: WorkspaceState,
    pub(crate) session: SharedSessionState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SharedOperationSummary {
    pub(crate) id: String,
    pub(crate) owner_window_id: String,
    pub(crate) label: String,
    pub(crate) item_count: usize,
    pub(crate) destination: Option<PathBuf>,
    pub(crate) status: OperationStatus,
    pub(crate) processed_entries: u64,
    pub(crate) total_entries: u64,
    pub(crate) processed_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) current_path: Option<PathBuf>,
    pub(crate) error: Option<String>,
}

#[derive(Clone)]
pub struct WindowRuntime {
    pub(crate) registry: WindowSessionRegistry,
    pub(crate) live_windows: Arc<AtomicUsize>,
    pub(crate) quitting: Arc<AtomicBool>,
    pub(crate) shared_state: Arc<Mutex<Option<SharedApplicationState>>>,
    pub(crate) shared_state_revision: Arc<AtomicU64>,
    pub(crate) operations: Arc<Mutex<Vec<SharedOperationSummary>>>,
    pub(crate) operations_revision: Arc<AtomicU64>,
    pub(crate) settings_store: Arc<Mutex<Option<Arc<SettingsStore>>>>,
    pub(crate) workspace_store: Arc<Mutex<Option<Arc<WorkspaceStore>>>>,
    /// One recovery journal per process: windows sharing it cannot overwrite
    /// each other's entries, and only entries from a previous run are offered
    /// for recovery.
    pub(crate) operation_recovery: Option<OperationRecoveryStore>,
    pub(crate) operation_recovery_warning: Arc<Mutex<Option<String>>>,
}

impl WindowRuntime {
    pub fn open(config_dir: &Path) -> (Self, Vec<String>) {
        let (registry, session_ids) = WindowSessionRegistry::open(config_dir);
        let (operation_recovery, _, operation_recovery_warning) =
            OperationRecoveryStore::open(config_dir);
        (
            Self {
                registry,
                live_windows: Arc::new(AtomicUsize::new(0)),
                quitting: Arc::new(AtomicBool::new(false)),
                shared_state: Arc::new(Mutex::new(None)),
                shared_state_revision: Arc::new(AtomicU64::new(0)),
                operations: Arc::new(Mutex::new(Vec::new())),
                operations_revision: Arc::new(AtomicU64::new(0)),
                settings_store: Arc::new(Mutex::new(None)),
                workspace_store: Arc::new(Mutex::new(None)),
                operation_recovery,
                operation_recovery_warning: Arc::new(Mutex::new(operation_recovery_warning)),
            },
            session_ids,
        )
    }

    pub fn mark_quitting(&self) {
        self.quitting.store(true, Ordering::Release);
    }

    pub(crate) fn next_session_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    pub(crate) fn register(&self, id: &str) -> std::io::Result<()> {
        if id != "primary" {
            self.registry.add(id.to_string())?;
        }
        Ok(())
    }

    pub(crate) fn lifetime(&self, id: String) -> WindowLifetime {
        self.live_windows.fetch_add(1, Ordering::AcqRel);
        WindowLifetime {
            runtime: self.clone(),
            id,
        }
    }

    pub(crate) fn is_last_window(&self) -> bool {
        self.live_windows.load(Ordering::Acquire) <= 1
    }

    /// The shared recovery journal and the interrupted operations still
    /// awaiting a decision. Only the first window receives the journal's
    /// warning, if any.
    pub(crate) fn operation_recovery(
        &self,
    ) -> (
        Option<OperationRecoveryStore>,
        Vec<InterruptedOperation>,
        Option<String>,
    ) {
        let warning = self
            .operation_recovery_warning
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let interrupted = self
            .operation_recovery
            .as_ref()
            .map(OperationRecoveryStore::interrupted)
            .unwrap_or_default();
        (self.operation_recovery.clone(), interrupted, warning)
    }

    pub(crate) fn share_settings_store(&self, store: SettingsStore) -> Arc<SettingsStore> {
        let mut shared = self
            .settings_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(shared.get_or_insert_with(|| Arc::new(store)))
    }

    pub(crate) fn share_workspace_store(&self, store: WorkspaceStore) -> Arc<WorkspaceStore> {
        let mut shared = self
            .workspace_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(shared.get_or_insert_with(|| Arc::new(store)))
    }

    pub(crate) fn adopt_or_snapshot(
        &self,
        settings: AppSettings,
        workspaces: WorkspaceState,
        session: SharedSessionState,
    ) -> (SharedApplicationState, u64) {
        let mut state = self
            .shared_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let snapshot = state
            .get_or_insert_with(|| SharedApplicationState {
                settings,
                workspaces,
                session,
            })
            .clone();
        let revision = if self.shared_state_revision.load(Ordering::Acquire) == 0 {
            self.shared_state_revision.store(1, Ordering::Release);
            1
        } else {
            self.shared_state_revision.load(Ordering::Acquire)
        };
        (snapshot, revision)
    }

    pub(crate) fn publish_settings(
        &self,
        seen_revision: &mut u64,
        mut settings: AppSettings,
    ) -> AppSettings {
        let mut state = self
            .shared_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(current) = state.as_mut() else {
            return settings;
        };
        // Folder view and window geometry belong to an individual window/tab.
        settings.window_placement = current.settings.window_placement;
        settings.view.view_mode = current.settings.view.view_mode;
        settings.view.sort_key = current.settings.view.sort_key.clone();
        settings.view.sort_direction = current.settings.view.sort_direction;
        settings.view.show_preview_panel = current.settings.view.show_preview_panel;
        settings.appearance.grid_min_width = current.settings.appearance.grid_min_width;
        if current.settings != settings {
            current.settings = settings;
            self.advance_revision(seen_revision);
        }
        current.settings.clone()
    }

    pub(crate) fn mutate_workspaces<R>(
        &self,
        seen_revision: &mut u64,
        mutation: impl FnOnce(&mut WorkspaceState) -> R,
    ) -> (R, WorkspaceState) {
        let mut state = self
            .shared_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = state
            .as_mut()
            .expect("window runtime state is initialized before mutations");
        let before = current.workspaces.clone();
        let result = mutation(&mut current.workspaces);
        if current.workspaces != before {
            self.advance_revision(seen_revision);
        }
        (result, current.workspaces.clone())
    }

    /// Applies a session change made by the window that has seen
    /// `seen_revision`, advancing that value past the change when possible.
    pub(crate) fn mutate_session<R>(
        &self,
        seen_revision: &mut u64,
        mutation: impl FnOnce(&mut SharedSessionState) -> R,
    ) -> (R, SharedSessionState) {
        let mut state = self
            .shared_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = state
            .as_mut()
            .expect("window runtime state is initialized before mutations");
        let before = current.session.clone();
        let result = mutation(&mut current.session);
        if current.session != before {
            self.advance_revision(seen_revision);
        }
        (result, current.session.clone())
    }

    /// Advances the shared revision for a change made by the window that has
    /// seen `seen_revision`. The mutating window already holds the result, so
    /// it need not pull it back, but only if it had seen every earlier change;
    /// otherwise its next pull must still pick those up. Callers hold the state
    /// lock, and revisions only advance under it, so the comparison cannot race.
    fn advance_revision(&self, seen_revision: &mut u64) {
        let previous = self.shared_state_revision.fetch_add(1, Ordering::AcqRel);
        if previous == *seen_revision {
            *seen_revision = previous + 1;
        }
    }

    pub(crate) fn snapshot_after(&self, revision: u64) -> Option<(SharedApplicationState, u64)> {
        let current_revision = self.shared_state_revision.load(Ordering::Acquire);
        if current_revision <= revision {
            return None;
        }
        self.shared_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .map(|state| (state, current_revision))
    }

    pub(crate) fn track_operation(
        &self,
        owner_window_id: String,
        id: String,
        request: &FileOperationRequest,
    ) {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operations.retain(|operation| operation.id != id);
        operations.push(SharedOperationSummary {
            id,
            owner_window_id,
            label: file_operation_label(request.kind).to_string(),
            item_count: request.sources.len(),
            destination: request.destination.clone(),
            status: OperationStatus::Running,
            processed_entries: 0,
            total_entries: 0,
            processed_bytes: 0,
            total_bytes: 0,
            current_path: None,
            error: None,
        });
        Self::trim_operation_summaries(&mut operations);
        self.operations_revision.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn track_background_operation(
        &self,
        owner_window_id: String,
        id: String,
        label: impl Into<String>,
        item_count: usize,
        destination: Option<PathBuf>,
    ) {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operations.retain(|operation| operation.id != id);
        operations.push(SharedOperationSummary {
            id,
            owner_window_id,
            label: label.into(),
            item_count,
            destination,
            status: OperationStatus::Running,
            processed_entries: 0,
            total_entries: 0,
            processed_bytes: 0,
            total_bytes: 0,
            current_path: None,
            error: None,
        });
        Self::trim_operation_summaries(&mut operations);
        self.operations_revision.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn trim_operation_summaries(operations: &mut Vec<SharedOperationSummary>) {
        while operations.len() > 50 {
            let Some(index) = operations
                .iter()
                .position(|operation| operation.status.is_settled())
            else {
                break;
            };
            operations.remove(index);
        }
    }

    pub(crate) fn apply_archive_progress(&self, progress: &ArchiveProgressEvent) {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(operation) = operations
            .iter_mut()
            .find(|operation| operation.id == progress.operation_id)
        else {
            return;
        };
        operation.processed_bytes = progress.processed_bytes;
        operation.total_bytes = progress.total_bytes;
        operation.current_path = Some(PathBuf::from(&progress.current_path));
        self.operations_revision.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn finish_background_operation(
        &self,
        id: &str,
        status: OperationStatus,
        error: Option<String>,
    ) {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(operation) = operations.iter_mut().find(|operation| operation.id == id) else {
            return;
        };
        operation.status = status;
        operation.error = error;
        self.operations_revision.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn apply_operation_event(&self, event: &FileOperationEvent) {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(operation) = operations
            .iter_mut()
            .find(|operation| operation.id == event.job_id)
        else {
            return;
        };
        operation.status = match event.state {
            explorie_native_services::FileOperationState::Running => OperationStatus::Running,
            explorie_native_services::FileOperationState::Completed => OperationStatus::Completed,
            explorie_native_services::FileOperationState::Cancelled => OperationStatus::Cancelled,
            explorie_native_services::FileOperationState::Failed => OperationStatus::Failed,
        };
        if let Some(progress) = &event.progress {
            operation.processed_entries = progress.processed_entries;
            operation.total_entries = progress.total_entries;
            operation.processed_bytes = progress.processed_bytes;
            operation.total_bytes = progress.total_bytes;
            operation.current_path.clone_from(&progress.current_path);
        }
        operation.error = event.error.as_ref().map(ToString::to_string);
        self.operations_revision.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn operations_revision(&self) -> u64 {
        self.operations_revision.load(Ordering::Acquire)
    }

    pub(crate) fn operation_summaries(&self) -> Vec<SharedOperationSummary> {
        self.operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn clear_owner_finished_operations(&self, owner_window_id: &str) {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = operations.len();
        operations.retain(|operation| {
            operation.owner_window_id != owner_window_id || !operation.status.is_settled()
        });
        if operations.len() != before {
            self.operations_revision.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Drop a summary whatever its status, for an operation another job has
    /// taken over.
    pub(crate) fn forget_operation(&self, id: &str) {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = operations.len();
        operations.retain(|operation| operation.id != id);
        if operations.len() != before {
            self.operations_revision.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub(crate) fn remove_owner_finished_operation(&self, owner_window_id: &str, id: &str) {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = operations.len();
        operations.retain(|operation| {
            operation.id != id
                || operation.owner_window_id != owner_window_id
                || !operation.status.is_settled()
        });
        if operations.len() != before {
            self.operations_revision.fetch_add(1, Ordering::AcqRel);
        }
    }
}

pub(crate) struct WindowLifetime {
    pub(crate) runtime: WindowRuntime,
    pub(crate) id: String,
}

impl Drop for WindowLifetime {
    fn drop(&mut self) {
        self.runtime.live_windows.fetch_sub(1, Ordering::AcqRel);
        if self.id != "primary" && !self.runtime.quitting.load(Ordering::Acquire) {
            let _ = self.runtime.registry.remove(&self.id);
        }
    }
}
