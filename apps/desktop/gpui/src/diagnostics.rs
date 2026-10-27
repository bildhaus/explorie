use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

#[derive(Clone, Debug)]
pub(crate) struct DiagnosticsSnapshot {
    pub(crate) item_count: usize,
    pub(crate) selected_count: usize,
    pub(crate) tab_count: usize,
    pub(crate) favorite_count: usize,
    pub(crate) smart_folder_count: usize,
    pub(crate) current_path_present: bool,
    pub(crate) view_mode: String,
    pub(crate) theme: String,
    pub(crate) show_hidden: bool,
    pub(crate) show_system_files: bool,
    pub(crate) preview_open: bool,
    pub(crate) show_status_bar: bool,
    pub(crate) operation_count: usize,
    pub(crate) active_operation_count: usize,
    pub(crate) retry_available: bool,
    pub(crate) undo_available: bool,
    pub(crate) redo_available: bool,
    pub(crate) helper_count: usize,
    pub(crate) available_helper_count: usize,
    pub(crate) remote_profile_count: usize,
    pub(crate) remote_connected_count: usize,
    pub(crate) remote_connecting_count: usize,
    pub(crate) remote_error_count: usize,
    pub(crate) remote_retrying_count: usize,
    pub(crate) remote_exhausted_count: usize,
    pub(crate) previous_session_unclean: bool,
    pub(crate) interrupted_operation_count: usize,
    pub(crate) safely_retryable_operation_count: usize,
    pub(crate) completed_move_recovery_count: usize,
    pub(crate) manual_review_operation_count: usize,
    pub(crate) active_recovery_job_count: usize,
    pub(crate) error_reporting_enabled: bool,
    pub(crate) error_report_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticsReport<'a> {
    exported_at_unix_ms: u128,
    app: AppReport<'a>,
    browser: BrowserReport<'a>,
    operations: OperationReport,
    preview: PreviewReport,
    remote_drives: RemoteDriveReport,
    recovery: RecoveryReport,
    error_reporting: ErrorReportingReport,
    privacy: PrivacyReport<'a>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppReport<'a> {
    name: &'a str,
    version: &'a str,
    runtime: &'a str,
    platform: &'a str,
    architecture: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserReport<'a> {
    item_count: usize,
    selected_count: usize,
    tab_count: usize,
    favorite_count: usize,
    smart_folder_count: usize,
    current_path_present: bool,
    view_mode: &'a str,
    theme: &'a str,
    show_hidden: bool,
    show_system_files: bool,
    show_status_bar: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationReport {
    operation_count: usize,
    active_count: usize,
    retry_available: bool,
    undo_available: bool,
    redo_available: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewReport {
    open: bool,
    helper_count: usize,
    available_helper_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteDriveReport {
    profile_count: usize,
    connected_count: usize,
    connecting_count: usize,
    error_count: usize,
    retrying_count: usize,
    exhausted_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryReport {
    previous_session_unclean: bool,
    interrupted_operation_count: usize,
    safely_retryable_operation_count: usize,
    completed_move_count: usize,
    manual_review_count: usize,
    active_job_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorReportingReport {
    enabled: bool,
    in_memory_report_count: usize,
    capacity: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PrivacyReport<'a> {
    path_values: &'a str,
    network_required: bool,
}

pub(crate) fn create_diagnostics_json(snapshot: &DiagnosticsSnapshot) -> String {
    let report = DiagnosticsReport {
        exported_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        app: AppReport {
            name: "explorie",
            version: env!("CARGO_PKG_VERSION"),
            runtime: "gpui",
            platform: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
        },
        browser: BrowserReport {
            item_count: snapshot.item_count,
            selected_count: snapshot.selected_count,
            tab_count: snapshot.tab_count,
            favorite_count: snapshot.favorite_count,
            smart_folder_count: snapshot.smart_folder_count,
            current_path_present: snapshot.current_path_present,
            view_mode: &snapshot.view_mode,
            theme: &snapshot.theme,
            show_hidden: snapshot.show_hidden,
            show_system_files: snapshot.show_system_files,
            show_status_bar: snapshot.show_status_bar,
        },
        operations: OperationReport {
            operation_count: snapshot.operation_count,
            active_count: snapshot.active_operation_count,
            retry_available: snapshot.retry_available,
            undo_available: snapshot.undo_available,
            redo_available: snapshot.redo_available,
        },
        preview: PreviewReport {
            open: snapshot.preview_open,
            helper_count: snapshot.helper_count,
            available_helper_count: snapshot.available_helper_count,
        },
        remote_drives: RemoteDriveReport {
            profile_count: snapshot.remote_profile_count,
            connected_count: snapshot.remote_connected_count,
            connecting_count: snapshot.remote_connecting_count,
            error_count: snapshot.remote_error_count,
            retrying_count: snapshot.remote_retrying_count,
            exhausted_count: snapshot.remote_exhausted_count,
        },
        recovery: RecoveryReport {
            previous_session_unclean: snapshot.previous_session_unclean,
            interrupted_operation_count: snapshot.interrupted_operation_count,
            safely_retryable_operation_count: snapshot.safely_retryable_operation_count,
            completed_move_count: snapshot.completed_move_recovery_count,
            manual_review_count: snapshot.manual_review_operation_count,
            active_job_count: snapshot.active_recovery_job_count,
        },
        error_reporting: ErrorReportingReport {
            enabled: snapshot.error_reporting_enabled,
            in_memory_report_count: snapshot.error_report_count,
            capacity: 50,
        },
        privacy: PrivacyReport {
            path_values: "omitted",
            network_required: false,
        },
    };
    format!(
        "{}\n",
        serde_json::to_string_pretty(&report).expect("diagnostics report is serializable")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_are_local_runtime_facts_without_path_values() {
        let json = create_diagnostics_json(&DiagnosticsSnapshot {
            item_count: 4,
            selected_count: 1,
            tab_count: 2,
            favorite_count: 3,
            smart_folder_count: 1,
            current_path_present: true,
            view_mode: "grid".to_string(),
            theme: "dark".to_string(),
            show_hidden: true,
            show_system_files: false,
            preview_open: true,
            show_status_bar: true,
            operation_count: 2,
            active_operation_count: 1,
            retry_available: false,
            undo_available: true,
            redo_available: false,
            helper_count: 3,
            available_helper_count: 2,
            remote_profile_count: 2,
            remote_connected_count: 1,
            remote_connecting_count: 0,
            remote_error_count: 1,
            remote_retrying_count: 0,
            remote_exhausted_count: 1,
            previous_session_unclean: false,
            interrupted_operation_count: 3,
            safely_retryable_operation_count: 1,
            completed_move_recovery_count: 1,
            manual_review_operation_count: 1,
            active_recovery_job_count: 0,
            error_reporting_enabled: true,
            error_report_count: 2,
        });

        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["app"]["runtime"], "gpui");
        assert_eq!(value["browser"]["itemCount"], 4);
        assert_eq!(value["privacy"]["pathValues"], "omitted");
        assert_eq!(value["privacy"]["networkRequired"], false);
        assert_eq!(value["remoteDrives"]["profileCount"], 2);
        assert_eq!(value["remoteDrives"]["exhaustedCount"], 1);
        assert_eq!(value["recovery"]["interruptedOperationCount"], 3);
        assert_eq!(value["recovery"]["manualReviewCount"], 1);
        assert_eq!(value["errorReporting"]["enabled"], true);
        assert_eq!(value["errorReporting"]["inMemoryReportCount"], 2);
        assert!(!json.contains("C:\\Users"));
        assert!(!json.contains("/Users/"));
    }
}
