//! `DirectoryWindow` behavior for operations.

use crate::operation::ProgressRedraw;
use crate::*;

/// Copy, cut and paste through the system file clipboard.
pub(crate) struct ClipboardUi {
    /// What this window last copied or cut, kept for UI affordances while
    /// it is still what the system clipboard holds.
    pub(crate) state: Option<ClipboardState>,
    /// The system file clipboard (an in-memory stand-in in tests).
    pub(crate) files: Rc<dyn FileClipboard>,
    /// What the system clipboard held when last looked at.
    pub(crate) system: SystemClipboard,
    /// The clipboard change count right after this window wrote `state`, so
    /// a peek can tell whether the clipboard still holds it.
    pub(crate) written: Option<i64>,
}

/// Window-side state of file operations: the operation panel, the conflict
/// policy and the conflict prompts waiting for an answer, and the progress of
/// undo and archive jobs.
#[derive(Default)]
pub(crate) struct OperationUi {
    pub(crate) panel_minimized: bool,
    pub(crate) panel_hidden: bool,
    pub(crate) conflict_policy: ConflictPolicy,
    pub(crate) conflict_prompts: VecDeque<FileConflictPrompt>,
    pub(crate) conflict_continuations: HashMap<String, FileOperationRequest>,
    pub(crate) undo_progress: Option<UndoProgressState>,
    pub(crate) archive_progress: Option<ArchiveProgressEvent>,
    pub(crate) archive_creation_id: Option<String>,
    pub(crate) archive_extraction_id: Option<String>,
}

impl DirectoryWindow {
    pub(crate) fn push_mutation_task(&mut self, task: Task<()>) {
        self.mutation.tasks.retain(|task| !task.is_ready());
        self.mutation.tasks.push(task);
    }

    pub(crate) fn apply_file_operation_event(
        &mut self,
        event: FileOperationEvent,
        cx: &mut Context<Self>,
    ) {
        if let Some(runtime) = self
            .window_lifetime
            .as_ref()
            .map(|lifetime| &lifetime.runtime)
        {
            runtime.apply_operation_event(&event);
        }
        let running = matches!(
            event.state,
            explorie_native_services::FileOperationState::Running
        );
        if running {
            self.operation_ui.panel_hidden = false;
        }
        // Running events without an error only carry progress; they arrive once
        // per copied chunk, so their redraws are throttled. Start, finish and
        // error events are always drawn immediately.
        let progress_only = running && event.error.is_none();
        let failed_error = matches!(
            event.state,
            explorie_native_services::FileOperationState::Failed
        )
        .then(|| {
            event.error.as_ref().map_or_else(
                || "Native file operation failed".to_string(),
                ToString::to_string,
            )
        });
        let terminal = !matches!(
            event.state,
            explorie_native_services::FileOperationState::Running
        );
        let completed = matches!(
            event.state,
            explorie_native_services::FileOperationState::Completed
        );
        let conflict = event
            .error
            .as_ref()
            .is_some_and(|error| error.code == ErrorCode::Conflict);
        let job_id = event.job_id.clone();
        if !self.operations.apply(event) {
            if progress_only {
                self.notify_progress(cx);
            } else {
                cx.notify();
            }
            return;
        }
        if progress_only {
            self.notify_progress(cx);
            return;
        }
        // A conflict under the "ask" policy is a question for the user, not a
        // failure: the operation waits for their decision.
        let conflict_request = conflict
            .then(|| self.operations.retry_request(&job_id))
            .flatten()
            .filter(|request| request.conflict_policy == ConflictPolicy::Error);
        if let Some(error) = failed_error
            && conflict_request.is_none()
        {
            self.record_error("File operation failed", error);
        }
        let recovery_error = if terminal {
            self.finish_operation_recovery(&job_id).err()
        } else {
            None
        };
        let undoable = terminal
            .then(|| self.operations.take_undo_record(&job_id))
            .flatten();
        let undo_available = undoable.is_some();
        if let Some(record) = undoable {
            self.undo_ledger.push(record);
        }
        if terminal {
            self.refresh(cx);
        }
        if completed {
            let moved = self.operations.operations().iter().any(|operation| {
                operation.id() == job_id && operation.request().kind == FileOperationKind::Move
            });
            if moved {
                self.clipboard.state = None;
                self.clear_moved_cut(&job_id);
            }
            if undo_available {
                self.status_message = Some("File operation completed • Undo available".to_string());
            } else {
                self.status_message = Some("File operation completed".to_string());
            }
        } else if terminal {
            let retryable = self.operations.retryable_count(&job_id);
            let operation = self
                .operations
                .operations()
                .iter()
                .find(|operation| operation.id() == job_id);
            let base = operation.and_then(|operation| {
                operation
                    .error()
                    .map(|error| format!("File operation failed: {error}"))
                    .or_else(|| Some("File operation cancelled".to_string()))
            });
            self.status_message = base.map(|mut message| {
                if retryable > 0 {
                    message.push_str(&format!(" • {retryable} item(s) can be retried"));
                }
                if undo_available {
                    message.push_str(" • completed items can be undone");
                }
                message
            });
        }
        if let Some(request) = conflict_request {
            self.operations.mark_needs_decision(&job_id);
            self.publish_operation_status(&job_id);
            if !self
                .operation_ui
                .conflict_prompts
                .iter()
                .any(|prompt| prompt.job_id == job_id)
            {
                self.operation_ui
                    .conflict_prompts
                    .push_back(FileConflictPrompt {
                        job_id: job_id.clone(),
                        request,
                        apply_to_all: false,
                    });
            }
            self.status_message = Some("A destination conflict needs your decision".to_string());
        }
        if completed && let Some(request) = self.operation_ui.conflict_continuations.remove(&job_id)
        {
            if let Some(next_id) = self.try_start_file_operation(request, cx) {
                self.supersede_operation(&job_id, &next_id, 0);
            }
        } else if terminal {
            self.operation_ui.conflict_continuations.remove(&job_id);
        }
        if let Some(error) = recovery_error {
            self.record_error("Recovery journal cleanup failed", error.to_string());
            self.status_message = Some(format!(
                "{} • recovery journal cleanup failed: {error}",
                self.status_message
                    .as_deref()
                    .unwrap_or("File operation settled")
            ));
        }
        self.recovery.notice =
            !self.recovery.interrupted.is_empty() || !self.recovery.jobs.is_empty();
        cx.notify();
    }

    /// Mirror a local status change (one native events don't report, such as
    /// waiting for a conflict decision) to the other windows' panels.
    fn publish_operation_status(&self, id: &str) {
        let Some(lifetime) = self.window_lifetime.as_ref() else {
            return;
        };
        if let Some(operation) = self
            .operations
            .operations()
            .iter()
            .find(|operation| operation.id() == id)
        {
            lifetime.runtime.finish_background_operation(
                id,
                operation.status(),
                operation.error().map(str::to_string),
            );
        }
    }

    /// Let the job carrying out a conflict decision take over the history
    /// entry of the job that stopped at the conflict.
    fn supersede_operation(&mut self, previous_id: &str, next_id: &str, skipped: usize) {
        if self.operations.supersede(previous_id, next_id, skipped)
            && let Some(lifetime) = self.window_lifetime.as_ref()
        {
            lifetime.runtime.forget_operation(previous_id);
        }
    }

    /// Settle a conflict prompt's operation without starting another job.
    fn settle_conflict_decision(&mut self, id: &str, unresolved: usize, cancelled: bool) {
        self.operations.settle_decision(id, unresolved, cancelled);
        self.publish_operation_status(id);
    }

    /// Redraws for a progress-only update at most ~10 times per second, with a
    /// trailing redraw so the latest progress is never left undrawn.
    pub(crate) fn notify_progress(&mut self, cx: &mut Context<Self>) {
        match self.operations.progress_throttle().progress(Instant::now()) {
            ProgressRedraw::Now => cx.notify(),
            ProgressRedraw::After(delay) => {
                let executor = cx.background_executor().clone();
                cx.spawn(async move |this, cx| {
                    executor.timer(delay).await;
                    let _ = this.update(cx, |view, cx| {
                        view.operations
                            .progress_throttle()
                            .caught_up(Instant::now());
                        cx.notify();
                    });
                })
                .detach();
            }
            ProgressRedraw::Pending => {}
        }
    }

    pub(crate) fn start_file_operation(
        &mut self,
        request: FileOperationRequest,
        cx: &mut Context<Self>,
    ) {
        let _ = self.try_start_file_operation(request, cx);
    }

    pub(crate) fn try_start_file_operation(
        &mut self,
        request: FileOperationRequest,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        self.try_start_file_operation_with_recovery_ids(request, None, cx)
    }

    pub(crate) fn try_start_file_operation_with_recovery_ids(
        &mut self,
        request: FileOperationRequest,
        recovery_ids: Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        if self.undo_ledger.is_processing() || self.mutation.in_progress {
            self.status_message =
                Some("Wait for the current filesystem change to finish".to_string());
            cx.notify();
            return None;
        }
        let recorded_now = recovery_ids.is_none();
        let recovery_ids = match recovery_ids {
            Some(ids) => ids,
            None => match self
                .recovery
                .store
                .as_mut()
                .map(|store| store.record(&request))
            {
                Some(Ok(ids)) => ids,
                Some(Err(error)) => {
                    self.record_error("File operation recovery setup failed", error.to_string());
                    self.status_message = Some(format!(
                        "Unable to start file operation safely; recovery journal could not be written: {error}"
                    ));
                    cx.notify();
                    return None;
                }
                None => Vec::new(),
            },
        };
        let id = match self
            .services
            .mutations
            .start_file_operation(request.clone())
        {
            Ok(id) => {
                if let Some(lifetime) = self.window_lifetime.as_ref() {
                    lifetime
                        .runtime
                        .track_operation(lifetime.id.clone(), id.clone(), &request);
                }
                self.operations.track(id.clone(), request);
                self.operation_ui.panel_hidden = false;
                self.operation_ui.panel_minimized = false;
                if !recovery_ids.is_empty() {
                    self.recovery.jobs.insert(id.clone(), recovery_ids.clone());
                }
                self.status_message = Some("File operation started".to_string());
                Some(id)
            }
            Err(error) => {
                self.record_error("File operation start failed", error.to_string());
                if recorded_now && let Some(store) = self.recovery.store.as_mut() {
                    let _ = store.remove(&recovery_ids);
                }
                self.status_message = Some(format!("Unable to start file operation: {error}"));
                None
            }
        };
        cx.notify();
        id
    }

    pub(crate) fn copy_selected(&mut self, cx: &mut Context<Self>) {
        self.set_clipboard(ClipboardKind::Copy, cx);
    }

    pub(crate) fn cut_selected(&mut self, cx: &mut Context<Self>) {
        self.set_clipboard(ClipboardKind::Cut, cx);
    }

    pub(crate) fn set_clipboard(&mut self, kind: ClipboardKind, cx: &mut Context<Self>) {
        let paths = self.effective_selected_paths();
        self.set_clipboard_paths(kind, paths, cx);
    }

    /// Copy or cut `paths` to the system clipboard, so they paste in other
    /// explorie windows and in Finder or Explorer, and remember them for this
    /// window's affordances.
    pub(crate) fn set_clipboard_paths(
        &mut self,
        kind: ClipboardKind,
        paths: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            self.status_message = Some("Select one or more items first".to_string());
        } else {
            let count = paths.len();
            let state = ClipboardState { kind, paths };
            let written = self
                .clipboard
                .files
                .write(&state.paths, kind == ClipboardKind::Cut);
            self.clipboard.state = Some(state.clone());
            self.operation_ui.panel_hidden = false;
            let verb = match kind {
                ClipboardKind::Copy => "Copied",
                ClipboardKind::Cut => "Cut",
            };
            self.clipboard.written = None;
            self.status_message = Some(match written {
                Ok(()) => {
                    self.clipboard.system = SystemClipboard::Files(count);
                    self.clipboard.written = self
                        .clipboard
                        .files
                        .peek()
                        .ok()
                        .map(|summary| summary.change_count);
                    format!("{verb} {count} item(s) to the clipboard")
                }
                Err(error) => {
                    self.record_error("Clipboard write failed", error.to_string());
                    // Paste in this window falls back to the in-app state.
                    self.clipboard.system = SystemClipboard::Unknown;
                    format!(
                        "{verb} {count} item(s) for this window only; the system clipboard is unavailable: {error}"
                    )
                }
            });
        }
        cx.notify();
    }

    /// Look at the system clipboard and update the paste affordances. The
    /// in-app state is dropped once the clipboard no longer holds what this
    /// window put there. Only peeks: on macOS, reading files another app
    /// copied outside a paste the user started would ask for permission.
    pub(crate) fn refresh_system_clipboard(&mut self) {
        match self.clipboard.files.peek() {
            Ok(summary) => {
                self.clipboard.system = SystemClipboard::from_summary(&summary);
                if self.clipboard.written != Some(summary.change_count) {
                    self.clipboard.state = None;
                    self.clipboard.written = None;
                }
            }
            Err(_) => self.clipboard.system = SystemClipboard::Unknown,
        }
    }

    /// How many items Paste would paste, as of the last look at the
    /// clipboard: the system clipboard's files, or the in-app state when the
    /// system clipboard is unavailable.
    pub(crate) fn paste_count(&self) -> Option<usize> {
        match &self.clipboard.system {
            SystemClipboard::Files(count) => Some(*count),
            SystemClipboard::Empty => None,
            SystemClipboard::Unknown => {
                self.clipboard.state.as_ref().map(|state| state.paths.len())
            }
        }
    }

    /// Read the files to paste. Files macOS withheld report guidance instead
    /// of "Nothing to paste"; an unavailable clipboard falls back to the
    /// in-app state.
    fn read_paste_source(&mut self) -> Result<Option<ClipboardState>, String> {
        match self.clipboard.files.read() {
            Ok(files) => {
                self.clipboard.system = SystemClipboard::from_files(files.as_ref());
                let current = files.and_then(file_clipboard::clipboard_state);
                if self.clipboard.state.is_some() && self.clipboard.state != current {
                    self.clipboard.state = None;
                    self.clipboard.written = None;
                }
                Ok(current)
            }
            Err(error) if error.code == ErrorCode::PermissionDenied => {
                self.record_error("Clipboard read blocked", error.message.clone());
                Err(error.message)
            }
            Err(_) => {
                self.clipboard.system = SystemClipboard::Unknown;
                Ok(self.clipboard.state.clone())
            }
        }
    }

    /// Paste into the current folder. Files copied anywhere (Finder,
    /// Explorer, another explorie window) paste as copies; explorie's cut
    /// pastes as a move. Text on the clipboard means there is nothing to
    /// paste.
    pub(crate) fn paste(&mut self, cx: &mut Context<Self>) {
        let clipboard = match self.read_paste_source() {
            Ok(Some(clipboard)) => clipboard,
            Ok(None) => {
                self.status_message = Some("Nothing to paste".to_string());
                cx.notify();
                return;
            }
            Err(guidance) => {
                self.status_message = Some(guidance);
                cx.notify();
                return;
            }
        };
        let destination = crate::window::drag_drop::operation_destination(self.browser.path());
        // Items pasted into the folder they came from: as in Finder and
        // Explorer, copies become duplicates ("note copy.txt") without
        // asking, and a cut has nothing to move.
        let (in_place, elsewhere): (Vec<_>, Vec<_>) =
            clipboard.paths.into_iter().partition(|path| {
                path.parent()
                    .is_some_and(|parent| explorie_core::same_directory(parent, &destination))
            });
        let (kind, in_place_policy) = match clipboard.kind {
            ClipboardKind::Copy => (FileOperationKind::Copy, Some(ConflictPolicy::Duplicate)),
            ClipboardKind::Cut => (FileOperationKind::Move, None),
        };
        if elsewhere.is_empty() && in_place_policy.is_none() {
            self.status_message = Some("The items are already in this folder".to_string());
            cx.notify();
            return;
        }
        for (sources, conflict_policy) in [
            (in_place, in_place_policy),
            (elsewhere, Some(self.operation_ui.conflict_policy)),
        ] {
            let Some(conflict_policy) = conflict_policy.filter(|_| !sources.is_empty()) else {
                continue;
            };
            self.start_file_operation(
                FileOperationRequest {
                    kind,
                    sources,
                    destination: Some(destination.clone()),
                    conflict_policy,
                },
                cx,
            );
        }
    }

    /// After a move completes, empty the system clipboard if it still holds
    /// explorie's cut of the moved items, so they cannot be pasted again
    /// from their old location.
    fn clear_moved_cut(&mut self, job_id: &str) {
        let Some(sources) = self
            .operations
            .operations()
            .iter()
            .find(|operation| operation.id() == job_id)
            .map(|operation| operation.request().sources.clone())
        else {
            return;
        };
        // Only explorie writes the cut marker, and reading what this app
        // wrote never asks; peek first so a clipboard another app replaced
        // meanwhile is never read.
        if !self.clipboard.files.peek().is_ok_and(|summary| summary.cut) {
            return;
        }
        let Ok(Some(files)) = self.clipboard.files.read() else {
            return;
        };
        if !files.cut || !files.paths.iter().all(|path| sources.contains(path)) {
            return;
        }
        match self.clipboard.files.clear() {
            Ok(()) => self.clipboard.system = SystemClipboard::Empty,
            Err(error) => self.record_error("Clipboard clear failed", error.to_string()),
        }
    }

    pub(crate) fn trash_selected(&mut self, cx: &mut Context<Self>) {
        let paths = self.effective_selected_paths();
        self.trash_paths(paths, cx);
    }

    pub(crate) fn trash_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            self.status_message = Some("Select one or more items first".to_string());
            cx.notify();
            return;
        }
        if self.settings.behavior.confirm_before_delete {
            let count = paths.len();
            self.search.active = false;
            self.mutation.prompt = Some(MutationPrompt::new(
                MutationPromptKind::Trash { paths },
                String::new(),
            ));
            self.status_message = Some(format!(
                "Moving {count} item(s) to the Trash requires confirmation"
            ));
            cx.notify();
            return;
        }
        self.start_trash_operations(paths, cx);
    }

    pub(crate) fn start_trash_operations(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        // One operation for the whole selection: the core batches the platform
        // Trash requests and reports exactly which items could not be moved.
        self.start_file_operation(
            FileOperationRequest {
                kind: FileOperationKind::Trash,
                sources: paths,
                destination: None,
                conflict_policy: ConflictPolicy::Error,
            },
            cx,
        );
    }

    pub(crate) fn prompt_permanent_delete_selected(&mut self, cx: &mut Context<Self>) {
        if self.undo_ledger.is_processing()
            || self.mutation.in_progress
            || self.operations.active_count() > 0
        {
            self.status_message =
                Some("Wait for the current filesystem change to finish".to_string());
            cx.notify();
            return;
        }
        let items: Vec<_> = self
            .browser
            .selected_entries()
            .into_iter()
            .map(|entry| (entry.path, entry.is_dir))
            .collect();
        if items.is_empty() {
            self.status_message = Some("Select one or more items first".to_string());
            cx.notify();
            return;
        }
        let count = items.len();
        self.search.active = false;
        self.mutation.prompt = Some(MutationPrompt::new(
            MutationPromptKind::PermanentDelete { items },
            String::new(),
        ));
        self.status_message = Some(format!(
            "Permanent deletion of {count} item(s) requires exact confirmation"
        ));
        cx.notify();
    }

    pub(crate) fn cancel_latest_operation(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.operations.latest_running_id().map(str::to_string) else {
            self.status_message = Some("No file operation is running".to_string());
            cx.notify();
            return;
        };
        self.cancel_operation(&id, cx);
    }

    pub(crate) fn cancel_operation(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.services.mutations.cancel_file_operation(id) {
            self.status_message = Some("Cancellation requested".to_string());
        } else {
            self.status_message = Some("The operation already finished".to_string());
        }
        cx.notify();
    }

    pub(crate) fn retry_latest_operation(&mut self, cx: &mut Context<Self>) {
        if self.undo_ledger.is_processing() || self.mutation.in_progress {
            self.status_message =
                Some("Wait for the current filesystem change to finish".to_string());
            cx.notify();
            return;
        }
        let Some(previous_id) = self.operations.latest_retryable_id().map(str::to_string) else {
            self.status_message = Some("No unresolved copy or move items to retry".to_string());
            cx.notify();
            return;
        };
        self.retry_operation(&previous_id, cx);
    }

    pub(crate) fn retry_operation(&mut self, previous_id: &str, cx: &mut Context<Self>) {
        if self.undo_ledger.is_processing() || self.mutation.in_progress {
            self.status_message =
                Some("Wait for the current filesystem change to finish".to_string());
            cx.notify();
            return;
        }
        let Some(request) = self.operations.retry_request(previous_id) else {
            self.status_message = Some("This operation cannot be retried".to_string());
            cx.notify();
            return;
        };
        let count = request.sources.len();
        let awaiting_decision = self.operations.operations().iter().any(|operation| {
            operation.id() == previous_id && operation.status() == OperationStatus::NeedsDecision
        });
        if let Some(id) = self.try_start_file_operation(request, cx) {
            self.operations.mark_retry_started(previous_id);
            if awaiting_decision {
                // Retrying answers the pending conflict: the new job carries on
                // the waiting operation's entry.
                self.supersede_operation(previous_id, &id, 0);
            }
            self.operation_ui
                .conflict_prompts
                .retain(|prompt| prompt.job_id != previous_id);
            self.status_message = Some(format!("Retrying {count} unresolved item(s)"));
        }
        cx.notify();
    }

    pub(crate) fn toggle_file_conflict_apply_to_all(&mut self, cx: &mut Context<Self>) {
        if let Some(prompt) = self.operation_ui.conflict_prompts.front_mut() {
            prompt.apply_to_all = !prompt.apply_to_all;
            cx.notify();
        }
    }

    pub(crate) fn resolve_file_conflict(
        &mut self,
        choice: FileConflictChoice,
        cx: &mut Context<Self>,
    ) {
        let Some(prompt) = self.operation_ui.conflict_prompts.front().cloned() else {
            return;
        };
        let count = prompt.request.sources.len();
        let Some(current) = prompt.request.sources.first().cloned() else {
            self.operation_ui.conflict_prompts.pop_front();
            cx.notify();
            return;
        };

        if choice == FileConflictChoice::Skip {
            if prompt.apply_to_all || count == 1 {
                self.settle_conflict_decision(&prompt.job_id, count, false);
                self.operation_ui.conflict_prompts.pop_front();
                self.status_message = Some(if count == 1 {
                    format!("Skipped {}", path_label(&current))
                } else {
                    format!("Skipped {count} unresolved items")
                });
                cx.notify();
                return;
            }

            let mut remainder = prompt.request.clone();
            remainder.sources.remove(0);
            if let Some(id) = self.try_start_file_operation(remainder, cx) {
                self.operations.mark_retry_started(&prompt.job_id);
                self.supersede_operation(&prompt.job_id, &id, 1);
                self.operation_ui.conflict_prompts.pop_front();
                self.status_message = Some(format!(
                    "Skipped {} • continuing with {} item(s)",
                    path_label(&current),
                    count - 1
                ));
                cx.notify();
            }
            return;
        }

        let mut selected = prompt.request.clone();
        selected.conflict_policy = match choice {
            FileConflictChoice::Replace => ConflictPolicy::Replace,
            FileConflictChoice::KeepBoth => ConflictPolicy::Rename,
            FileConflictChoice::Skip => unreachable!("skip handled above"),
        };
        let continuation = if prompt.apply_to_all || count == 1 {
            None
        } else {
            selected.sources.truncate(1);
            let mut continuation = prompt.request.clone();
            continuation.sources.remove(0);
            Some(continuation)
        };
        let Some(id) = self.try_start_file_operation(selected, cx) else {
            return;
        };
        self.operations.mark_retry_started(&prompt.job_id);
        self.supersede_operation(&prompt.job_id, &id, 0);
        self.operation_ui.conflict_prompts.pop_front();
        if let Some(continuation) = continuation {
            self.operation_ui
                .conflict_continuations
                .insert(id, continuation);
        }
        let action = match choice {
            FileConflictChoice::Replace => "Replacing",
            FileConflictChoice::KeepBoth => "Keeping both for",
            FileConflictChoice::Skip => unreachable!("skip handled above"),
        };
        self.status_message = Some(if prompt.apply_to_all {
            format!("{action} {count} unresolved item(s)")
        } else {
            format!("{action} {}", path_label(&current))
        });
        cx.notify();
    }

    pub(crate) fn cancel_all_file_conflicts(&mut self, cx: &mut Context<Self>) {
        let prompts = std::mem::take(&mut self.operation_ui.conflict_prompts);
        let count = prompts
            .iter()
            .map(|prompt| prompt.request.sources.len())
            .sum::<usize>();
        for prompt in prompts {
            self.settle_conflict_decision(&prompt.job_id, prompt.request.sources.len(), true);
        }
        self.status_message = Some(format!("Cancelled {count} unresolved item(s)"));
        cx.notify();
    }

    pub(crate) fn clear_completed_operations(&mut self, cx: &mut Context<Self>) {
        self.operations.clear_completed();
        if let Some(lifetime) = self.window_lifetime.as_ref() {
            lifetime
                .runtime
                .clear_owner_finished_operations(&lifetime.id);
        }
        cx.notify();
    }

    pub(crate) fn remove_finished_operation(&mut self, id: &str, cx: &mut Context<Self>) {
        self.operations.remove_finished(id);
        if let Some(lifetime) = self.window_lifetime.as_ref() {
            lifetime
                .runtime
                .remove_owner_finished_operation(&lifetime.id, id);
        }
        cx.notify();
    }

    pub(crate) fn foreign_operation_summaries(&self) -> Vec<SharedOperationSummary> {
        let Some(lifetime) = self.window_lifetime.as_ref() else {
            return Vec::new();
        };
        lifetime
            .runtime
            .operation_summaries()
            .into_iter()
            .filter(|operation| operation.owner_window_id != lifetime.id)
            .collect()
    }

    pub(crate) fn process_active_operation_count(&self) -> usize {
        self.operations.active_count()
            + usize::from(
                self.operation_ui.archive_creation_id.is_some()
                    || self.operation_ui.archive_extraction_id.is_some(),
            )
            + usize::from(self.operation_ui.undo_progress.is_some())
            + self
                .foreign_operation_summaries()
                .iter()
                .filter(|operation| operation.status == OperationStatus::Running)
                .count()
    }

    pub(crate) fn toggle_operation_panel_minimized(&mut self, cx: &mut Context<Self>) {
        self.operation_ui.panel_minimized = !self.operation_ui.panel_minimized;
        cx.notify();
    }

    pub(crate) fn open_operation_panel(&mut self, cx: &mut Context<Self>) {
        self.operation_ui.panel_hidden = false;
        self.operation_ui.panel_minimized = false;
        cx.notify();
    }

    pub(crate) fn close_operation_panel(&mut self, cx: &mut Context<Self>) {
        let foreign_active = self
            .foreign_operation_summaries()
            .iter()
            .any(|operation| operation.status == OperationStatus::Running);
        if self.operations.active_count() == 0
            && !foreign_active
            && self.operation_ui.undo_progress.is_none()
            && self.operation_ui.archive_progress.is_none()
            && self.operation_ui.archive_creation_id.is_none()
            && self.operation_ui.archive_extraction_id.is_none()
        {
            self.operation_ui.panel_hidden = true;
        }
        cx.notify();
    }

    pub(crate) fn cycle_conflict_policy(&mut self, cx: &mut Context<Self>) {
        self.operation_ui.conflict_policy = match self.operation_ui.conflict_policy {
            ConflictPolicy::Error => ConflictPolicy::Rename,
            ConflictPolicy::Rename => ConflictPolicy::Replace,
            // Duplicate is only for pasting into the source folder; it is
            // never the chosen policy, but cycle back to asking if it is.
            ConflictPolicy::Replace | ConflictPolicy::Duplicate => ConflictPolicy::Error,
        };
        cx.notify();
    }

    pub(crate) fn apply_undo_progress_update(
        &mut self,
        update: UndoProgressUpdate,
        cx: &mut Context<Self>,
    ) {
        let Some(progress) = self.operation_ui.undo_progress.as_mut() else {
            return;
        };
        match update {
            UndoProgressUpdate::Started(job_id) => {
                progress.current_job_id = Some(job_id);
                progress.processed_bytes = 0;
                progress.total_bytes = 0;
            }
            UndoProgressUpdate::Progress(file_progress) => {
                progress.processed_bytes = file_progress.processed_bytes;
                progress.total_bytes = file_progress.total_bytes;
                self.notify_progress(cx);
                return;
            }
            UndoProgressUpdate::ItemCompleted => {
                progress.completed_items = progress
                    .completed_items
                    .saturating_add(1)
                    .min(progress.total_items);
                progress.current_job_id = None;
                progress.processed_bytes = 0;
                progress.total_bytes = 0;
            }
        }
        cx.notify();
    }

    pub(crate) fn cancel_undo(&mut self, cx: &mut Context<Self>) {
        let Some(progress) = self.operation_ui.undo_progress.as_mut() else {
            self.status_message = Some("No undo is running".to_string());
            cx.notify();
            return;
        };
        progress.cancellation.store(true, Ordering::Release);
        progress.cancelling = true;
        if let Some(job_id) = progress.current_job_id.as_deref() {
            self.services.mutations.cancel_file_operation(job_id);
        }
        self.status_message = Some("Cancelling undo…".to_string());
        cx.notify();
    }

    pub(crate) fn undo(&mut self, cx: &mut Context<Self>) {
        if self.mutation.prompt.is_some() {
            self.status_message = Some("Finish or cancel the name prompt first".to_string());
            cx.notify();
            return;
        }
        if self.operations.active_count() > 0 || self.mutation.in_progress {
            self.status_message =
                Some("Wait for active filesystem operations before undoing".to_string());
            cx.notify();
            return;
        }
        let Some(mut record) = self.undo_ledger.begin_undo(SystemTime::now()) else {
            self.status_message = Some("Nothing available to undo".to_string());
            cx.notify();
            return;
        };
        if let Err(error) = record.validate_expected_paths() {
            self.undo_ledger.finish_undo(record, false);
            self.status_message = Some(format!("Undo blocked: {error}"));
            cx.notify();
            return;
        }
        let description = record.description().to_string();
        let total_items = undo_action_item_count(&record.action);
        let cancellation = Arc::new(AtomicBool::new(false));
        let services = self.services.clone();
        let recovery_store = self.recovery.store.clone();
        self.operation_ui.undo_progress = Some(UndoProgressState {
            description: description.clone(),
            completed_items: 0,
            total_items,
            processed_bytes: 0,
            total_bytes: 0,
            current_job_id: None,
            cancellation: Arc::clone(&cancellation),
            cancelling: false,
        });
        self.status_message = Some(format!("Undoing {description}…"));
        self.push_mutation_task(cx.spawn(async move |this, cx| {
            let result = undo_action_with_progress(
                record.action.clone(),
                services,
                recovery_store,
                cancellation,
                |update| {
                    let _ = this.update(cx, |view, cx| view.apply_undo_progress_update(update, cx));
                },
            )
            .await;
            let _ = this.update(cx, |view, cx| {
                let progress = view.operation_ui.undo_progress.take();
                match result {
                    Ok(action) => {
                        record.action = action;
                        record.capture_redo_paths();
                        view.undo_ledger.finish_undo(record, true);
                        view.refresh(cx);
                        view.status_message = Some(format!("Undid {description}"));
                    }
                    Err(error) => {
                        view.undo_ledger.finish_undo(record, false);
                        if error.code != ErrorCode::Cancelled {
                            view.record_error("Undo failed", error.to_string());
                        }
                        view.status_message = Some(if error.code == ErrorCode::Cancelled {
                            let completed = progress
                                .as_ref()
                                .map_or(0, |progress| progress.completed_items);
                            format!("Undo cancelled after {completed} of {total_items} item(s)")
                        } else {
                            format!("Undo failed: {error}")
                        });
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn redo(&mut self, cx: &mut Context<Self>) {
        if self.mutation.prompt.is_some() {
            self.status_message = Some("Finish or cancel the name prompt first".to_string());
            cx.notify();
            return;
        }
        if self.operations.active_count() > 0 || self.mutation.in_progress {
            self.status_message =
                Some("Wait for active filesystem operations before redoing".to_string());
            cx.notify();
            return;
        }
        let Some(mut record) = self.undo_ledger.begin_redo() else {
            self.status_message = Some("Nothing available to redo".to_string());
            cx.notify();
            return;
        };
        if let Err(error) = record.validate_expected_paths() {
            self.undo_ledger.finish_redo(record, false);
            self.status_message = Some(format!("Redo blocked: {error}"));
            cx.notify();
            return;
        }
        let description = record.description().to_string();
        let services = self.services.clone();
        let recovery_store = self.recovery.store.clone();
        self.status_message = Some(format!("Redoing {description}…"));
        self.push_mutation_task(cx.spawn(async move |this, cx| {
            let result = redo_action(record.action.clone(), services, recovery_store).await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(action) => {
                        record.action = action;
                        record.capture_undo_paths();
                        view.undo_ledger.finish_redo(record, true);
                        view.refresh(cx);
                        view.status_message = Some(format!("Redid {description}"));
                    }
                    Err(error) => {
                        view.undo_ledger.finish_redo(record, false);
                        view.record_error("Redo failed", error.to_string());
                        view.status_message = Some(format!("Redo failed: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn render_file_conflict_prompt(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(prompt) = self.operation_ui.conflict_prompts.front().cloned() else {
            return div().into_any_element();
        };
        let Some(source) = prompt.current_source().map(Path::to_path_buf) else {
            return div().into_any_element();
        };
        let destination = prompt
            .destination_path()
            .unwrap_or_else(|| PathBuf::from("Unknown destination"));
        let name = path_label(&source);
        let count = prompt.request.sources.len();
        let operation = match prompt.request.kind {
            FileOperationKind::Copy => "Copy",
            FileOperationKind::Move => "Move",
            FileOperationKind::Trash => "File operation",
        };
        let apply_label = if prompt.apply_to_all {
            format!("☑ Apply this action to all {count} unresolved items")
        } else {
            format!("☐ Apply this action to all {count} unresolved items")
        };
        let max_dialog_height = (self
            .layout
            .last_window_bounds
            .height
            .unwrap_or(DEFAULT_WINDOW_HEIGHT)
            - 24.0 * self.palette.scale)
            .max(320.0 * self.palette.scale);

        div()
            .id("file-conflict-backdrop")
            .debug_selector(|| "file-conflict-backdrop".to_string())
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_center()
            .p_3()
            .bg(with_alpha(rgb(0x000000), 0.58))
            .child(
                div()
                    .id("file-conflict-dialog")
                    .debug_selector(|| "file-conflict-dialog".to_string())
                    .role(Role::AlertDialog)
                    .aria_label("File already exists")
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(px(560.0 * self.palette.scale))
                    .max_h(px(max_dialog_height))
                    .border_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.panel)
                    .shadow_lg()
                    .child(
                        div()
                            .id("file-conflict-header")
                            .debug_selector(|| "file-conflict-header".to_string())
                            .flex()
                            .items_center()
                            .gap_3()
                            .h(px(46.0))
                            .flex_none()
                            .px_4()
                            .border_b_1()
                            .border_color(self.palette.border)
                            .bg(self.palette.topbar)
                            .child(div().text_lg().text_color(rgb(0xfbbf24)).child("⚠"))
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("File already exists"),
                            )
                            .when(count > 1, |header| {
                                header.child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .bg(self.palette.selected)
                                        .text_xs()
                                        .text_color(self.palette.muted)
                                        .child(format!("{count} unresolved")),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("file-conflict-content")
                            .debug_selector(|| "file-conflict-content".to_string())
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_4()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(
                                div()
                                    .text_sm()
                                    .child(format!(
                                        "A file named “{name}” already exists in the destination. What would you like to do?"
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(conflict_path_card(
                                        if operation == "Copy" {
                                            "Source (copying from)"
                                        } else {
                                            "Source (moving from)"
                                        },
                                        &source,
                                        self.palette,
                                    ))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_wrap()
                                            .items_center()
                                            .px_1()
                                            .text_color(self.palette.muted)
                                            .child("→"),
                                    )
                                    .child(conflict_path_card(
                                        "Existing file",
                                        &destination,
                                        self.palette,
                                    )),
                            )
                            .when(count > 1, |content| {
                                content.child(
                                    toolbar_button(
                                        "conflict-apply-all",
                                        &apply_label,
                                        selected_control_color(
                                            prompt.apply_to_all,
                                            self.palette,
                                        ),
                                    )
                                    .debug_selector(|| "conflict-apply-all".to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.toggle_file_conflict_apply_to_all(cx)
                                    })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("file-conflict-footer")
                            .debug_selector(|| "file-conflict-footer".to_string())
                            .flex()
                            .items_center()
                            .h(px(46.0))
                            .flex_none()
                            .gap_2()
                            .px_4()
                            .border_t_1()
                            .border_color(self.palette.border)
                            .bg(self.palette.topbar)
                            .child(
                                toolbar_button("conflict-skip", "Skip", self.palette.control)
                                    .debug_selector(|| "conflict-skip".to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.resolve_file_conflict(FileConflictChoice::Skip, cx)
                                    })),
                            )
                            .child(
                                toolbar_button("conflict-replace", "Replace", self.palette.accent)
                                .debug_selector(|| "conflict-replace".to_string())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.resolve_file_conflict(FileConflictChoice::Replace, cx)
                                })),
                            )
                            .child(
                                toolbar_button(
                                    "conflict-keep-both",
                                    "Keep Both",
                                    self.palette.control,
                                )
                                .debug_selector(|| "conflict-keep-both".to_string())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.resolve_file_conflict(FileConflictChoice::KeepBoth, cx)
                                    })),
                            )
                            .child(div().flex_1())
                            .child(
                                toolbar_button(
                                    "conflict-cancel-all",
                                    "Cancel All",
                                    self.palette.control,
                                )
                                .debug_selector(|| "conflict-cancel-all".to_string())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_all_file_conflicts(cx)
                                })),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn render_operation_panel(
        &mut self,
        status_message_visible: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = self.palette;
        let scale = self.palette.scale;
        let foreign_operations = self.foreign_operation_summaries();
        let queue_active_count = self.operations.active_count();
        let local_active_count = queue_active_count
            + usize::from(
                self.operation_ui.archive_creation_id.is_some()
                    || self.operation_ui.archive_extraction_id.is_some(),
            )
            + usize::from(self.operation_ui.undo_progress.is_some());
        let active_count = local_active_count
            + foreign_operations
                .iter()
                .filter(|operation| operation.status == OperationStatus::Running)
                .count();
        let failed_count = self
            .operations
            .operations()
            .iter()
            .filter(|operation| operation.status() == OperationStatus::Failed)
            .count()
            + foreign_operations
                .iter()
                .filter(|operation| operation.status == OperationStatus::Failed)
                .count();
        let waiting_count = self
            .operations
            .operations()
            .iter()
            .filter(|operation| operation.status() == OperationStatus::NeedsDecision)
            .count();
        let finished_count = self
            .operations
            .operations()
            .iter()
            .filter(|operation| operation.status().is_settled())
            .count();
        let has_visible_content = self.clipboard.state.is_some()
            || !self.operations.operations().is_empty()
            || !foreign_operations.is_empty()
            || self.operation_ui.archive_progress.is_some()
            || self.operation_ui.archive_creation_id.is_some()
            || self.operation_ui.archive_extraction_id.is_some()
            || self.operation_ui.undo_progress.is_some();
        if !has_visible_content || self.operation_ui.panel_hidden {
            return div()
                .id("operation-panel")
                .debug_selector(|| "operation-panel".to_string())
                .h_0()
                .into_any_element();
        }

        let mut bottom = 12.0 * scale;
        if self.settings.view.show_status_bar {
            bottom += 28.0 * scale;
            if status_message_visible {
                bottom += 34.0 * scale;
            }
        }
        if self.operation_ui.panel_minimized && active_count > 0 {
            return div()
                .id("operation-panel")
                .debug_selector(|| "operation-panel".to_string())
                .role(Role::Button)
                .aria_label(format!("Expand operations: {active_count} in progress"))
                .focusable()
                .tab_stop(true)
                .absolute()
                .right(px(16.0 * scale))
                .bottom(px(bottom))
                .flex()
                .items_center()
                .gap_2()
                .w(px(280.0 * scale))
                .h(px(40.0 * scale))
                .px_3()
                .rounded_lg()
                .border_1()
                .border_color(self.palette.border)
                .bg(self.palette.panel)
                .shadow_lg()
                .occlude()
                .focus(move |panel| panel.border_color(palette.accent))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| this.toggle_operation_panel_minimized(cx)))
                .child(
                    div()
                        .w(px(10.0))
                        .h(px(10.0))
                        .rounded_full()
                        .bg(self.palette.accent),
                )
                .child(div().text_sm().child(format!(
                    "{active_count} operation{} in progress",
                    if active_count == 1 { "" } else { "s" }
                )))
                .into_any_element();
        }

        let status_label = if active_count > 0 {
            format!("{active_count} active")
        } else if waiting_count > 0 {
            "Needs decision".to_string()
        } else if failed_count > 0 {
            format!("{failed_count} failed")
        } else {
            "Finished".to_string()
        };
        let status_color = if active_count > 0 {
            self.palette.accent
        } else if waiting_count > 0 {
            decision_color()
        } else if failed_count > 0 {
            rgb(0xff6b6b)
        } else {
            rgb(0x6fcf97)
        };

        let mut rows = Vec::new();
        if let Some(clipboard) = self.clipboard.state.as_ref() {
            let verb = match clipboard.kind {
                ClipboardKind::Copy => "copy",
                ClipboardKind::Cut => "move",
            };
            let count = clipboard.paths.len();
            rows.push(
                div()
                    .id("operation-clipboard")
                    .debug_selector(|| "operation-clipboard".to_string())
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded_sm()
                    .bg(self.palette.surface)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(format!("Clipboard: {count} item(s) ready to {verb}")),
                    )
                    .child(
                        toolbar_button("paste", "Paste", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| this.paste(cx))),
                    )
                    .child(
                        toolbar_button(
                            "conflict-policy",
                            &format!(
                                "Conflicts: {}",
                                conflict_policy_label(self.operation_ui.conflict_policy)
                            ),
                            self.palette.control,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.cycle_conflict_policy(cx))),
                    )
                    .into_any_element(),
            );
        }

        if let Some(progress) = self.operation_ui.archive_progress.as_ref() {
            let label = if progress.total_bytes > 0 {
                format!(
                    "{} / {} • {}",
                    format_size(progress.processed_bytes),
                    format_size(progress.total_bytes),
                    progress.current_path
                )
            } else {
                progress.current_path.clone()
            };
            rows.push(
                operation_context_row(
                    "archive-progress",
                    "Archive",
                    label,
                    self.palette.accent,
                    self.palette,
                )
                .into_any_element(),
            );
        }

        if self.operation_ui.archive_creation_id.is_some()
            && self.operation_ui.archive_progress.is_none()
        {
            rows.push(
                operation_context_row(
                    "archive-creation",
                    "Create archive",
                    "In progress".to_string(),
                    self.palette.accent,
                    self.palette,
                )
                .into_any_element(),
            );
        }

        if let Some(operation_id) = self.operation_ui.archive_extraction_id.clone() {
            rows.push(
                operation_context_row(
                    "archive-extraction",
                    "Extract archive",
                    "In progress".to_string(),
                    self.palette.accent,
                    self.palette,
                )
                .child(
                    toolbar_button("cancel-archive-extraction", "Cancel", rgb(0x493232)).on_click(
                        cx.listener(move |this, _, _, cx| {
                            if this.services.archives.cancel(&operation_id) {
                                this.status_message =
                                    Some("Cancelling archive extraction…".to_string());
                                cx.notify();
                            }
                        }),
                    ),
                )
                .into_any_element(),
            );
        }

        if let Some(progress) = self.operation_ui.undo_progress.as_ref() {
            let state = if progress.cancelling {
                "Cancelling"
            } else {
                "In progress"
            };
            let label = format!(
                "{state} • {} / {} item(s)",
                progress.completed_items, progress.total_items
            );
            rows.push(
                operation_context_row(
                    "undo-progress",
                    &format!("Undo {}", progress.description),
                    label,
                    self.palette.accent,
                    self.palette,
                )
                .child(
                    toolbar_button("cancel-undo", "Cancel", rgb(0x493232))
                        .debug_selector(|| "cancel-undo".to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_undo(cx))),
                )
                .into_any_element(),
            );
        }

        for (index, operation) in foreign_operations.iter().rev().enumerate() {
            let (status, status_color) = operation_status_label(operation.status, self.palette);
            let destination = operation
                .destination
                .as_deref()
                .map(path_label)
                .map(|destination| format!(" → {destination}"))
                .unwrap_or_default();
            let progress = if operation.total_bytes > 0 {
                format!(
                    " • {} / {}",
                    format_size(operation.processed_bytes),
                    format_size(operation.total_bytes)
                )
            } else if operation.total_entries > 0 {
                format!(
                    " • {} / {} entries",
                    operation.processed_entries, operation.total_entries
                )
            } else {
                String::new()
            };
            let error = operation
                .error
                .as_deref()
                .map(|error| format!(" • {error}"))
                .unwrap_or_default();
            let current_path = operation
                .current_path
                .as_deref()
                .map(path_label)
                .map(|path| format!(" • {path}"))
                .unwrap_or_default();
            let details = format!(
                "Another window • {} item{}{}{progress}{current_path}{error}",
                operation.item_count,
                if operation.item_count == 1 { "" } else { "s" },
                destination,
            );
            rows.push(
                div()
                    .id(("foreign-operation-row", index))
                    .debug_selector(move || format!("foreign-operation-row-{index}"))
                    .role(Role::Group)
                    .aria_label(format!("{} in another window: {status}", operation.label))
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_3()
                    .rounded_sm()
                    .bg(self.palette.surface)
                    .child(
                        div()
                            .w(px(10.0))
                            .h(px(10.0))
                            .rounded_full()
                            .bg(status_color),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{} • {status}", operation.label)),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(self.palette.tertiary)
                                    .child(details),
                            ),
                    )
                    .into_any_element(),
            );
        }

        for (index, operation) in self
            .operations
            .operations()
            .iter()
            .rev()
            .cloned()
            .enumerate()
        {
            let kind = file_operation_label(operation.request().kind);
            let count = operation.total_items();
            let skipped = operation.skipped_items();
            let destination = operation
                .request()
                .destination
                .as_deref()
                .map(path_label)
                .map(|destination| format!(" → {destination}"))
                .unwrap_or_default();
            let skipped_note = if skipped > 0 {
                format!(" • {skipped} skipped")
            } else {
                String::new()
            };
            let details = format!(
                "{count} item{}{destination}{skipped_note}",
                if count == 1 { "" } else { "s" }
            );
            let (status, status_color) = operation_status_label(operation.status(), self.palette);
            let progress_fraction = operation.progress().map_or_else(
                || {
                    if operation.status() == OperationStatus::Completed {
                        1.0
                    } else {
                        0.0
                    }
                },
                |progress| {
                    let fraction = if progress.total_bytes > 0 {
                        progress.processed_bytes as f32 / progress.total_bytes as f32
                    } else if progress.total_entries > 0 {
                        progress.processed_entries as f32 / progress.total_entries as f32
                    } else {
                        0.0
                    };
                    fraction.clamp(0.0, 1.0)
                },
            );
            let percent = (progress_fraction * 100.0).round() as u32;
            let processed = operation.progress().map(|progress| {
                if progress.total_bytes > 0 {
                    format!(
                        "{} / {}",
                        format_size(progress.processed_bytes),
                        format_size(progress.total_bytes)
                    )
                } else {
                    format!(
                        "{} / {} entries",
                        progress.processed_entries, progress.total_entries
                    )
                }
            });
            let current_path = operation
                .progress()
                .and_then(|progress| progress.current_path.as_deref())
                .map(|path| path.display().to_string());
            let error = operation.error().map(str::to_string);
            let decision = (operation.status() == OperationStatus::NeedsDecision).then(|| {
                let conflict = self
                    .operation_ui
                    .conflict_prompts
                    .iter()
                    .find(|prompt| prompt.job_id == operation.id())
                    .and_then(FileConflictPrompt::current_source)
                    .map(path_label);
                match conflict {
                    Some(name) => {
                        format!("“{name}” already exists • choose Skip, Replace or Keep Both")
                    }
                    None => "Waiting for a conflict decision".to_string(),
                }
            });
            let running = operation.status() == OperationStatus::Running;
            let settled = operation.status().is_settled();
            let retryable = operation.retryable_count() > 0
                && operation.status() != OperationStatus::NeedsDecision
                && operation.request().kind != FileOperationKind::Trash;
            let operation_id = operation.id().to_string();
            let cancel_id = operation_id.clone();
            let retry_id = operation_id.clone();
            let remove_id = operation_id;
            let show_progress = running || operation.status() == OperationStatus::Completed;
            rows.push(
                div()
                    .id(("operation-row", index))
                    .debug_selector(move || format!("operation-row-{index}"))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded_sm()
                    .bg(self.palette.surface)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(24.0 * self.palette.scale))
                                    .h(px(24.0 * self.palette.scale))
                                    .rounded_sm()
                                    .bg(with_alpha(status_color, 0.14))
                                    .text_color(status_color)
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(&kind[..1]),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(kind),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_xs()
                                            .text_color(self.palette.tertiary)
                                            .child(details),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(status_color)
                                    .child(status),
                            )
                            .when(running, |header| {
                                header.child(
                                    toolbar_button(
                                        ("cancel-operation", index),
                                        "Cancel",
                                        rgb(0x493232),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| this.cancel_operation(&cancel_id, cx),
                                    )),
                                )
                            })
                            .when(retryable, |header| {
                                header.child(
                                    toolbar_button(
                                        ("retry-operation", index),
                                        "Retry",
                                        rgb(0x31523b),
                                    )
                                    .debug_selector(move || format!("retry-operation-{index}"))
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| this.retry_operation(&retry_id, cx),
                                    )),
                                )
                            })
                            .when(settled, |header| {
                                header.child(
                                    operation_icon_button(
                                        ("remove-operation", index),
                                        "Remove operation",
                                        "close",
                                        self.palette,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.remove_finished_operation(&remove_id, cx)
                                        },
                                    )),
                                )
                            }),
                    )
                    .when(show_progress, |row| {
                        row.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .h(px(6.0))
                                        .w_full()
                                        .rounded_sm()
                                        .overflow_hidden()
                                        .bg(self.palette.window)
                                        .child(
                                            div()
                                                .h_full()
                                                .w(px(330.0 * scale * progress_fraction))
                                                .bg(status_color),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .justify_between()
                                        .text_xs()
                                        .text_color(self.palette.tertiary)
                                        .child(
                                            if operation.status() == OperationStatus::Completed {
                                                "Completed".to_string()
                                            } else {
                                                format!("{percent}%")
                                            },
                                        )
                                        .when_some(processed, |stats, processed| {
                                            stats.child(processed)
                                        }),
                                ),
                        )
                    })
                    .when_some(current_path, |row, current_path| {
                        row.child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(self.palette.tertiary)
                                .child(current_path),
                        )
                    })
                    .when_some(decision, |row, decision| {
                        row.child(
                            div()
                                .debug_selector(move || format!("operation-decision-{index}"))
                                .p_2()
                                .rounded_sm()
                                .border_1()
                                .border_color(with_alpha(decision_color(), 0.4))
                                .bg(with_alpha(decision_color(), 0.1))
                                .text_xs()
                                .text_color(decision_color())
                                .child(decision),
                        )
                    })
                    .when_some(error, |row, error| {
                        row.child(
                            div()
                                .p_2()
                                .rounded_sm()
                                .border_1()
                                .border_color(with_alpha(rgb(0xff6b6b), 0.4))
                                .bg(with_alpha(rgb(0xff6b6b), 0.1))
                                .text_xs()
                                .text_color(rgb(0xff8f8f))
                                .child(error),
                        )
                    })
                    .into_any_element(),
            );
        }

        div()
            .id("operation-panel")
            .debug_selector(|| "operation-panel".to_string())
            .role(Role::Group)
            .aria_label("File operations")
            .absolute()
            .right(px(12.0 * scale))
            .bottom(px(bottom))
            .flex()
            .flex_col()
            .w(px(if self.operation_ui.panel_minimized {
                280.0
            } else {
                380.0
            } * scale))
            .max_h(px((440.0 * scale).min(
                (self
                    .layout
                    .last_window_bounds
                    .height
                    .unwrap_or(DEFAULT_WINDOW_HEIGHT)
                    - 80.0 * scale)
                    .max(160.0 * scale),
            )))
            .rounded_lg()
            .overflow_hidden()
            .border_1()
            .border_color(self.palette.border)
            .bg(self.palette.panel)
            .shadow_lg()
            .occlude()
            .child(
                div()
                    .id("operation-panel-header")
                    .debug_selector(|| "operation-panel-header".to_string())
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(44.0))
                    .px_3()
                    .flex_none()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.topbar)
                    .child(
                        div()
                            .w(px(10.0))
                            .h(px(10.0))
                            .rounded_full()
                            .bg(status_color),
                    )
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Operations"),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_full()
                            .bg(with_alpha(status_color, 0.16))
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(status_color)
                            .child(status_label),
                    )
                    .when(finished_count > 0, |header| {
                        header.child(
                            operation_icon_button(
                                "clear-operations",
                                "Clear finished operations",
                                "check",
                                self.palette,
                            )
                            .debug_selector(|| "clear-operations".to_string())
                            .on_click(
                                cx.listener(|this, _, _, cx| this.clear_completed_operations(cx)),
                            ),
                        )
                    })
                    .child(
                        operation_icon_button(
                            "minimize-operations",
                            if self.operation_ui.panel_minimized {
                                "Expand operations"
                            } else {
                                "Minimize operations"
                            },
                            if self.operation_ui.panel_minimized {
                                "arrow-up"
                            } else {
                                "minus"
                            },
                            self.palette,
                        )
                        .debug_selector(|| "minimize-operations".to_string())
                        .on_click(
                            cx.listener(|this, _, _, cx| this.toggle_operation_panel_minimized(cx)),
                        ),
                    )
                    .when(active_count == 0, |header| {
                        header.child(
                            operation_icon_button(
                                "close-operations",
                                "Close operations",
                                "close",
                                self.palette,
                            )
                            .debug_selector(|| "close-operations".to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.close_operation_panel(cx))),
                        )
                    }),
            )
            .when(!self.operation_ui.panel_minimized, |panel| {
                panel.child(
                    div()
                        .id("operation-panel-body")
                        .debug_selector(|| "operation-panel-body".to_string())
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .gap_2()
                        .p_2()
                        .overflow_y_scroll()
                        .children(rows),
                )
            })
            .into_any_element()
    }
}

/// The amber used for operations waiting on the user's decision.
fn decision_color() -> Rgba {
    rgb(0xfbbf24)
}

/// The status word and color an operation row shows.
fn operation_status_label(status: OperationStatus, palette: UiPalette) -> (&'static str, Rgba) {
    match status {
        OperationStatus::Running => ("In progress", palette.accent),
        OperationStatus::Completed => ("Completed", rgb(0x6fcf97)),
        OperationStatus::Cancelled => ("Cancelled", rgb(0xffb86c)),
        OperationStatus::Failed => ("Failed", rgb(0xff6b6b)),
        OperationStatus::NeedsDecision => ("Needs decision", decision_color()),
        OperationStatus::Skipped => ("Skipped", palette.muted),
    }
}
