use crate::listing::normalize_path;
use crate::{
    ActiveOperation, BlockingTask, ErrorCode, ResourcePaths, ServiceContext, ServiceError,
    ServiceEvent, ServiceResult, SharedState,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::fs;
#[cfg(any(windows, target_os = "macos"))]
use std::fs::File;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
#[cfg(any(windows, target_os = "macos"))]
use std::process::Child;
use std::process::Command;
#[cfg(any(windows, target_os = "macos"))]
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::process::{ProcessError, ProcessOutput, run_with_timeout};

const READY_TIMEOUT: Duration = Duration::from_secs(15);
const RC_TIMEOUT: Duration = Duration::from_secs(5);
const RCLONE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const RCLONE_LIST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RCLONE_OUTPUT: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDriveProfile {
    pub id: String,
    pub name: String,
    pub remote: String,
    #[serde(default)]
    pub remote_path: String,
    pub mount_target: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDriveEnvironment {
    pub platform: String,
    pub rclone_available: bool,
    pub rclone_version: Option<String>,
    pub winfsp_available: Option<bool>,
    pub helper_status: Option<String>,
    pub occupied_mount_targets: Vec<String>,
    pub error: Option<ServiceError>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteDriveState {
    ApprovalRequired,
    Connecting,
    Connected,
    Disconnecting,
    Disconnected,
    Error,
}

impl RemoteDriveState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalRequired => "approval-required",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Disconnecting => "disconnecting",
            Self::Disconnected => "disconnected",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDriveStatus {
    pub id: String,
    pub state: RemoteDriveState,
    pub mount_path: Option<PathBuf>,
    pub error: Option<ServiceError>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisconnectResult {
    pub status: RemoteDriveStatus,
    pub pending_uploads: u64,
    pub errored_files: u64,
    pub blocked: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDriveExitBlocker {
    pub pending_uploads: u64,
    pub errored_files: u64,
    pub error: Option<ServiceError>,
}

struct RunningMount {
    child: Box<dyn RemoteDriveProcess>,
    rclone: PathBuf,
    rc_url: String,
    rc_user: String,
    rc_pass: String,
    mount_path: PathBuf,
}

/// The information needed to start one remote-drive process.
///
/// Credentials are generated per mount and are only held for the lifetime of
/// the process; implementations must not persist them.
#[derive(Clone)]
pub struct RemoteMountRequest {
    pub profile: RemoteDriveProfile,
    pub rclone: PathBuf,
    pub cache_dir: PathBuf,
    pub rc_url: String,
    pub rc_user: String,
    pub rc_pass: String,
    pub helper_port: Option<u16>,
}

impl fmt::Debug for RemoteMountRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteMountRequest")
            .field("profile", &self.profile)
            .field("rclone", &self.rclone)
            .field("cache_dir", &self.cache_dir)
            .field("rc_url", &self.rc_url)
            .field("rc_user", &self.rc_user)
            .field("rc_pass", &"[redacted]")
            .field("helper_port", &self.helper_port)
            .finish()
    }
}

/// A remote-control request sent to the running rclone process.
#[derive(Clone)]
pub struct RemoteControlRequest {
    pub rclone: PathBuf,
    pub rc_url: String,
    pub rc_user: String,
    pub rc_pass: String,
    pub endpoint: String,
}

impl fmt::Debug for RemoteControlRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteControlRequest")
            .field("rclone", &self.rclone)
            .field("rc_url", &self.rc_url)
            .field("rc_user", &self.rc_user)
            .field("rc_pass", &"[redacted]")
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

/// A process status independent of `std::process::Child`, so tests can inject
/// a deterministic process without changing production lifecycle behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteProcessStatus {
    pub success: bool,
    pub code: Option<i32>,
}

impl std::fmt::Display for RemoteProcessStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code {
            Some(code) => write!(formatter, "exit code {code}"),
            None if self.success => formatter.write_str("success"),
            None => formatter.write_str("unknown exit status"),
        }
    }
}

/// Process lifecycle required by the remote-drive service.
pub trait RemoteDriveProcess: Send {
    fn try_wait(&mut self) -> ServiceResult<Option<RemoteProcessStatus>>;
    fn wait(&mut self) -> ServiceResult<RemoteProcessStatus>;
    fn kill(&mut self) -> ServiceResult<()>;
}

/// Host-provided remote-drive side effects.
///
/// The default implementation delegates to rclone, WinFsp, and the macOS
/// helper. Tests and GPUI hosts can inject a fake implementation without
/// changing process or helper safety checks in production.
pub trait RemoteDriveBackend: Send + Sync {
    fn find_rclone(&self, resources: &ResourcePaths) -> Option<PathBuf>;
    fn rclone_version(&self, rclone: &Path) -> ServiceResult<String>;
    fn list_remotes(&self, rclone: &Path) -> ServiceResult<Vec<String>>;
    fn ensure_capabilities(&self, rclone: &Path) -> ServiceResult<()>;
    fn winfsp_available(&self) -> Option<bool>;
    fn occupied_mount_targets(&self) -> Vec<String>;
    fn helper_status(&self) -> Option<String>;
    fn configure(&self, rclone: &Path, resources: &ResourcePaths) -> ServiceResult<()>;
    fn start_mount(
        &self,
        request: &RemoteMountRequest,
    ) -> ServiceResult<Box<dyn RemoteDriveProcess>>;
    fn remote_control(&self, request: &RemoteControlRequest) -> ServiceResult<Value>;
    fn mount_helper(&self, id: &str, volume_name: &str, port: u16) -> ServiceResult<()>;
    fn unmount_helper(&self, id: &str, volume_name: &str, force: bool) -> ServiceResult<()>;
    fn install_winfsp(&self, context: &ServiceContext) -> ServiceResult<()>;
    fn register_helper(&self) -> ServiceResult<String>;
    fn unregister_helper(&self) -> ServiceResult<()>;
    fn open_helper_settings(&self) -> ServiceResult<()>;
}

struct SystemRemoteDriveBackend;

#[cfg(any(windows, target_os = "macos"))]
struct SystemRemoteDriveProcess(Child);

#[cfg(any(windows, target_os = "macos"))]
impl RemoteDriveProcess for SystemRemoteDriveProcess {
    fn try_wait(&mut self) -> ServiceResult<Option<RemoteProcessStatus>> {
        self.0
            .try_wait()
            .map(|status| status.map(process_status))
            .map_err(ServiceError::from)
    }

    fn wait(&mut self) -> ServiceResult<RemoteProcessStatus> {
        self.0
            .wait()
            .map(process_status)
            .map_err(ServiceError::from)
    }

    fn kill(&mut self) -> ServiceResult<()> {
        self.0.kill().map_err(ServiceError::from)
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn process_status(status: std::process::ExitStatus) -> RemoteProcessStatus {
    RemoteProcessStatus {
        success: status.success(),
        code: status.code(),
    }
}

impl RemoteDriveBackend for SystemRemoteDriveBackend {
    fn find_rclone(&self, resources: &ResourcePaths) -> Option<PathBuf> {
        find_rclone(resources)
    }

    fn rclone_version(&self, rclone: &Path) -> ServiceResult<String> {
        rclone_version(rclone)
    }

    fn list_remotes(&self, rclone: &Path) -> ServiceResult<Vec<String>> {
        list_remotes_with_command(rclone)
    }

    fn ensure_capabilities(&self, rclone: &Path) -> ServiceResult<()> {
        ensure_rclone_capabilities(rclone)
    }

    fn winfsp_available(&self) -> Option<bool> {
        winfsp_available()
    }

    fn occupied_mount_targets(&self) -> Vec<String> {
        occupied_mount_targets()
    }

    fn helper_status(&self) -> Option<String> {
        macos_helper_status()
    }

    fn configure(&self, rclone: &Path, resources: &ResourcePaths) -> ServiceResult<()> {
        configure_rclone_with_system(rclone, resources)
    }

    fn start_mount(
        &self,
        request: &RemoteMountRequest,
    ) -> ServiceResult<Box<dyn RemoteDriveProcess>> {
        start_mount_with_system(request)
    }

    fn remote_control(&self, request: &RemoteControlRequest) -> ServiceResult<Value> {
        remote_control_with_system(request)
    }

    fn mount_helper(&self, id: &str, volume_name: &str, port: u16) -> ServiceResult<()> {
        #[cfg(target_os = "macos")]
        {
            // The privileged helper refuses to mount unless rclone is already
            // listening, and rclone opens its NFS socket only after reaching
            // the remote, which can be after its remote-control API is ready.
            wait_for_loopback_listener(port, READY_TIMEOUT)?;
            macos::mount(id, volume_name, port)
                .map_err(|error| ServiceError::new(ErrorCode::HelperMissing, error))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (id, volume_name, port);
            Ok(())
        }
    }

    fn unmount_helper(&self, id: &str, volume_name: &str, force: bool) -> ServiceResult<()> {
        #[cfg(target_os = "macos")]
        {
            macos::unmount(id, volume_name, force)
                .map_err(|error| ServiceError::new(ErrorCode::RemoteUnavailable, error))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (id, volume_name, force);
            Ok(())
        }
    }

    fn install_winfsp(&self, context: &ServiceContext) -> ServiceResult<()> {
        install_winfsp_blocking(context)
    }

    fn register_helper(&self) -> ServiceResult<String> {
        #[cfg(target_os = "macos")]
        {
            macos::register().map_err(|error| ServiceError::new(ErrorCode::HelperMissing, error))
        }
        #[cfg(not(target_os = "macos"))]
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "The Remote Drives helper is only available on macOS.",
        ))
    }

    fn unregister_helper(&self) -> ServiceResult<()> {
        #[cfg(target_os = "macos")]
        {
            macos::unregister().map_err(|error| ServiceError::new(ErrorCode::HelperMissing, error))
        }
        #[cfg(not(target_os = "macos"))]
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "The Remote Drives helper is only available on macOS.",
        ))
    }

    fn open_helper_settings(&self) -> ServiceResult<()> {
        #[cfg(target_os = "macos")]
        {
            macos::open_settings();
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "The Remote Drives helper is only available on macOS.",
        ))
    }
}

#[derive(Clone)]
pub struct RemoteDriveService {
    context: ServiceContext,
    shared: Arc<SharedState>,
    mounts: Arc<Mutex<HashMap<String, RunningMount>>>,
    backend: Arc<dyn RemoteDriveBackend>,
}

impl RemoteDriveService {
    pub(crate) fn new(context: ServiceContext, shared: Arc<SharedState>) -> Self {
        Self::with_backend(context, shared, Arc::new(SystemRemoteDriveBackend))
    }

    pub(crate) fn with_backend(
        context: ServiceContext,
        shared: Arc<SharedState>,
        backend: Arc<dyn RemoteDriveBackend>,
    ) -> Self {
        Self {
            context,
            shared,
            mounts: Arc::new(Mutex::new(HashMap::new())),
            backend,
        }
    }

    pub fn environment(&self) -> BlockingTask<RemoteDriveEnvironment> {
        let service = self.clone();
        self.context
            .spawn_blocking(move || Ok(service.environment_blocking()))
    }

    pub fn environment_blocking(&self) -> RemoteDriveEnvironment {
        let rclone = self.find_rclone();
        let version = rclone
            .as_ref()
            .and_then(|path| self.backend.rclone_version(path).ok());
        RemoteDriveEnvironment {
            platform: std::env::consts::OS.to_string(),
            rclone_available: rclone.is_some(),
            rclone_version: version,
            winfsp_available: self.backend.winfsp_available(),
            helper_status: self.backend.helper_status(),
            occupied_mount_targets: self.backend.occupied_mount_targets(),
            error: rclone.is_none().then(|| {
                ServiceError::new(ErrorCode::RemoteUnavailable, missing_rclone()).retryable(true)
            }),
        }
    }

    pub fn list_remotes(&self) -> BlockingTask<Vec<String>> {
        let service = self.clone();
        self.context
            .spawn_blocking(move || service.list_remotes_blocking())
    }

    pub fn list_remotes_blocking(&self) -> ServiceResult<Vec<String>> {
        let rclone = self.find_rclone().ok_or_else(|| {
            ServiceError::new(ErrorCode::RemoteUnavailable, missing_rclone()).retryable(true)
        })?;
        self.backend.list_remotes(&rclone)
    }

    pub fn configure(&self) -> BlockingTask<()> {
        let service = self.clone();
        self.context
            .spawn_blocking(move || service.configure_blocking())
    }

    pub fn configure_blocking(&self) -> ServiceResult<()> {
        let _guard = ActiveOperation::new(Arc::clone(&self.shared));
        let rclone = self.find_rclone().ok_or_else(|| {
            ServiceError::new(ErrorCode::RemoteUnavailable, missing_rclone()).retryable(true)
        })?;
        self.backend.configure(&rclone, self.context.resources())
    }

    pub fn connect(&self, profile: RemoteDriveProfile) -> BlockingTask<RemoteDriveStatus> {
        let service = self.clone();
        self.context
            .spawn_blocking(move || service.connect_blocking(profile))
    }

    pub fn connect_blocking(
        &self,
        profile: RemoteDriveProfile,
    ) -> ServiceResult<RemoteDriveStatus> {
        let _guard = ActiveOperation::new(Arc::clone(&self.shared));
        validate_remote_drive_profile(&profile)?;
        #[cfg(target_os = "macos")]
        if self.backend.helper_status().as_deref() != Some("enabled") {
            let approval = status(&profile.id, RemoteDriveState::ApprovalRequired, None, None);
            self.publish_status(approval.clone());
            return Ok(approval);
        }

        {
            let mut mounts = self
                .mounts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(existing) = mounts.get_mut(&profile.id) {
                if existing.child.try_wait()?.is_none() {
                    return Ok(connected_status(&profile.id, &existing.mount_path));
                }
                let stale_root = normalize_path(&existing.mount_path);
                mounts.remove(&profile.id);
                self.shared
                    .remote_roots
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&stale_root);
            }
        }

        self.publish_status(status(
            &profile.id,
            RemoteDriveState::Connecting,
            None,
            None,
        ));
        let result = self.connect_inner(&profile);
        match result {
            Ok(connected) => {
                self.shared
                    .remote_roots
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(normalize_path(
                        connected.mount_path.as_deref().unwrap_or(Path::new("")),
                    ));
                self.publish_status(connected.clone());
                Ok(connected)
            }
            Err(error) => {
                self.publish_status(status(
                    &profile.id,
                    RemoteDriveState::Error,
                    None,
                    Some(error.clone()),
                ));
                Err(error)
            }
        }
    }

    fn connect_inner(&self, profile: &RemoteDriveProfile) -> ServiceResult<RemoteDriveStatus> {
        let rclone = self.find_rclone().ok_or_else(|| {
            ServiceError::new(ErrorCode::RemoteUnavailable, missing_rclone()).retryable(true)
        })?;
        self.backend.ensure_capabilities(&rclone)?;
        if !self
            .backend
            .list_remotes(&rclone)?
            .iter()
            .any(|remote| remote == &profile.remote)
        {
            return Err(ServiceError::new(
                ErrorCode::RemoteUnavailable,
                "The selected rclone remote is no longer configured.",
            ));
        }
        #[cfg(windows)]
        if self.backend.winfsp_available() != Some(true) {
            return Err(ServiceError::new(
                ErrorCode::HelperMissing,
                "Install WinFsp before mounting remote drives on Windows.",
            ));
        }

        let mount_path = mount_path(profile)?;
        ensure_mount_target_available(&mount_path)?;
        let cache_dir = profile_cache_dir(&self.context.resources().cache_dir, &profile.id);
        fs::create_dir_all(&cache_dir).map_err(ServiceError::from)?;
        let rc_port = free_port()?;
        let rc_url = format!("http://127.0.0.1:{rc_port}/");
        let rc_user = "explorie".to_string();
        let rc_pass = Uuid::new_v4().simple().to_string();
        #[cfg(target_os = "macos")]
        let helper_port = Some(free_port()?);
        #[cfg(not(target_os = "macos"))]
        let helper_port = None;
        let request = RemoteMountRequest {
            profile: profile.clone(),
            rclone: rclone.clone(),
            cache_dir,
            rc_url: rc_url.clone(),
            rc_user: rc_user.clone(),
            rc_pass: rc_pass.clone(),
            helper_port,
        };
        let mut child = self.backend.start_mount(&request)?;
        if let Err(error) = wait_for_rc(
            child.as_mut(),
            self.backend.as_ref(),
            &rclone,
            &rc_url,
            &rc_user,
            &rc_pass,
        ) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }

        #[cfg(target_os = "macos")]
        if let Some(port) = helper_port
            && let Err(error) = self
                .backend
                .mount_helper(&profile.id, &profile.mount_target, port)
        {
            let _ = self.backend.remote_control(&RemoteControlRequest {
                rclone: rclone.clone(),
                rc_url: rc_url.clone(),
                rc_user: rc_user.clone(),
                rc_pass: rc_pass.clone(),
                endpoint: "core/quit".to_string(),
            });
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }

        let running = RunningMount {
            child,
            rclone,
            rc_url,
            rc_user,
            rc_pass,
            mount_path: mount_path.clone(),
        };
        self.mounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(profile.id.clone(), running);
        Ok(connected_status(&profile.id, &mount_path))
    }

    pub fn disconnect(&self, id: String, force: bool) -> BlockingTask<DisconnectResult> {
        let service = self.clone();
        self.context
            .spawn_blocking(move || service.disconnect_blocking(&id, force))
    }

    pub fn disconnect_blocking(&self, id: &str, force: bool) -> ServiceResult<DisconnectResult> {
        let _guard = ActiveOperation::new(Arc::clone(&self.shared));
        let mut mount = {
            let mut mounts = self
                .mounts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(mount) = mounts.remove(id) else {
                return Ok(DisconnectResult {
                    status: status(id, RemoteDriveState::Disconnected, None, None),
                    pending_uploads: 0,
                    errored_files: 0,
                    blocked: false,
                });
            };
            mount
        };

        let (pending, errors) =
            match disconnect_stats(force, || vfs_stats(self.backend.as_ref(), &mount)) {
                Ok(stats) => stats,
                Err(error) => {
                    let mount_path = mount.mount_path.clone();
                    self.mounts
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(id.to_string(), mount);
                    let service_error = ServiceError::new(
                        ErrorCode::RemoteUnavailable,
                        format!("Unable to verify pending remote writes: {error}"),
                    )
                    .retryable(true);
                    return Ok(DisconnectResult {
                        status: status(
                            id,
                            RemoteDriveState::Connected,
                            Some(mount_path),
                            Some(service_error),
                        ),
                        pending_uploads: 0,
                        errored_files: 0,
                        blocked: true,
                    });
                }
            };
        if !force && (pending > 0 || errors > 0) {
            let mount_path = mount.mount_path.clone();
            self.mounts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.to_string(), mount);
            return Ok(DisconnectResult {
                status: status(id, RemoteDriveState::Connected, Some(mount_path), None),
                pending_uploads: pending,
                errored_files: errors,
                blocked: true,
            });
        }

        self.publish_status(status(
            id,
            RemoteDriveState::Disconnecting,
            Some(mount.mount_path.clone()),
            None,
        ));
        let mut cleanup_error = None;
        #[cfg(target_os = "macos")]
        {
            match macos_volume_name(&mount.mount_path) {
                Ok(volume_name) => {
                    if let Err(error) = self.backend.unmount_helper(id, &volume_name, force) {
                        cleanup_error = Some(error.retryable(true));
                        if !force {
                            self.mounts
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(id.to_string(), mount);
                            return Err(cleanup_error.expect("cleanup error is set"));
                        }
                    }
                }
                Err(error) => {
                    cleanup_error = Some(error);
                    if !force {
                        self.mounts
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .insert(id.to_string(), mount);
                        return Err(cleanup_error.expect("cleanup error is set"));
                    }
                }
            }
        }
        if let Err(error) = rc_call(
            self.backend.as_ref(),
            &mount.rclone,
            &mount.rc_url,
            &mount.rc_user,
            &mount.rc_pass,
            "core/quit",
        ) && cleanup_error.is_none()
        {
            cleanup_error = Some(error);
        }
        if let Err(error) = wait_or_kill(&mut *mount.child)
            && cleanup_error.is_none()
        {
            cleanup_error = Some(error);
        }
        if let Some(error) = cleanup_error {
            let mount_path = mount.mount_path.clone();
            self.mounts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.to_string(), mount);
            self.publish_status(status(
                id,
                RemoteDriveState::Error,
                Some(mount_path),
                Some(error.clone()),
            ));
            return Err(error);
        }
        self.shared
            .remote_roots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&normalize_path(&mount.mount_path));
        let status = status(id, RemoteDriveState::Disconnected, None, None);
        self.publish_status(status.clone());
        Ok(DisconnectResult {
            status,
            pending_uploads: pending,
            errored_files: errors,
            blocked: false,
        })
    }

    pub fn statuses(&self) -> Vec<RemoteDriveStatus> {
        self.mounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter_mut()
            .map(|(id, mount)| match mount.child.try_wait() {
                Ok(Some(exit)) => RemoteDriveStatus {
                    id: id.clone(),
                    state: RemoteDriveState::Error,
                    mount_path: Some(mount.mount_path.clone()),
                    error: Some(
                        ServiceError::new(
                            ErrorCode::RemoteUnavailable,
                            format!("rclone exited with {exit}"),
                        )
                        .retryable(true),
                    ),
                },
                Ok(None) => connected_status(id, &mount.mount_path),
                Err(error) => RemoteDriveStatus {
                    id: id.clone(),
                    state: RemoteDriveState::Error,
                    mount_path: Some(mount.mount_path.clone()),
                    error: Some(error),
                },
            })
            .collect()
    }

    pub fn statuses_task(&self) -> BlockingTask<Vec<RemoteDriveStatus>> {
        let service = self.clone();
        self.context.spawn_blocking(move || Ok(service.statuses()))
    }

    pub fn is_mount_root(&self, path: &Path) -> bool {
        self.shared
            .remote_roots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&normalize_path(path))
    }

    pub fn mount_paths(&self) -> Vec<PathBuf> {
        self.mounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|mount| mount.mount_path.clone())
            .collect()
    }

    pub fn disconnect_all(&self) -> BlockingTask<()> {
        let service = self.clone();
        self.context
            .spawn_blocking(move || service.disconnect_all_blocking())
    }

    pub fn disconnect_all_blocking(&self) -> ServiceResult<()> {
        let _guard = ActiveOperation::new(Arc::clone(&self.shared));
        let ids: Vec<String> = self
            .mounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        let mut first_error = None;
        for id in ids {
            if let Err(error) = self.disconnect_blocking(&id, true)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub fn disconnect_all_if_clean(&self) -> bool {
        let _guard = ActiveOperation::new(Arc::clone(&self.shared));
        let ids: Vec<String> = self
            .mounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        let mut blocker = RemoteDriveExitBlocker {
            pending_uploads: 0,
            errored_files: 0,
            error: None,
        };
        for id in ids {
            match self.disconnect_blocking(&id, false) {
                Ok(result) if result.blocked => {
                    blocker.pending_uploads += result.pending_uploads;
                    blocker.errored_files += result.errored_files;
                    if result.status.error.is_some() {
                        blocker.error = result.status.error;
                    }
                }
                Ok(_) => {}
                Err(error) => blocker.error = Some(error),
            }
        }
        let clean =
            blocker.pending_uploads == 0 && blocker.errored_files == 0 && blocker.error.is_none();
        if !clean {
            self.context
                .events()
                .publish(ServiceEvent::RemoteDriveExitBlocked(blocker));
        }
        clean
    }

    pub fn disconnect_all_if_clean_task(&self) -> BlockingTask<bool> {
        let service = self.clone();
        self.context
            .spawn_blocking(move || Ok(service.disconnect_all_if_clean()))
    }

    pub fn install_winfsp(&self) -> BlockingTask<()> {
        let service = self.clone();
        self.context.spawn_blocking(move || {
            let _guard = ActiveOperation::new(Arc::clone(&service.shared));
            service.backend.install_winfsp(&service.context)
        })
    }

    pub fn register_helper(&self) -> BlockingTask<String> {
        let backend = Arc::clone(&self.backend);
        let shared = Arc::clone(&self.shared);
        self.context.spawn_blocking(move || {
            let _guard = ActiveOperation::new(shared);
            backend.register_helper()
        })
    }

    pub fn unregister_helper(&self) -> BlockingTask<()> {
        let service = self.clone();
        self.context.spawn_blocking(move || {
            let _guard = ActiveOperation::new(Arc::clone(&service.shared));
            service.disconnect_all_blocking()?;
            service.backend.unregister_helper()
        })
    }

    pub fn open_helper_settings(&self) -> BlockingTask<()> {
        let backend = Arc::clone(&self.backend);
        self.context
            .spawn_blocking(move || backend.open_helper_settings())
    }

    fn publish_status(&self, status: RemoteDriveStatus) {
        self.context
            .events()
            .publish(ServiceEvent::RemoteDriveStatus(status));
    }

    fn find_rclone(&self) -> Option<PathBuf> {
        self.backend.find_rclone(self.context.resources())
    }
}

pub fn validate_remote_drive_profile(profile: &RemoteDriveProfile) -> ServiceResult<()> {
    if Uuid::parse_str(&profile.id).is_err() {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "Remote drive ID must be a UUID.",
        ));
    }
    if profile.name.trim().is_empty()
        || profile.name.len() > 64
        || profile.name.chars().any(char::is_control)
    {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "Remote drive name must be 1-64 printable characters.",
        ));
    }
    if profile.remote.trim().is_empty()
        || profile.remote.contains([':', '/', '\\'])
        || profile.remote.chars().any(char::is_control)
    {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "Select a configured rclone remote.",
        ));
    }
    if profile
        .remote_path
        .split('/')
        .any(|part| part == "." || part == ".." || part.contains('\\'))
        || profile.remote_path.starts_with('/')
        || profile.remote_path.chars().any(char::is_control)
    {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "Remote subpath must be a relative rclone path.",
        ));
    }
    #[cfg(windows)]
    if !is_windows_drive_target(&profile.mount_target) {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "Choose an unused Windows drive letter from D: through Z:.",
        ));
    }
    #[cfg(target_os = "macos")]
    validate_volume_name(&profile.mount_target)?;
    Ok(())
}

#[cfg(windows)]
fn is_windows_drive_target(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 2 && (b'D'..=b'Z').contains(&bytes[0].to_ascii_uppercase()) && bytes[1] == b':'
}

#[cfg(target_os = "macos")]
fn validate_volume_name(value: &str) -> ServiceResult<()> {
    if value.trim().is_empty()
        || value.len() > 64
        || matches!(value, "." | "..")
        || value.contains(['/', '\\'])
        || value.chars().any(char::is_control)
    {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "macOS volume names must be 1-64 printable path-safe characters.",
        ));
    }
    Ok(())
}

#[cfg(any(windows, target_os = "macos", test))]
fn remote_spec(profile: &RemoteDriveProfile) -> String {
    let path = profile.remote_path.trim_matches('/');
    if path.is_empty() {
        format!("{}:", profile.remote)
    } else {
        format!("{}:{path}", profile.remote)
    }
}

fn mount_path(profile: &RemoteDriveProfile) -> ServiceResult<PathBuf> {
    #[cfg(windows)]
    return Ok(PathBuf::from(format!(
        "{}\\",
        profile.mount_target.to_ascii_uppercase()
    )));
    #[cfg(target_os = "macos")]
    return Ok(Path::new("/Volumes").join(&profile.mount_target));
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = profile;
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Remote Drives currently support Windows and macOS only.",
        ))
    }
}

fn ensure_mount_target_available(path: &Path) -> ServiceResult<()> {
    if path.try_exists().map_err(ServiceError::from)? {
        return Err(ServiceError::new(
            ErrorCode::Conflict,
            format!("Mount target is already in use: {}", path.display()),
        ));
    }
    Ok(())
}

fn profile_cache_dir(app_cache_dir: &Path, profile_id: &str) -> PathBuf {
    app_cache_dir.join("remote-drives").join(profile_id)
}

#[cfg(windows)]
fn occupied_mount_targets() -> Vec<String> {
    (b'D'..=b'Z')
        .filter_map(|letter| {
            let target = format!("{}:", char::from(letter));
            Path::new(&format!("{target}\\"))
                .try_exists()
                .unwrap_or(true)
                .then_some(target)
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn occupied_mount_targets() -> Vec<String> {
    fs::read_dir("/Volumes")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn occupied_mount_targets() -> Vec<String> {
    Vec::new()
}

fn parse_remotes(output: &str) -> ServiceResult<Vec<String>> {
    let mut remotes = Vec::new();
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Some(name) = line.strip_suffix(':') else {
            return Err(ServiceError::new(
                ErrorCode::RemoteUnavailable,
                "rclone returned an invalid remote name.",
            ));
        };
        if name.is_empty() || name.contains([':', '/', '\\']) {
            return Err(ServiceError::new(
                ErrorCode::RemoteUnavailable,
                "rclone returned an invalid remote name.",
            ));
        }
        remotes.push(name.to_string());
    }
    remotes.sort();
    remotes.dedup();
    Ok(remotes)
}

fn development_sidecar_name() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("rclone-x86_64-pc-windows-msvc.exe")
    } else if cfg!(all(windows, target_arch = "aarch64")) {
        Some("rclone-aarch64-pc-windows-msvc.exe")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("rclone-x86_64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("rclone-aarch64-apple-darwin")
    } else {
        None
    }
}

fn find_rclone(resources: &ResourcePaths) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("EXPLORIE_RCLONE_PATH").map(PathBuf::from)
        && rclone_version(&path).is_ok()
    {
        return Some(path);
    }
    let binary_name = if cfg!(windows) {
        "rclone.exe"
    } else {
        "rclone"
    };
    let mut candidates = Vec::new();
    let executable_resource_dir = resources
        .current_exe
        .as_ref()
        .and_then(|path| path.parent())
        .map(|path| path.join("resources"));
    if let Some(current_exe) = resources.current_exe.as_ref()
        && let Some(directory) = current_exe.parent()
    {
        candidates.push(directory.join(binary_name));
        if let Some(sidecar_name) = development_sidecar_name() {
            candidates.push(directory.join(sidecar_name));
        }
    }
    if let Some(resource_dir) = resources.resource_dir.as_ref() {
        candidates.push(resource_dir.join(binary_name));
        candidates.push(resource_dir.join("binaries").join(binary_name));
    }
    if let Some(resource_dir) = executable_resource_dir {
        candidates.push(resource_dir.join(binary_name));
        candidates.push(resource_dir.join("binaries").join(binary_name));
    }
    #[cfg(target_os = "macos")]
    if let Some(resource_dir) = resources
        .current_exe
        .as_ref()
        .and_then(|path| path.parent())
        .and_then(|path| path.parent())
        .map(|path| path.join("Resources"))
    {
        candidates.push(resource_dir.join(binary_name));
        candidates.push(resource_dir.join("binaries").join(binary_name));
    }
    if let Some(sidecar_name) = development_sidecar_name() {
        candidates.push(resources.manifest_dir.join("binaries").join(sidecar_name));
    }
    candidates.push(PathBuf::from(binary_name));
    #[cfg(windows)]
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        candidates.push(
            PathBuf::from(program_files)
                .join("rclone")
                .join("rclone.exe"),
        );
    }
    #[cfg(target_os = "macos")]
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin/rclone"),
        PathBuf::from("/usr/local/bin/rclone"),
        PathBuf::from("/usr/bin/rclone"),
    ]);
    candidates
        .into_iter()
        .find(|candidate| rclone_version(candidate).is_ok())
}

fn rclone_version(rclone: &Path) -> ServiceResult<String> {
    let output = run_rclone_command(
        rclone_command(rclone).arg("version"),
        RCLONE_PROBE_TIMEOUT,
        "rclone version check",
    )?;
    if !output.status.success() {
        return Err(ServiceError::new(
            ErrorCode::RemoteUnavailable,
            "rclone version check failed",
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(str::to_string)
        .ok_or_else(|| {
            ServiceError::new(
                ErrorCode::RemoteUnavailable,
                "rclone version check returned no output",
            )
        })
}

fn ensure_rclone_capabilities(rclone: &Path) -> ServiceResult<()> {
    let args: &[&str] = if cfg!(target_os = "macos") {
        &["serve", "nfs", "--help"]
    } else {
        &["mount", "--help"]
    };
    let output = run_with_timeout(
        rclone_command(rclone).args(args),
        RCLONE_PROBE_TIMEOUT,
        0,
        0,
    )
    .map_err(|error| remote_process_error("rclone capability check", error))?;
    output.status.success().then_some(()).ok_or_else(|| {
        ServiceError::new(
            ErrorCode::RemoteUnavailable,
            "rclone 1.65 or newer with mount support is required.",
        )
    })
}

fn list_remotes_with_command(rclone: &Path) -> ServiceResult<Vec<String>> {
    let output = run_rclone_command(
        rclone_command(rclone).args(["listremotes", "--ask-password=false"]),
        RCLONE_LIST_TIMEOUT,
        "list rclone remotes",
    )?;
    if !output.status.success() {
        return Err(remote_command_error(
            "rclone could not read its configuration",
            &output.stderr,
            None,
        ));
    }
    parse_remotes(&String::from_utf8_lossy(&output.stdout))
}

fn configure_rclone_with_system(rclone: &Path, _resources: &ResourcePaths) -> ServiceResult<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let status = Command::new("cmd.exe")
            .args(["/D", "/S", "/C"])
            .raw_arg(r#"start "" /WAIT "%EXPLORIE_RCLONE%" config"#)
            .env("EXPLORIE_RCLONE", normal_windows_path(rclone))
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|error| remote_io("open rclone configuration", error))?;
        status.success().then_some(()).ok_or_else(|| {
            ServiceError::new(
                ErrorCode::RemoteUnavailable,
                "rclone configuration was cancelled or failed.",
            )
        })
    }

    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt;
        let directory = _resources.cache_dir.join("remote-drives");
        fs::create_dir_all(&directory).map_err(ServiceError::from)?;
        let session = Uuid::new_v4().simple().to_string();
        let script = directory.join(format!("configure-{session}.command"));
        let finished = directory.join(format!("configure-{session}.finished"));
        fs::write(
            &script,
            format!(
                "#!/bin/zsh\n{} config\nstatus=$?\ntouch {}\nexit $status\n",
                shell_quote(&rclone.to_string_lossy()),
                shell_quote(&finished.to_string_lossy())
            ),
        )
        .map_err(ServiceError::from)?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700))
            .map_err(ServiceError::from)?;
        let opened = Command::new("open")
            .arg(&script)
            .status()
            .map_err(|error| remote_io("open Terminal", error))?;
        if !opened.success() {
            let _ = fs::remove_file(&script);
            return Err(ServiceError::new(
                ErrorCode::RemoteUnavailable,
                "Failed to open rclone configuration in Terminal.",
            ));
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(30 * 60) {
            if finished.is_file() {
                let _ = fs::remove_file(&finished);
                let _ = fs::remove_file(&script);
                return Ok(());
            }
            thread::sleep(Duration::from_millis(250));
        }
        let _ = fs::remove_file(&script);
        Err(ServiceError::new(
            ErrorCode::RemoteUnavailable,
            "Timed out waiting for rclone configuration to finish.",
        ))
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (rclone, _resources);
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Remote configuration currently supports Windows and macOS only.",
        ))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn start_mount_with_system(
    _request: &RemoteMountRequest,
) -> ServiceResult<Box<dyn RemoteDriveProcess>> {
    Err(ServiceError::new(
        ErrorCode::Unsupported,
        "Remote Drives currently support Windows and macOS only.",
    ))
}

#[cfg(any(windows, target_os = "macos"))]
fn start_mount_with_system(
    request: &RemoteMountRequest,
) -> ServiceResult<Box<dyn RemoteDriveProcess>> {
    let mut command = rclone_command(&request.rclone);
    let remote = remote_spec(&request.profile);
    let rc_addr = rc_bind_address(&request.rc_url)?;

    #[cfg(windows)]
    command.args([
        "mount",
        &remote,
        &request.profile.mount_target,
        "--volname",
        &request.profile.name,
        "--vfs-cache-mode",
        "writes",
    ]);

    #[cfg(target_os = "macos")]
    {
        let port = request.helper_port.ok_or_else(|| {
            ServiceError::new(
                ErrorCode::Internal,
                "Missing macOS remote-drive helper port.",
            )
        })?;
        command.args(nfs_serve_arguments(&remote, port));
    }

    let log = File::create(request.cache_dir.join("rclone.log")).map_err(ServiceError::from)?;
    let log_err = log.try_clone().map_err(ServiceError::from)?;
    command
        .args(["--cache-dir"])
        .arg(&request.cache_dir)
        .args(["--rc", "--rc-addr"])
        .arg(rc_addr)
        .env("RCLONE_RC_USER", &request.rc_user)
        .env("RCLONE_RC_PASS", &request.rc_pass)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    command
        .spawn()
        .map(|child| Box::new(SystemRemoteDriveProcess(child)) as Box<dyn RemoteDriveProcess>)
        .map_err(|error| {
            ServiceError::new(
                ErrorCode::RemoteUnavailable,
                format!("Failed to start rclone: {error}"),
            )
            .retryable(true)
        })
}

/// `rclone serve nfs` has no authentication, so it must only ever listen on
/// IPv4 loopback, on a fresh random port for the lifetime of one mount. The
/// privileged macOS helper additionally checks that the listener is this
/// user's signed rclone before mounting it.
#[cfg(any(target_os = "macos", test))]
fn nfs_serve_arguments(remote: &str, port: u16) -> Vec<String> {
    [
        "serve",
        "nfs",
        remote,
        "--addr",
        &std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)).to_string(),
        "--vfs-cache-mode",
        "full",
    ]
    .map(String::from)
    .to_vec()
}

#[cfg(any(target_os = "macos", test))]
fn wait_for_loopback_listener(port: u16, timeout: Duration) -> ServiceResult<()> {
    let address = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port));
    let started = Instant::now();
    while started.elapsed() < timeout {
        if std::net::TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(150));
    }
    Err(ServiceError::new(
        ErrorCode::RemoteUnavailable,
        "Timed out waiting for rclone's local NFS server to start.",
    )
    .retryable(true))
}

#[cfg(any(windows, target_os = "macos", test))]
fn rc_bind_address(rc_url: &str) -> ServiceResult<&str> {
    rc_url
        .strip_prefix("http://")
        .and_then(|address| address.strip_suffix('/'))
        .filter(|address| {
            !address.is_empty() && address.contains(':') && !address.contains(['/', '\\'])
        })
        .ok_or_else(|| {
            ServiceError::new(
                ErrorCode::Internal,
                "Invalid local rclone remote-control address.",
            )
        })
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

fn rclone_command(rclone: &Path) -> Command {
    let command = Command::new(rclone);
    #[cfg(windows)]
    let mut command = command;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn run_rclone_command(
    command: &mut Command,
    timeout: Duration,
    action: &str,
) -> ServiceResult<ProcessOutput> {
    let output = run_with_timeout(command, timeout, MAX_RCLONE_OUTPUT, MAX_RCLONE_OUTPUT)
        .map_err(|error| remote_process_error(action, error))?;
    if output.stdout_truncated || output.stderr_truncated {
        return Err(ServiceError::new(
            ErrorCode::RemoteUnavailable,
            format!("{action} returned more output than Explorie can safely process"),
        ));
    }
    Ok(output)
}

fn remote_process_error(action: &str, error: ProcessError) -> ServiceError {
    match error {
        ProcessError::Io(error) => remote_io(action, error),
        ProcessError::TimedOut => {
            ServiceError::new(ErrorCode::RemoteUnavailable, format!("{action} timed out"))
                .retryable(true)
        }
    }
}

#[cfg(windows)]
fn winfsp_available() -> Option<bool> {
    use std::os::windows::process::CommandExt;
    Some(
        Command::new("sc.exe")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["query", "WinFsp.Launcher"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success()),
    )
}

#[cfg(not(windows))]
fn winfsp_available() -> Option<bool> {
    None
}

fn install_winfsp_blocking(context: &ServiceContext) -> ServiceResult<()> {
    #[cfg(windows)]
    {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        use std::os::windows::process::CommandExt;
        if winfsp_available() == Some(true) {
            return Ok(());
        }
        let mut candidates = Vec::new();
        if let Some(resources) = context.resources().resource_dir.as_ref() {
            candidates.push(resources.join("installers").join("winfsp-2.1.25156.msi"));
        }
        candidates.push(
            context
                .resources()
                .manifest_dir
                .join("resources")
                .join("winfsp-2.1.25156.msi"),
        );
        let bundled = candidates
            .into_iter()
            .find(|path| path.is_file())
            .ok_or_else(|| {
                ServiceError::new(
                    ErrorCode::HelperMissing,
                    "The bundled WinFsp installer is unavailable. Reinstall Explorie.",
                )
            })?;
        let cache_dir = context.resources().cache_dir.join("installers");
        fs::create_dir_all(&cache_dir).map_err(ServiceError::from)?;
        let installer = cache_dir.join("winfsp-2.1.25156.msi");
        const EXPECTED_SHA256: &str =
            "073a70e00f77423e34bed98b86e600def93393ba5822204fac57a29324db9f7a";
        let digest = |path: &Path| -> ServiceResult<String> {
            let mut file = File::open(path).map_err(ServiceError::from)?;
            let mut hasher = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = file.read(&mut buffer).map_err(ServiceError::from)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            Ok(format!("{:x}", hasher.finalize()))
        };
        if digest(&bundled)? != EXPECTED_SHA256 {
            return Err(ServiceError::new(
                ErrorCode::HelperMissing,
                "The bundled WinFsp installer failed its integrity check. Reinstall Explorie.",
            ));
        }
        if installer.is_file() && digest(&installer)? == EXPECTED_SHA256 {
            // The staged installer is already verified.
        } else {
            let staged = installer.with_extension("msi.tmp");
            fs::copy(&bundled, &staged).map_err(ServiceError::from)?;
            if digest(&staged)? != EXPECTED_SHA256 {
                let _ = fs::remove_file(&staged);
                return Err(ServiceError::new(
                    ErrorCode::HelperMissing,
                    "The staged WinFsp installer failed its integrity check.",
                ));
            }
            let _ = fs::remove_file(&installer);
            fs::rename(staged, &installer).map_err(ServiceError::from)?;
        }
        let log = cache_dir.join("winfsp-install.log");
        let installer = normal_windows_path(&installer);
        let log = normal_windows_path(&log);
        let status = Command::new("msiexec.exe")
            .creation_flags(CREATE_NO_WINDOW)
            .arg("/i")
            .arg(&installer)
            .args(["/norestart", "/L*V"])
            .arg(&log)
            .status()
            .map_err(ServiceError::from)?;
        if !status.success() {
            return Err(ServiceError::new(
                ErrorCode::HelperMissing,
                format!(
                    "WinFsp installation was cancelled or failed. Details: {}",
                    log.display()
                ),
            ));
        }
        if winfsp_available() != Some(true) {
            return Err(ServiceError::new(
                ErrorCode::HelperMissing,
                "WinFsp was installed but its launcher service is unavailable. Restart Windows and try again.",
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = context;
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "WinFsp is only used on Windows.",
        ))
    }
}

fn free_port() -> ServiceResult<u16> {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(ServiceError::from)
}

fn wait_for_rc(
    child: &mut dyn RemoteDriveProcess,
    backend: &dyn RemoteDriveBackend,
    rclone: &Path,
    url: &str,
    user: &str,
    pass: &str,
) -> ServiceResult<()> {
    let started = Instant::now();
    while started.elapsed() < READY_TIMEOUT {
        if let Some(status) = child.try_wait()? {
            return Err(ServiceError::new(
                ErrorCode::RemoteUnavailable,
                format!("rclone exited before the mount was ready: {status}"),
            )
            .retryable(true));
        }
        if rc_call(backend, rclone, url, user, pass, "rc/noopauth").is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(150));
    }
    Err(ServiceError::new(
        ErrorCode::RemoteUnavailable,
        "Timed out waiting for rclone to start.",
    )
    .retryable(true))
}

fn rc_call(
    backend: &dyn RemoteDriveBackend,
    rclone: &Path,
    url: &str,
    user: &str,
    pass: &str,
    endpoint: &str,
) -> ServiceResult<Value> {
    backend.remote_control(&RemoteControlRequest {
        rclone: rclone.to_path_buf(),
        rc_url: url.to_string(),
        rc_user: user.to_string(),
        rc_pass: pass.to_string(),
        endpoint: endpoint.to_string(),
    })
}

fn remote_control_with_system(request: &RemoteControlRequest) -> ServiceResult<Value> {
    let output = run_rclone_command(
        rclone_command(&request.rclone)
            .args(["rc", "--url"])
            .arg(&request.rc_url)
            .arg(&request.endpoint)
            .env("RCLONE_RC_USER", &request.rc_user)
            .env("RCLONE_RC_PASS", &request.rc_pass),
        RC_TIMEOUT,
        "rclone remote-control call",
    )?;
    if !output.status.success() {
        return Err(remote_command_error(
            "rclone remote-control call failed",
            &output.stderr,
            Some(&request.rc_pass),
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        ServiceError::new(ErrorCode::RemoteUnavailable, error.to_string()).retryable(true)
    })
}

fn disconnect_stats(
    force: bool,
    get_stats: impl FnOnce() -> ServiceResult<(u64, u64)>,
) -> ServiceResult<(u64, u64)> {
    if force { Ok((0, 0)) } else { get_stats() }
}

fn vfs_stats(backend: &dyn RemoteDriveBackend, mount: &RunningMount) -> ServiceResult<(u64, u64)> {
    let stats = rc_call(
        backend,
        &mount.rclone,
        &mount.rc_url,
        &mount.rc_user,
        &mount.rc_pass,
        "vfs/stats",
    )?;
    let cache = &stats["diskCache"];
    let pending = cache["uploadsQueued"].as_u64().unwrap_or(0)
        + cache["uploadsInProgress"].as_u64().unwrap_or(0);
    Ok((pending, cache["erroredFiles"].as_u64().unwrap_or(0)))
}

fn wait_or_kill(child: &mut dyn RemoteDriveProcess) -> ServiceResult<()> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    let kill_error = child.kill().err();
    let wait_result = child.wait().map(|_| ());
    match kill_error {
        Some(error) => Err(error),
        None => wait_result,
    }
}

fn status(
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

fn connected_status(id: &str, mount_path: &Path) -> RemoteDriveStatus {
    status(
        id,
        RemoteDriveState::Connected,
        Some(mount_path.to_path_buf()),
        None,
    )
}

fn remote_io(operation: &str, error: std::io::Error) -> ServiceError {
    ServiceError::new(
        ErrorCode::RemoteUnavailable,
        format!("Failed to {operation}: {error}"),
    )
    .retryable(true)
}

fn remote_command_error(prefix: &str, stderr: &[u8], secret: Option<&str>) -> ServiceError {
    let mut detail = String::from_utf8_lossy(stderr).trim().to_string();
    if let Some(secret) = secret.filter(|secret| !secret.is_empty()) {
        detail = detail.replace(secret, "[redacted]");
    }
    ServiceError::new(
        ErrorCode::RemoteUnavailable,
        if detail.is_empty() {
            prefix.to_string()
        } else {
            format!("{prefix}: {detail}")
        },
    )
    .retryable(true)
}

fn missing_rclone() -> String {
    "The bundled rclone executable is unavailable. Reinstall Explorie or run prepare:rclone in development.".to_string()
}

#[allow(dead_code)]
fn macos_volume_name(path: &Path) -> ServiceResult<String> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            ServiceError::new(
                ErrorCode::InvalidInput,
                "macOS mount path does not contain a valid volume name.",
            )
        })?;
    if name.trim().is_empty()
        || name.len() > 64
        || matches!(name, "." | "..")
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "macOS mount path contains an invalid volume name.",
        ));
    }
    Ok(name.to_string())
}

#[cfg(windows)]
fn normal_windows_path(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    if let Some(path) = value.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{path}"))
    } else if let Some(path) = value.strip_prefix(r"\\?\") {
        PathBuf::from(path)
    } else {
        path.to_path_buf()
    }
}

#[cfg(target_os = "macos")]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, CString, c_char};

    unsafe extern "C" {
        fn explorie_mount_helper_status() -> i32;
        fn explorie_mount_helper_register() -> i32;
        fn explorie_mount_helper_unregister() -> i32;
        fn explorie_mount_helper_open_settings();
        fn explorie_mount_helper_mount(
            id: *const c_char,
            name: *const c_char,
            port: u16,
        ) -> *mut c_char;
        fn explorie_mount_helper_unmount(
            id: *const c_char,
            name: *const c_char,
            force: bool,
        ) -> *mut c_char;
        fn explorie_mount_helper_free(value: *mut c_char);
    }

    pub fn status() -> String {
        match unsafe { explorie_mount_helper_status() } {
            0 => "not-registered",
            1 => "enabled",
            2 => "approval-required",
            _ => "unavailable",
        }
        .to_string()
    }

    pub fn register() -> Result<String, String> {
        match unsafe { explorie_mount_helper_register() } {
            1 => Ok("enabled".to_string()),
            2 => Ok("approval-required".to_string()),
            _ => Err("Unable to register the Remote Drives helper.".to_string()),
        }
    }

    pub fn unregister() -> Result<(), String> {
        (unsafe { explorie_mount_helper_unregister() } == 0)
            .then_some(())
            .ok_or_else(|| "Unable to remove the Remote Drives helper.".to_string())
    }

    pub fn open_settings() {
        unsafe { explorie_mount_helper_open_settings() };
    }

    pub fn mount(id: &str, name: &str, port: u16) -> Result<(), String> {
        let id = CString::new(id).map_err(|_| "Invalid helper profile ID")?;
        let name = CString::new(name).map_err(|_| "Invalid helper volume name")?;
        call(unsafe { explorie_mount_helper_mount(id.as_ptr(), name.as_ptr(), port) })
    }

    pub fn unmount(id: &str, name: &str, force: bool) -> Result<(), String> {
        let id = CString::new(id).map_err(|_| "Invalid helper profile ID")?;
        let name = CString::new(name).map_err(|_| "Invalid helper volume name")?;
        call(unsafe { explorie_mount_helper_unmount(id.as_ptr(), name.as_ptr(), force) })
    }

    fn call(value: *mut c_char) -> Result<(), String> {
        if value.is_null() {
            // The helper uses a nil reply to signal success; only a returned
            // string represents an error.
            return Ok(());
        }
        let message = unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned();
        unsafe { explorie_mount_helper_free(value) };
        if message.is_empty() {
            Ok(())
        } else {
            Err(message)
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_helper_status() -> Option<String> {
    Some(macos::status())
}
#[cfg(not(target_os = "macos"))]
fn macos_helper_status() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfs_server_listens_only_on_ipv4_loopback() {
        assert_eq!(
            nfs_serve_arguments("cloud:folder", 49_152),
            [
                "serve",
                "nfs",
                "cloud:folder",
                "--addr",
                "127.0.0.1:49152",
                "--vfs-cache-mode",
                "full"
            ]
        );
    }

    #[test]
    fn nfs_mounts_wait_for_the_loopback_listener() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        wait_for_loopback_listener(port, Duration::from_secs(5)).unwrap();
        drop(listener);
        let started = Instant::now();
        let error = wait_for_loopback_listener(port, Duration::from_millis(400)).unwrap_err();
        assert_eq!(error.code, ErrorCode::RemoteUnavailable);
        assert!(started.elapsed() >= Duration::from_millis(400));
    }

    #[test]
    fn parses_and_sorts_remote_names() {
        assert_eq!(
            parse_remotes("zeta:\nalpha:\nalpha:\n").unwrap(),
            vec!["alpha", "zeta"]
        );
        assert!(parse_remotes("not-a-remote\n").is_err());
    }

    #[test]
    fn remote_spec_never_treats_subpath_as_a_local_path() {
        let profile = RemoteDriveProfile {
            id: Uuid::new_v4().to_string(),
            name: "Drive".to_string(),
            remote: "cloud".to_string(),
            remote_path: "folder/subfolder".to_string(),
            mount_target: "R:".to_string(),
        };
        assert_eq!(remote_spec(&profile), "cloud:folder/subfolder");
    }

    #[test]
    fn rclone_bind_address_strips_the_http_client_url_shape() {
        assert_eq!(
            rc_bind_address("http://127.0.0.1:49152/").unwrap(),
            "127.0.0.1:49152"
        );
        assert!(rc_bind_address("127.0.0.1:49152").is_err());
        assert!(rc_bind_address("http://127.0.0.1:49152/path").is_err());
    }

    #[test]
    fn rejects_path_traversal_and_unconfigured_remote_shapes() {
        let mut profile = RemoteDriveProfile {
            id: Uuid::new_v4().to_string(),
            name: "Drive".to_string(),
            remote: "cloud".to_string(),
            remote_path: "../secret".to_string(),
            mount_target: "R:".to_string(),
        };
        assert!(validate_remote_drive_profile(&profile).is_err());
        profile.remote_path.clear();
        profile.remote = "cloud:path".to_string();
        assert!(validate_remote_drive_profile(&profile).is_err());
        profile.remote = "cloud".to_string();
        profile.remote_path = "/absolute".to_string();
        assert!(validate_remote_drive_profile(&profile).is_err());
    }

    #[test]
    fn cache_paths_are_stable_per_profile() {
        let base = Path::new("cache-root");
        assert_eq!(
            profile_cache_dir(base, "672ce77a-b72d-4e16-a9e8-55e0ac5bc580"),
            base.join("remote-drives")
                .join("672ce77a-b72d-4e16-a9e8-55e0ac5bc580")
        );
    }

    #[test]
    fn existing_mount_targets_are_rejected() {
        let target = std::env::temp_dir().join(format!("explorie-mount-test-{}", Uuid::new_v4()));
        fs::create_dir(&target).unwrap();
        assert!(ensure_mount_target_available(&target).is_err());
        fs::remove_dir(&target).unwrap();
        assert!(ensure_mount_target_available(&target).is_ok());
    }

    #[test]
    fn forced_disconnect_skips_unavailable_remote_stats() {
        assert_eq!(
            disconnect_stats(true, || Err(ServiceError::new(ErrorCode::Io, "offline"))),
            Ok((0, 0))
        );
        assert!(
            disconnect_stats(false, || Err(ServiceError::new(ErrorCode::Io, "offline"))).is_err()
        );
    }

    #[test]
    fn remote_command_details_redact_the_control_password() {
        let error = remote_command_error(
            "rclone remote-control call failed",
            b"authorization failed: do-not-log",
            Some("do-not-log"),
        );
        assert!(!error.message.contains("do-not-log"));
        assert!(error.message.contains("[redacted]"));
    }

    #[test]
    fn macos_disconnect_passes_only_the_volume_basename_to_the_helper() {
        assert_eq!(
            macos_volume_name(Path::new("/Volumes/Remote Drive")).unwrap(),
            "Remote Drive"
        );
        assert!(macos_volume_name(Path::new("/Volumes/..")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_drop_verbatim_prefixes() {
        assert_eq!(
            normal_windows_path(Path::new(r"\\?\C:\Program Files\Explorie\winfsp.msi")),
            PathBuf::from(r"C:\Program Files\Explorie\winfsp.msi")
        );
        assert_eq!(
            normal_windows_path(Path::new(r"\\?\UNC\server\share\winfsp.msi")),
            PathBuf::from(r"\\server\share\winfsp.msi")
        );
    }
}
