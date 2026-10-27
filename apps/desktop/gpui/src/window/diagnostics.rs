//! `DirectoryWindow` behavior for diagnostics.

use crate::*;

impl DirectoryWindow {
    pub(crate) fn diagnostics_snapshot(&self) -> DiagnosticsSnapshot {
        let now = SystemTime::now();
        DiagnosticsSnapshot {
            item_count: self.browser.visible_entries().len(),
            selected_count: self.effective_selection_count(),
            tab_count: self.browser.tabs().len(),
            favorite_count: self.browser.favorites().len(),
            smart_folder_count: self.browser.smart_folders().len(),
            current_path_present: !self.browser.path().as_os_str().is_empty(),
            view_mode: self.browser.view_mode().label().to_lowercase(),
            theme: self.settings.appearance.theme.label().to_lowercase(),
            show_hidden: self.browser.show_hidden(),
            show_system_files: self.browser.show_system_files(),
            preview_open: !matches!(self.preview.state, PreviewState::Closed),
            show_status_bar: self.settings.view.show_status_bar,
            operation_count: self.operations.operations().len(),
            active_operation_count: self.process_active_operation_count(),
            retry_available: self.operations.latest_retryable_id().is_some(),
            undo_available: self.undo_ledger.can_undo(now),
            redo_available: self.undo_ledger.can_redo(),
            helper_count: self.preview.helpers.len(),
            available_helper_count: self
                .preview
                .helpers
                .iter()
                .filter(|helper| helper.available)
                .count(),
            remote_profile_count: self.settings.remote_profiles.len(),
            remote_connected_count: self
                .remote
                .statuses
                .values()
                .filter(|status| status.state == RemoteDriveState::Connected)
                .count(),
            remote_connecting_count: self
                .remote
                .statuses
                .values()
                .filter(|status| status.state == RemoteDriveState::Connecting)
                .count(),
            remote_error_count: self
                .remote
                .statuses
                .values()
                .filter(|status| status.state == RemoteDriveState::Error)
                .count(),
            remote_retrying_count: self
                .remote
                .retries
                .values()
                .filter(|retry| retry.phase != RemoteRetryPhase::Exhausted)
                .count(),
            remote_exhausted_count: self
                .remote
                .retries
                .values()
                .filter(|retry| retry.phase == RemoteRetryPhase::Exhausted)
                .count(),
            previous_session_unclean: self.recovery.previous_session_unclean,
            interrupted_operation_count: self.recovery.interrupted.len(),
            safely_retryable_operation_count: self
                .recovery
                .interrupted
                .iter()
                .filter(|operation| operation.disposition() == RecoveryDisposition::SafeToRetry)
                .count(),
            completed_move_recovery_count: self
                .recovery
                .interrupted
                .iter()
                .filter(|operation| operation.disposition() == RecoveryDisposition::CompletedMove)
                .count(),
            manual_review_operation_count: self
                .recovery
                .interrupted
                .iter()
                .filter(|operation| operation.disposition() == RecoveryDisposition::NeedsReview)
                .count(),
            active_recovery_job_count: self.recovery.jobs.len(),
            error_reporting_enabled: self.settings.behavior.enable_error_reporting,
            error_report_count: self.error_reports.len(),
        }
    }

    pub(crate) fn record_error(&mut self, operation: impl Into<String>, error: impl AsRef<str>) {
        if self.settings.behavior.enable_error_reporting {
            self.error_reports.record(operation, error);
        }
    }

    pub(crate) fn copy_diagnostics(&mut self, cx: &mut Context<Self>) {
        let json = create_diagnostics_json(&self.diagnostics_snapshot());
        cx.write_to_clipboard(ClipboardItem::new_string(json));
        self.show_toast(
            "Path-free native diagnostics copied to the clipboard",
            ToastKind::Success,
            cx,
        );
    }

    pub(crate) fn copy_error_reports(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.error_reports.export_json()));
        self.show_toast(
            "Path-redacted error reports copied to the clipboard",
            ToastKind::Success,
            cx,
        );
    }

    pub(crate) fn clear_error_reports(&mut self, cx: &mut Context<Self>) {
        self.error_reports.clear();
        self.show_toast("Cleared in-memory error reports", ToastKind::Success, cx);
    }
}
