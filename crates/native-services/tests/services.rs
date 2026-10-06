use explorie_native_services::archive::ArchiveFormat;
use explorie_native_services::listing::ListRequest;
use explorie_native_services::remote_drives::RemoteDriveProfile;
#[cfg(any(windows, target_os = "macos"))]
use explorie_native_services::remote_drives::{
    RemoteControlRequest, RemoteDriveBackend, RemoteDriveProcess, RemoteMountRequest,
    RemoteProcessStatus,
};
use explorie_native_services::{
    CompressionLevel, FileOperationEvent, FileOperationKind, FileOperationRequest,
    FileOperationState, NativeServices, ResourcePaths, ServiceEvent, WatcherState,
};
#[cfg(any(windows, target_os = "macos"))]
use explorie_native_services::{ServiceContext, ServiceError, ServiceResult};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
#[cfg(any(windows, target_os = "macos"))]
use std::path::{Path, PathBuf};
#[cfg(any(windows, target_os = "macos"))]
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn services(root: &std::path::Path) -> NativeServices {
    NativeServices::new(ResourcePaths::test(root))
}

#[test]
fn listing_and_metadata_are_available_without_a_host_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("document.txt"), "hello").unwrap();
    let native = services(root);

    let entries = native
        .listing
        .list(ListRequest {
            path: root.to_path_buf(),
            calc_dir_size: false,
        })
        .wait()
        .unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry.path.ends_with("document.txt"))
    );

    let mut custom = HashMap::new();
    custom.insert("status".to_string(), json!("Done"));
    native
        .metadata
        .update_fields(root.to_path_buf(), "document.txt".into(), custom)
        .wait()
        .unwrap();
    let entries = native
        .listing
        .list(ListRequest {
            path: root.to_path_buf(),
            calc_dir_size: false,
        })
        .wait()
        .unwrap();
    assert_eq!(entries[0].custom.get("status"), Some(&json!("Done")));
}

#[test]
fn file_jobs_publish_typed_progress_and_result_events() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("destination");
    fs::write(&source, "copy me").unwrap();
    fs::create_dir(&destination).unwrap();
    let native = services(temp.path());
    let receiver = native.subscribe();
    let job_id = native
        .mutations
        .start_file_operation(FileOperationRequest {
            kind: FileOperationKind::Copy,
            sources: vec![source],
            destination: Some(destination.clone()),
            conflict_policy: Default::default(),
        })
        .unwrap();

    let mut saw_progress = false;
    let mut completed = false;
    for _ in 0..20 {
        match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
            ServiceEvent::FileOperation(FileOperationEvent {
                job_id: event_job,
                state: FileOperationState::Running,
                progress: Some(_),
                ..
            }) if event_job == job_id => saw_progress = true,
            ServiceEvent::FileOperation(FileOperationEvent {
                job_id: event_job,
                state: FileOperationState::Completed,
                result: Some(result),
                ..
            }) if event_job == job_id => {
                assert_eq!(result.processed_entries, 1);
                completed = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_progress);
    assert!(completed);
    assert_eq!(
        fs::read_to_string(destination.join("source.txt")).unwrap(),
        "copy me"
    );
}

#[test]
fn failed_batch_jobs_publish_only_the_unresolved_sources_for_retry() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first.txt");
    let second = temp.path().join("second.txt");
    let destination = temp.path().join("destination");
    fs::write(&first, "first").unwrap();
    fs::write(&second, "second").unwrap();
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("second.txt"), "existing").unwrap();
    let native = services(temp.path());
    let receiver = native.subscribe();
    let request = FileOperationRequest {
        kind: FileOperationKind::Copy,
        sources: vec![first.clone(), second.clone()],
        destination: Some(destination.clone()),
        conflict_policy: Default::default(),
    };
    let job_id = native.mutations.start_file_operation(request).unwrap();

    let retryable = loop {
        if let ServiceEvent::FileOperation(FileOperationEvent {
            job_id: event_job,
            state: FileOperationState::Failed,
            result: Some(result),
            retryable_sources,
            ..
        }) = receiver.recv_timeout(Duration::from_secs(1)).unwrap()
            && event_job == job_id
        {
            assert_eq!(result.targets, vec![destination.join("first.txt")]);
            break retryable_sources;
        }
    };
    assert_eq!(retryable, vec![second.clone()]);
    assert_eq!(
        fs::read_to_string(destination.join("first.txt")).unwrap(),
        "first"
    );
    assert_eq!(
        fs::read_to_string(destination.join("second.txt")).unwrap(),
        "existing"
    );

    fs::remove_file(destination.join("second.txt")).unwrap();
    let retry_job = native
        .mutations
        .start_file_operation(FileOperationRequest {
            kind: FileOperationKind::Copy,
            sources: retryable,
            destination: Some(destination.clone()),
            conflict_policy: Default::default(),
        })
        .unwrap();
    loop {
        if let ServiceEvent::FileOperation(FileOperationEvent {
            job_id: event_job,
            state: FileOperationState::Completed,
            ..
        }) = receiver.recv_timeout(Duration::from_secs(1)).unwrap()
            && event_job == retry_job
        {
            break;
        }
    }
    assert_eq!(
        fs::read_to_string(destination.join("second.txt")).unwrap(),
        "second"
    );
}

#[test]
fn file_jobs_can_be_cancelled_through_the_service_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let destination = temp.path().join("destination");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&destination).unwrap();
    for index in 0..4096 {
        fs::write(source.join(format!("entry-{index}.txt")), b"entry").unwrap();
    }
    let native = services(temp.path());
    let receiver = native.subscribe();
    let job_id = native
        .mutations
        .start_file_operation(FileOperationRequest {
            kind: FileOperationKind::Copy,
            sources: vec![source.clone()],
            destination: Some(destination.clone()),
            conflict_policy: Default::default(),
        })
        .unwrap();

    assert!(native.mutations.cancel_file_operation(&job_id));
    let mut cancelled = false;
    for _ in 0..20 {
        match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
            ServiceEvent::FileOperation(FileOperationEvent {
                job_id: event_job,
                state: FileOperationState::Cancelled,
                result: Some(result),
                retryable_sources,
                ..
            }) if event_job == job_id => {
                assert!(result.targets.is_empty());
                assert_eq!(retryable_sources, vec![source.clone()]);
                cancelled = true;
                break;
            }
            _ => {}
        }
    }
    assert!(cancelled);
    assert!(!destination.join("source").exists());
}

#[test]
fn archive_preview_and_platform_services_use_background_tasks() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.txt");
    let archive = temp.path().join("bundle.zip");
    let extracted = temp.path().join("extracted");
    fs::write(&source, "archive me").unwrap();
    let native = services(temp.path());
    let receiver = native.subscribe();

    let result = native
        .archives
        .compress(explorie_native_services::CompressRequest {
            paths: vec![source.clone()],
            output_path: archive.clone(),
            format: ArchiveFormat::Zip,
            compression_level: CompressionLevel::Normal,
            password: None,
            operation_id: "archive-test".into(),
        })
        .wait()
        .unwrap();
    assert_eq!(result.output_path, archive);
    let listed = native
        .archives
        .list(result.output_path.clone())
        .wait()
        .unwrap();
    assert_eq!(listed.entry_count, 1);
    let extract = |operation_id: &str| {
        native
            .archives
            .extract(explorie_native_services::ExtractRequest {
                archive_path: result.output_path.clone(),
                output_dir: extracted.clone(),
                password: None,
                allow_extended_limits: false,
                operation_id: operation_id.into(),
            })
            .wait()
    };
    extract("extract-test").unwrap();
    assert_eq!(
        fs::read_to_string(extracted.join("source.txt")).unwrap(),
        "archive me"
    );
    // Extracting again never merges into (and overwrites) the existing folder.
    fs::write(extracted.join("source.txt"), "edited").unwrap();
    let error = extract("extract-again").unwrap_err();
    assert_eq!(error.code, explorie_native_services::ErrorCode::Conflict);
    assert_eq!(
        fs::read_to_string(extracted.join("source.txt")).unwrap(),
        "edited"
    );

    let progress = receiver
        .try_iter()
        .find_map(|event| match event {
            ServiceEvent::ArchiveProgress(progress) => Some(progress),
            _ => None,
        })
        .expect("archive progress event");
    assert_eq!(progress.operation_id, "archive-test");

    let text = native.previews.read_text(source, 5).wait().unwrap();
    assert_eq!(text.text, "archi");
    assert!(text.truncated);
    let helpers = native.previews.helper_status().wait().unwrap();
    assert_eq!(helpers.len(), 3);
    assert!(receiver.try_iter().any(|event| {
        matches!(event, ServiceEvent::HelperStatus(status) if status.helper == "FFmpeg")
    }));
    let colors = native.integration.finder_tag_colors().wait().unwrap();
    assert_eq!(colors.get("Orange"), Some(&7));
}

#[test]
fn remote_errors_are_typed_and_watcher_lifecycle_is_native() {
    let temp = tempfile::tempdir().unwrap();
    let native = services(temp.path());
    let invalid = native
        .remotes
        .connect(RemoteDriveProfile {
            id: "not-a-uuid".into(),
            name: "Remote".into(),
            remote: "remote".into(),
            remote_path: String::new(),
            mount_target: "D:".into(),
        })
        .wait()
        .unwrap_err();
    assert_eq!(
        invalid.code,
        explorie_native_services::ErrorCode::InvalidInput
    );
    assert_eq!(native.remotes.statuses().len(), 0);

    let receiver = native.subscribe();
    let first = native
        .watcher
        .watch(vec![temp.path().to_path_buf()])
        .unwrap();
    let second = native
        .watcher
        .watch(vec![temp.path().to_path_buf()])
        .unwrap();
    assert_ne!(first.id(), second.id());
    assert_eq!(native.watcher.registration_count(), 1);
    fs::write(temp.path().join("watch.txt"), "watch").unwrap();
    let event = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
    match event {
        ServiceEvent::Watcher(event) => {
            assert_eq!(event.state, WatcherState::Changed);
            assert!(event.paths.iter().any(|path| path.ends_with("watch.txt")));
        }
        other => panic!("unexpected event: {other:?}"),
    }
    drop(second);
    drop(first);
    assert_eq!(native.watcher.registration_count(), 0);
}

#[cfg(any(windows, target_os = "macos"))]
#[derive(Clone, Default)]
struct FakeRemoteBackend {
    state: Arc<Mutex<FakeRemoteState>>,
}

#[cfg(any(windows, target_os = "macos"))]
#[derive(Default)]
struct FakeRemoteState {
    started: usize,
    stopped: usize,
    pending_uploads: u64,
    last_volume_name: Option<String>,
    quit_requested: bool,
    fail_unmount: bool,
    fail_quit: bool,
    /// The helper status to report instead of "enabled" (macOS).
    helper: Option<String>,
    /// What registering the helper changes its status to; `None` fails.
    helper_after_register: Option<String>,
    helper_registrations: usize,
    helper_settings_opened: usize,
}

#[cfg(any(windows, target_os = "macos"))]
struct FakeRemoteProcess {
    state: Arc<Mutex<FakeRemoteState>>,
    alive: bool,
}

#[cfg(any(windows, target_os = "macos"))]
impl RemoteDriveProcess for FakeRemoteProcess {
    fn try_wait(&mut self) -> ServiceResult<Option<RemoteProcessStatus>> {
        if self.alive && self.state.lock().unwrap().quit_requested {
            self.alive = false;
            self.state.lock().unwrap().stopped += 1;
        }
        Ok((!self.alive).then_some(RemoteProcessStatus {
            success: true,
            code: Some(0),
        }))
    }

    fn wait(&mut self) -> ServiceResult<RemoteProcessStatus> {
        if self.alive {
            self.alive = false;
            self.state.lock().unwrap().stopped += 1;
        }
        Ok(RemoteProcessStatus {
            success: true,
            code: Some(0),
        })
    }

    fn kill(&mut self) -> ServiceResult<()> {
        self.wait().map(|_| ())
    }
}

#[cfg(any(windows, target_os = "macos"))]
impl RemoteDriveBackend for FakeRemoteBackend {
    fn find_rclone(&self, _resources: &ResourcePaths) -> Option<PathBuf> {
        Some(PathBuf::from("fake-rclone"))
    }

    fn rclone_version(&self, _rclone: &Path) -> ServiceResult<String> {
        Ok("fake-rclone 1.0".into())
    }

    fn list_remotes(&self, _rclone: &Path) -> ServiceResult<Vec<String>> {
        Ok(vec!["remote".into()])
    }

    fn ensure_capabilities(&self, _rclone: &Path) -> ServiceResult<()> {
        Ok(())
    }

    fn winfsp_available(&self) -> Option<bool> {
        #[cfg(windows)]
        return Some(true);
        #[cfg(not(windows))]
        None
    }

    fn occupied_mount_targets(&self) -> Vec<String> {
        Vec::new()
    }

    fn helper_status(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        return Some(
            self.state
                .lock()
                .unwrap()
                .helper
                .clone()
                .unwrap_or_else(|| "enabled".into()),
        );
        #[cfg(not(target_os = "macos"))]
        None
    }

    fn configure(&self, _rclone: &Path, _resources: &ResourcePaths) -> ServiceResult<()> {
        Ok(())
    }

    fn start_mount(
        &self,
        _request: &RemoteMountRequest,
    ) -> ServiceResult<Box<dyn RemoteDriveProcess>> {
        self.state.lock().unwrap().started += 1;
        Ok(Box::new(FakeRemoteProcess {
            state: Arc::clone(&self.state),
            alive: true,
        }))
    }

    fn remote_control(&self, request: &RemoteControlRequest) -> ServiceResult<serde_json::Value> {
        match request.endpoint.as_str() {
            "vfs/stats" => Ok(json!({
                "diskCache": {
                    "uploadsQueued": self.state.lock().unwrap().pending_uploads,
                    "uploadsInProgress": 0,
                    "erroredFiles": 0
                }
            })),
            "rc/noopauth" => Ok(json!({})),
            "core/quit" => {
                if self.state.lock().unwrap().fail_quit {
                    self.state.lock().unwrap().quit_requested = true;
                    return Err(ServiceError::new(
                        explorie_native_services::ErrorCode::RemoteUnavailable,
                        "fake quit failed",
                    ));
                }
                self.state.lock().unwrap().quit_requested = true;
                Ok(json!({}))
            }
            endpoint => Err(ServiceError::new(
                explorie_native_services::ErrorCode::Unsupported,
                format!("unexpected fake endpoint: {endpoint}"),
            )),
        }
    }

    fn mount_helper(&self, _id: &str, volume_name: &str, _port: u16) -> ServiceResult<()> {
        self.state.lock().unwrap().last_volume_name = Some(volume_name.into());
        Ok(())
    }

    fn unmount_helper(&self, _id: &str, volume_name: &str, _force: bool) -> ServiceResult<()> {
        let mut state = self.state.lock().unwrap();
        state.last_volume_name = Some(volume_name.into());
        if state.fail_unmount {
            return Err(ServiceError::new(
                explorie_native_services::ErrorCode::RemoteUnavailable,
                "fake unmount failed",
            ));
        }
        Ok(())
    }

    fn install_winfsp(&self, _context: &ServiceContext) -> ServiceResult<()> {
        Ok(())
    }

    fn register_helper(&self) -> ServiceResult<String> {
        let mut state = self.state.lock().unwrap();
        state.helper_registrations += 1;
        if state.helper.is_none() {
            return Ok("enabled".into());
        }
        match state.helper_after_register.clone() {
            Some(after) => {
                state.helper = Some(after.clone());
                Ok(after)
            }
            None => Err(ServiceError::new(
                explorie_native_services::ErrorCode::HelperMissing,
                "Unable to install the Remote Drives helper: Operation not permitted",
            )),
        }
    }

    fn unregister_helper(&self) -> ServiceResult<()> {
        Ok(())
    }

    fn open_helper_settings(&self) -> ServiceResult<()> {
        self.state.lock().unwrap().helper_settings_opened += 1;
        Ok(())
    }
}

#[cfg(windows)]
fn unused_mount_target() -> String {
    (b'D'..=b'Z')
        .map(|letter| format!("{}:", char::from(letter)))
        .find(|target| !Path::new(&format!("{target}\\")).exists())
        .unwrap_or_else(|| "Z:".into())
}

#[cfg(target_os = "macos")]
fn unused_mount_target() -> String {
    format!("Explorie Test {}", std::process::id())
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn injected_remote_backend_drives_connect_exit_blocker_and_disconnect() {
    let temp = tempfile::tempdir().unwrap();
    let backend = Arc::new(FakeRemoteBackend::default());
    let native = NativeServices::with_remote_backend(
        ResourcePaths::test(temp.path()),
        Arc::clone(&backend) as Arc<dyn RemoteDriveBackend>,
    );
    let receiver = native.subscribe();
    let id = uuid::Uuid::new_v4().to_string();
    let profile = RemoteDriveProfile {
        id: id.clone(),
        name: "Remote".into(),
        remote: "remote".into(),
        remote_path: String::new(),
        mount_target: unused_mount_target(),
    };
    let _mount_target = profile.mount_target.clone();

    let connected = native.remotes.connect(profile).wait().unwrap();
    assert_eq!(
        connected.state,
        explorie_native_services::RemoteDriveState::Connected
    );
    assert_eq!(backend.state.lock().unwrap().started, 1);
    assert!(
        native
            .remotes
            .is_mount_root(connected.mount_path.as_deref().unwrap())
    );

    backend.state.lock().unwrap().pending_uploads = 1;
    assert!(!native.remotes.disconnect_all_if_clean());
    assert_eq!(native.remotes.statuses().len(), 1);
    assert!(receiver.try_iter().any(|event| {
        matches!(event, ServiceEvent::RemoteDriveExitBlocked(blocker) if blocker.pending_uploads == 1)
    }));

    backend.state.lock().unwrap().pending_uploads = 0;
    assert!(native.remotes.disconnect_all_if_clean());
    assert!(native.remotes.statuses().is_empty());
    assert_eq!(backend.state.lock().unwrap().stopped, 1);
    assert!(
        !native
            .remotes
            .is_mount_root(connected.mount_path.as_deref().unwrap())
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        backend.state.lock().unwrap().last_volume_name.as_deref(),
        Some(_mount_target.as_str())
    );
}

#[cfg(target_os = "macos")]
fn helper_test_profile() -> RemoteDriveProfile {
    RemoteDriveProfile {
        id: uuid::Uuid::new_v4().to_string(),
        name: "Remote".into(),
        remote: "remote".into(),
        remote_path: String::new(),
        mount_target: unused_mount_target(),
    }
}

/// On a fresh install the helper is not registered yet. Connecting must
/// register it (that is what makes macOS list it under Login Items) and say
/// what to approve, instead of reporting "approval required" with nothing
/// for the user to approve.
#[cfg(target_os = "macos")]
#[test]
fn connecting_installs_a_missing_helper_and_says_what_to_approve() {
    use explorie_native_services::{HELPER_APPROVAL_MESSAGE, RemoteDriveState};
    let temp = tempfile::tempdir().unwrap();
    let backend = Arc::new(FakeRemoteBackend::default());
    {
        let mut state = backend.state.lock().unwrap();
        state.helper = Some("not-registered".into());
        state.helper_after_register = Some("approval-required".into());
    }
    let native = NativeServices::with_remote_backend(
        ResourcePaths::test(temp.path()),
        Arc::clone(&backend) as Arc<dyn RemoteDriveBackend>,
    );
    let profile = helper_test_profile();

    let waiting = native.remotes.connect(profile.clone()).wait().unwrap();
    assert_eq!(waiting.state, RemoteDriveState::ApprovalRequired);
    assert_eq!(
        waiting.error.as_ref().unwrap().message,
        HELPER_APPROVAL_MESSAGE
    );
    {
        let state = backend.state.lock().unwrap();
        assert_eq!(state.helper_registrations, 1);
        assert_eq!(state.helper_settings_opened, 1);
        assert_eq!(state.started, 0, "nothing mounts before approval");
    }

    // Once the user allows it, connecting goes through.
    backend.state.lock().unwrap().helper = Some("enabled".into());
    let connected = native.remotes.connect(profile).wait().unwrap();
    assert_eq!(connected.state, RemoteDriveState::Connected);
    assert_eq!(backend.state.lock().unwrap().helper_registrations, 1);
    native.remotes.disconnect_all().wait().unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn a_helper_that_cannot_be_installed_is_reported_as_missing() {
    use explorie_native_services::RemoteDriveState;
    let temp = tempfile::tempdir().unwrap();
    let backend = Arc::new(FakeRemoteBackend::default());
    backend.state.lock().unwrap().helper = Some("not-registered".into());
    let native = NativeServices::with_remote_backend(
        ResourcePaths::test(temp.path()),
        Arc::clone(&backend) as Arc<dyn RemoteDriveBackend>,
    );

    let missing = native
        .remotes
        .connect(helper_test_profile())
        .wait()
        .unwrap();
    assert_eq!(missing.state, RemoteDriveState::HelperMissing);
    assert!(
        missing
            .error
            .as_ref()
            .unwrap()
            .message
            .contains("Operation not permitted"),
        "{:?}",
        missing.error
    );
    let state = backend.state.lock().unwrap();
    assert_eq!(state.helper_settings_opened, 0);
    assert_eq!(state.started, 0);
}

#[cfg(target_os = "macos")]
#[test]
fn a_helper_enabled_by_registering_connects_right_away() {
    use explorie_native_services::RemoteDriveState;
    let temp = tempfile::tempdir().unwrap();
    let backend = Arc::new(FakeRemoteBackend::default());
    {
        let mut state = backend.state.lock().unwrap();
        state.helper = Some("not-registered".into());
        state.helper_after_register = Some("enabled".into());
    }
    let native = NativeServices::with_remote_backend(
        ResourcePaths::test(temp.path()),
        Arc::clone(&backend) as Arc<dyn RemoteDriveBackend>,
    );

    let connected = native
        .remotes
        .connect(helper_test_profile())
        .wait()
        .unwrap();
    assert_eq!(connected.state, RemoteDriveState::Connected);
    assert_eq!(backend.state.lock().unwrap().helper_settings_opened, 0);
    native.remotes.disconnect_all().wait().unwrap();
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn forced_disconnect_reports_cleanup_errors_and_keeps_root_protected() {
    let temp = tempfile::tempdir().unwrap();
    let backend = Arc::new(FakeRemoteBackend::default());
    let native = NativeServices::with_remote_backend(
        ResourcePaths::test(temp.path()),
        Arc::clone(&backend) as Arc<dyn RemoteDriveBackend>,
    );
    let profile = RemoteDriveProfile {
        id: uuid::Uuid::new_v4().to_string(),
        name: "Remote".into(),
        remote: "remote".into(),
        remote_path: String::new(),
        mount_target: unused_mount_target(),
    };
    let connected = native.remotes.connect(profile).wait().unwrap();
    let mount_path = connected.mount_path.clone().unwrap();

    backend.state.lock().unwrap().fail_quit = true;
    assert!(native.remotes.disconnect_all().wait().is_err());
    assert_eq!(native.remotes.statuses().len(), 1);
    assert!(native.remotes.is_mount_root(&mount_path));

    backend.state.lock().unwrap().fail_quit = false;
    native.remotes.disconnect_all().wait().unwrap();
    assert!(native.remotes.statuses().is_empty());
    assert!(!native.remotes.is_mount_root(&mount_path));
}

/// Case-only renames and batch renames on a real exFAT volume, which lacks
/// an exclusive rename and treats "notes.txt" and "Notes.txt" as one name.
/// Run with `cargo test --test services -- --ignored exfat`.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "creates and attaches an exFAT disk image with hdiutil"]
fn case_only_renames_work_on_exfat() {
    struct AttachedImage(PathBuf);
    impl Drop for AttachedImage {
        fn drop(&mut self) {
            let _ = std::process::Command::new("hdiutil")
                .args(["detach", "-force"])
                .arg(&self.0)
                .output();
        }
    }
    let hdiutil = |args: &[&std::ffi::OsStr]| {
        let output = std::process::Command::new("hdiutil")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "hdiutil {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let temp = tempfile::tempdir().unwrap();
    let image = temp.path().join("rename.dmg");
    let volume = temp.path().join("volume");
    fs::create_dir(&volume).unwrap();
    hdiutil(&[
        "create".as_ref(),
        "-size".as_ref(),
        "20m".as_ref(),
        "-fs".as_ref(),
        "ExFAT".as_ref(),
        "-volname".as_ref(),
        "EXPLORIE".as_ref(),
        image.as_os_str(),
    ]);
    hdiutil(&[
        "attach".as_ref(),
        "-nobrowse".as_ref(),
        "-mountpoint".as_ref(),
        volume.as_os_str(),
        image.as_os_str(),
    ]);
    let _attached = AttachedImage(volume.clone());
    let directory = volume.join("files");
    fs::create_dir_all(directory.join("folder")).unwrap();
    fs::write(directory.join("notes.txt"), "keep").unwrap();
    fs::write(directory.join("alpha.txt"), "alpha").unwrap();
    let native = services(temp.path());

    for (before, after) in [("notes.txt", "Notes.txt"), ("folder", "FOLDER")] {
        let renamed = native
            .mutations
            .rename_path(directory.join(before), after.into())
            .wait()
            .unwrap();
        assert_eq!(PathBuf::from(renamed), directory.join(after));
    }
    native
        .mutations
        .batch_rename(vec![explorie_native_services::BatchRenameItem {
            source_path: directory.join("alpha.txt"),
            new_base_name: "ALPHA.txt".to_string(),
        }])
        .wait()
        .unwrap();

    let mut names: Vec<String> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with("._"))
        .collect();
    names.sort();
    assert_eq!(names, ["ALPHA.txt", "FOLDER", "Notes.txt"]);
    assert_eq!(
        fs::read_to_string(directory.join("Notes.txt")).unwrap(),
        "keep"
    );
}
