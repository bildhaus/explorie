use std::{collections::VecDeque, time::SystemTime};

use serde::Serialize;

const MAX_ERROR_REPORTS: usize = 50;
const MAX_MESSAGE_CHARS: usize = 400;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ErrorReport {
    id: u64,
    timestamp_unix_ms: u128,
    operation: String,
    category: String,
    message: String,
}

impl ErrorReport {
    pub(crate) fn operation(&self) -> &str {
        &self.operation
    }

    pub(crate) fn category(&self) -> &str {
        &self.category
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Default)]
pub(crate) struct ErrorReportLog {
    reports: VecDeque<ErrorReport>,
    next_id: u64,
}

impl ErrorReportLog {
    pub(crate) fn record(&mut self, operation: impl Into<String>, error: impl AsRef<str>) {
        self.next_id = self.next_id.wrapping_add(1);
        let error = error.as_ref();
        let category = classify_error(error);
        let message = redact_sensitive_values(error);
        self.reports.push_front(ErrorReport {
            id: self.next_id,
            timestamp_unix_ms: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            operation: operation.into(),
            category: category.to_string(),
            message,
        });
        self.reports.truncate(MAX_ERROR_REPORTS);
    }

    pub(crate) fn reports(&self) -> impl Iterator<Item = &ErrorReport> {
        self.reports.iter()
    }

    pub(crate) fn len(&self) -> usize {
        self.reports.len()
    }

    pub(crate) fn clear(&mut self) {
        self.reports.clear();
    }

    pub(crate) fn export_json(&self) -> String {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Export<'a> {
            exported_at_unix_ms: u128,
            count: usize,
            reports: &'a VecDeque<ErrorReport>,
        }

        let export = Export {
            exported_at_unix_ms: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            count: self.reports.len(),
            reports: &self.reports,
        };
        format!(
            "{}\n",
            serde_json::to_string_pretty(&export).expect("error reports are serializable")
        )
    }
}

fn redact_sensitive_values(message: &str) -> String {
    let normalized = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let path_start = normalized.char_indices().find_map(|(index, character)| {
        let suffix = &normalized[index..];
        let boundary = index == 0
            || normalized[..index]
                .chars()
                .next_back()
                .is_some_and(|previous| {
                    previous.is_whitespace() || matches!(previous, '(' | '[' | '"' | '\'')
                });
        let drive_path = boundary
            && character.is_ascii_alphabetic()
            && suffix
                .get(1..3)
                .is_some_and(|value| value == ":\\" || value == ":/");
        let unc_path = boundary && suffix.starts_with("\\\\");
        let unix_path = boundary && suffix.starts_with('/');
        (drive_path || unc_path || unix_path).then_some(index)
    });

    let lowercase = normalized.to_ascii_lowercase();
    let sensitive_start = [
        "http://",
        "https://",
        "password=",
        "password:",
        "token=",
        "token:",
        "secret=",
        "secret:",
        "rc_pass",
    ]
    .into_iter()
    .filter_map(|needle| lowercase.find(needle))
    .min();
    let redaction_start = match (path_start, sensitive_start) {
        (Some(path), Some(sensitive)) => Some(path.min(sensitive)),
        (path, sensitive) => path.or(sensitive),
    };

    let redacted =
        redaction_start.map_or(normalized.as_str(), |index| normalized[..index].trim_end());
    let suffix = redaction_start
        .map(|_| " [sensitive value redacted]")
        .unwrap_or_default();
    redacted
        .chars()
        .chain(suffix.chars())
        .take(MAX_MESSAGE_CHARS)
        .collect::<String>()
}

fn classify_error(message: &str) -> &'static str {
    let message = message.to_ascii_lowercase();
    if message.contains("permission") || message.contains("access denied") {
        "access"
    } else if message.contains("not found") || message.contains("does not exist") {
        "not-found"
    } else if message.contains("already exists") || message.contains("conflict") {
        "conflict"
    } else if message.contains("network")
        || message.contains("offline")
        || message.contains("connect")
    {
        "network"
    } else if message.contains("cancel") {
        "cancelled"
    } else {
        "native"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_newest_reports_and_classifies_them() {
        let mut log = ErrorReportLog::default();
        for index in 0..55 {
            log.record(format!("Operation {index}"), "Permission denied");
        }

        assert_eq!(log.len(), 50);
        assert_eq!(log.reports().next().unwrap().operation(), "Operation 54");
        assert_eq!(log.reports().next().unwrap().category(), "access");
        assert_eq!(log.reports().last().unwrap().operation(), "Operation 5");
    }

    #[test]
    fn exports_path_redacted_windows_and_unix_errors() {
        let mut log = ErrorReportLog::default();
        log.record(
            "Open failed",
            r#"Could not open C:\Users\omer\Private\notes.txt: access denied"#,
        );
        log.record(
            "Reveal failed",
            "Could not reveal /Users/omer/Private/notes.txt: not found",
        );

        let json = log.export_json();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["count"], 2);
        assert!(json.contains("[sensitive value redacted]"));
        assert!(!json.contains("Users"));
        assert!(!json.contains("Private"));
        assert!(!json.contains("notes.txt"));
    }

    #[test]
    fn preserves_path_free_detail_and_clears() {
        let mut log = ErrorReportLog::default();
        log.record("Remote connect failed", "Remote is offline; retry later");
        assert_eq!(
            log.reports().next().unwrap().message(),
            "Remote is offline; retry later"
        );
        assert_eq!(log.reports().next().unwrap().category(), "network");

        log.clear();
        assert_eq!(log.len(), 0);
    }

    #[test]
    fn redacts_urls_and_credential_shaped_values() {
        let mut log = ErrorReportLog::default();
        log.record(
            "Remote failed",
            "request to https://user:secret@example.test failed",
        );
        log.record("Helper failed", "password=do-not-export");

        let json = log.export_json();
        assert!(!json.contains("example.test"));
        assert!(!json.contains("do-not-export"));
        assert!(!json.contains("user:secret"));
    }
}
