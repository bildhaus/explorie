//! Remote drive status helpers and editor state.

use crate::*;

pub(crate) fn path_base_name(path: &std::path::Path) -> ServiceResult<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            ServiceError::new(
                ErrorCode::InvalidInput,
                format!("Path has no file name: {}", path.display()),
            )
        })
}

pub(crate) fn remote_status(
    id: &str,
    state: RemoteDriveState,
    mount_path: Option<PathBuf>,
    error: Option<ServiceError>,
) -> RemoteDriveStatus {
    RemoteDriveStatus {
        id: id.to_string(),
        state,
        mount_path,
        error,
    }
}

pub(crate) const REMOTE_CONNECT_MAX_ATTEMPTS: u8 = 3;

#[cfg(test)]
pub(crate) const REMOTE_RETRY_BASE_DELAY: Duration = Duration::from_millis(25);
#[cfg(not(test))]
pub(crate) const REMOTE_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);

#[cfg(test)]
pub(crate) const REMOTE_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(25);
#[cfg(not(test))]
pub(crate) const REMOTE_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) fn remote_retry_delay(completed_attempt: u8) -> Duration {
    REMOTE_RETRY_BASE_DELAY.saturating_mul(1_u32 << completed_attempt.saturating_sub(1))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteRetryPhase {
    Connecting,
    Waiting,
    Exhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteRetryState {
    pub(crate) attempt: u8,
    pub(crate) max_attempts: u8,
    pub(crate) phase: RemoteRetryPhase,
    pub(crate) delay: Option<Duration>,
}

impl RemoteRetryState {
    pub(crate) fn connecting(attempt: u8) -> Self {
        Self {
            attempt,
            max_attempts: REMOTE_CONNECT_MAX_ATTEMPTS,
            phase: RemoteRetryPhase::Connecting,
            delay: None,
        }
    }
}

pub(crate) fn readable_retry_delay(delay: Duration) -> String {
    if delay.as_millis() < 1_000 {
        format!("{} ms", delay.as_millis())
    } else {
        format!("{} s", delay.as_secs())
    }
}

pub(crate) fn remote_error_guidance(error: &ServiceError) -> &'static str {
    match error.code {
        ErrorCode::HelperMissing => "Install or enable the required mount helper, then retry.",
        ErrorCode::PermissionDenied => {
            "Check access to the mount target and remote configuration, then retry."
        }
        ErrorCode::Conflict | ErrorCode::Busy => {
            "Choose an unused mount target or close the process using it, then retry."
        }
        ErrorCode::InvalidInput | ErrorCode::NotFound => {
            "Edit the profile or rclone configuration, then retry."
        }
        ErrorCode::RemoteUnavailable | ErrorCode::Io => {
            "Check the connection and rclone configuration, then retry."
        }
        ErrorCode::Cancelled => "Retry when you are ready.",
        ErrorCode::Unsupported | ErrorCode::Internal => {
            "Review Diagnostics, correct the setup, then retry."
        }
    }
}

pub(crate) fn remote_profile_detail(
    profile: &RemoteDriveProfile,
    status: &RemoteDriveStatus,
    retry: Option<&RemoteRetryState>,
) -> String {
    if let Some(retry) = retry {
        match retry.phase {
            RemoteRetryPhase::Connecting => {
                return format!(
                    "Connecting — attempt {} of {}",
                    retry.attempt, retry.max_attempts
                );
            }
            RemoteRetryPhase::Waiting => {
                let message = status
                    .error
                    .as_ref()
                    .map_or("Connection failed", |error| error.message.as_str());
                return format!(
                    "{message} • Retrying in {} (attempt {} of {})",
                    readable_retry_delay(retry.delay.unwrap_or_default()),
                    retry.attempt,
                    retry.max_attempts
                );
            }
            RemoteRetryPhase::Exhausted => {
                let Some(error) = status.error.as_ref() else {
                    return "Connection failed after automatic retries. Retry manually."
                        .to_string();
                };
                return format!(
                    "{} • Gave up after {} attempts. {}",
                    error.message,
                    retry.attempt,
                    remote_error_guidance(error)
                );
            }
        }
    }
    if let Some(error) = status.error.as_ref() {
        // Waiting for approval already says what to allow and where.
        if status.state == RemoteDriveState::ApprovalRequired {
            return error.message.clone();
        }
        return format!("{} • {}", error.message, remote_error_guidance(error));
    }
    format!(
        "{}:{} • {} • {}",
        profile.remote,
        profile.remote_path,
        profile.mount_target,
        status.state.as_str()
    )
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteEditorField {
    Name,
    Remote,
    RemotePath,
    MountTarget,
}

impl RemoteEditorField {
    pub(crate) fn label(self, platform: Option<&str>) -> &'static str {
        match self {
            Self::Name => "Display name",
            Self::Remote => "rclone remote",
            Self::RemotePath => "Subpath (optional)",
            Self::MountTarget if platform == Some("windows") => "Drive letter (D:–Z:)",
            Self::MountTarget => "Volume name",
        }
    }

    /// The hint shown in the empty field for this step.
    pub(crate) fn placeholder(self, platform: Option<&str>) -> &'static str {
        match self {
            Self::Name => "My NAS",
            Self::Remote => "Remote name from rclone config",
            Self::RemotePath => "Optional folder inside the remote",
            Self::MountTarget if platform == Some("windows") => "D:",
            Self::MountTarget if platform == Some("macos") => "Name shown in Finder",
            Self::MountTarget => "Volume name",
        }
    }

    pub(crate) fn next(self) -> Option<Self> {
        match self {
            Self::Name => Some(Self::Remote),
            Self::Remote => Some(Self::RemotePath),
            Self::RemotePath => Some(Self::MountTarget),
            Self::MountTarget => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RemoteProfileEditor {
    pub(crate) draft: RemoteDriveProfile,
    pub(crate) field: RemoteEditorField,
}

#[derive(Clone, Debug)]
pub(crate) struct BlockedRemoteDisconnect {
    pub(crate) id: String,
    pub(crate) pending_uploads: u64,
    pub(crate) errored_files: u64,
}
