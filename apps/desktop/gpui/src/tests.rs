use super::*;
use explorie_native_services::{
    AppInfo, AudioBackend, AudioPlayback, FinderTagsBackend, PlatformActionsBackend,
    RemoteControlRequest, RemoteDriveBackend, RemoteDriveProcess, RemoteMountRequest,
    RemoteProcessStatus, ResourcePaths, ServiceContext, VideoBackend, VideoPlayback,
};
use gpui::{AppContext, KeyBinding, Keystroke, TestAppContext};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[test]
fn desktop_windows_use_platform_appropriate_native_chrome() {
    let options = desktop_window_options(Bounds::default());
    let titlebar = options.titlebar.expect("desktop titlebar options");
    assert_eq!(titlebar.title.as_deref(), Some(APP_NAME));
    assert_eq!(
        titlebar.appears_transparent,
        cfg!(any(windows, target_os = "macos"))
    );
    assert_eq!(
        titlebar.traffic_light_position,
        if cfg!(target_os = "macos") {
            Some(point(px(9.0), px(9.0)))
        } else {
            None
        }
    );
}

#[test]
fn containing_location_uses_the_most_specific_ancestor() {
    let locations = vec![
        ("Home".to_string(), PathBuf::from("root"), "home"),
        (
            "Documents".to_string(),
            PathBuf::from("root/Documents"),
            "file",
        ),
        (
            "Downloads".to_string(),
            PathBuf::from("root/Downloads"),
            "download",
        ),
    ];

    assert_eq!(
        most_specific_location_index(Path::new("root/Documents/projects/explorie"), &locations,),
        Some(1)
    );
    assert_eq!(
        most_specific_location_index(Path::new("elsewhere"), &locations),
        None
    );
}

#[test]
fn stepped_slider_fraction_spans_the_track_and_clamps_invalid_indexes() {
    assert_eq!(stepped_slider_fraction(0, 5), 0.0);
    assert_eq!(stepped_slider_fraction(2, 5), 0.5);
    assert_eq!(stepped_slider_fraction(4, 5), 1.0);
    assert_eq!(stepped_slider_fraction(20, 5), 1.0);
    assert_eq!(stepped_slider_fraction(0, 1), 0.0);
}

#[test]
fn syntax_highlights_ignore_ranges_before_the_rendered_line() {
    let document = TextPreviewDocument::new(TextPreview {
        text: "pub fn\nlet label".to_string(),
        truncated: false,
        language: Some("Rust".to_string()),
        encoding: "UTF-8".to_string(),
        wrapped: false,
        highlights: vec![explorie_native_services::TextHighlight {
            start: 0,
            end: 3,
            kind: TextHighlightKind::Keyword,
        }],
    });
    assert_eq!(document.line_syntax_spans(0).len(), 1);
    assert!(document.line_syntax_spans(1).is_empty());
}

#[test]
fn ui_scale_shortcuts_step_clamp_and_reset_to_the_default() {
    assert_eq!(stepped_f32(1.0, UI_SCALE_STEPS, 1), 1.1);
    assert_eq!(stepped_f32(1.0, UI_SCALE_STEPS, -1), 0.9);
    assert_eq!(stepped_f32(1.2, UI_SCALE_STEPS, 1), 1.25);
    assert_eq!(stepped_f32(1.2, UI_SCALE_STEPS, -1), 1.1);
    assert_eq!(stepped_f32(1.4, UI_SCALE_STEPS, 1), 1.4);
    assert_eq!(stepped_f32(0.9, UI_SCALE_STEPS, -1), 0.9);
    assert_eq!(AppearanceSettings::default().ui_scale, 1.0);
}

#[gpui::test]
fn ui_scale_shortcuts_dispatch_through_the_app_keymap(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            let view = cx.new(|cx| {
                let view = DirectoryWindow::new(directory.clone(), services, cx);
                view.install_shortcut_bindings(cx);
                view
            });
            window.focus(&view.focus_handle(cx), cx);
            view
        })
        .unwrap()
    });

    cx.dispatch_keystroke(*window, secondary_keystroke("+"));
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.settings.appearance.ui_scale, 1.1)
        })
        .unwrap();
    cx.dispatch_keystroke(*window, secondary_keystroke("-"));
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.settings.appearance.ui_scale, 1.0)
        })
        .unwrap();
    cx.dispatch_keystroke(*window, secondary_keystroke("-"));
    cx.dispatch_keystroke(*window, secondary_keystroke("0"));
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.settings.appearance.ui_scale, 1.0)
        })
        .unwrap();

    remove_fixture(&directory);
}

#[test]
fn default_font_resolves_to_the_platform_monospace_family() {
    assert_eq!(
        font_family(&AppSettings::default()),
        monospace_font_family()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_system_font_uses_gpui_platform_alias() {
    let mut settings = AppSettings::default();
    settings.appearance.font = FontChoice::System;
    assert_eq!(font_family(&settings), MACOS_SYSTEM_FONT_FAMILY);
}

pub(crate) fn fixture_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "explorie-gpui-fixture-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    fs::create_dir(&path).unwrap();
    #[cfg(target_os = "macos")]
    let path = path.canonicalize().unwrap();
    path
}

/// Removes a fixture directory. On Windows a file sent to the Recycle Bin or a
/// directory watch can hold a handle for a moment after an operation has
/// finished, so deletion is retried briefly there.
/// How long a test polls for background work (file operations, previews,
/// archive jobs) before giving up. Generous, because parallel test processes
/// on a loaded CI runner can make that work far slower than on a laptop.
pub(crate) const POLL_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) fn remove_fixture(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::remove_dir_all(path) {
            Ok(()) => return,
            // A background writer (a session flush, a thumbnail) can add a
            // file while the tree is being removed, and Windows keeps files
            // locked briefly after their handles close.
            Err(error)
                if Instant::now() < deadline
                    && (error.kind() == std::io::ErrorKind::DirectoryNotEmpty
                        || (cfg!(windows)
                            && error.kind() == std::io::ErrorKind::PermissionDenied)) =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("failed to remove {}: {error}", path.display()),
        }
    }
}

/// The name Keep Both gives the first copy of `name` on this platform.
pub(crate) fn kept_name(name: &str) -> PathBuf {
    PathBuf::from(explorie_core::numbered_name(
        std::ffi::OsStr::new(name),
        explorie_core::FIRST_CONFLICT_NUMBER,
        true,
    ))
}

pub(crate) fn secondary_keystroke(keys: &str) -> Keystroke {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    Keystroke::parse(&format!("{modifier}-{keys}")).unwrap()
}

fn minimal_pdf(page_count: usize) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut objects = Vec::with_capacity(page_count + 2);
    objects.push("<</Type/Catalog/Pages 2 0 R>>".to_string());
    let kids = (0..page_count)
        .map(|index| format!("{} 0 R", index + 3))
        .collect::<Vec<_>>()
        .join(" ");
    objects.push(format!("<</Type/Pages/Kids[{kids}]/Count {page_count}>>"));
    for _ in 0..page_count {
        objects.push("<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]>>".to_string());
    }
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj{object}endobj\n", index + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF",
            objects.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

fn minimal_psd(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"8BPS");
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.extend_from_slice(&[0; 6]);
    bytes.extend_from_slice(&4_u16.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&8_u16.to_be_bytes());
    bytes.extend_from_slice(&3_u16.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    let pixel_count = usize::try_from(u64::from(width) * u64::from(height)).unwrap();
    for channel in color {
        bytes.extend(std::iter::repeat_n(channel, pixel_count));
    }
    bytes
}

fn absolute_entry(path: PathBuf) -> FileEntry {
    FileEntry {
        id: Uuid::new_v4(),
        path,
        size: 0,
        modified: SystemTime::UNIX_EPOCH,
        hidden: false,
        is_dir: false,
        custom: std::collections::HashMap::new(),
        is_symlink: false,
        is_junction: false,
        link_target: None,
        has_xattrs: false,
        is_package: false,
        link_target_is_dir: false,
        is_cloud_placeholder: false,
        tags: Vec::new(),
    }
}

fn metadata_entry(path: PathBuf) -> FileEntry {
    let metadata = fs::metadata(&path).unwrap();
    let mut entry = absolute_entry(path);
    entry.size = metadata.len();
    entry.modified = metadata.modified().unwrap();
    entry
}

fn write_test_png(path: &Path, width: u32, height: u32, color: [u8; 4]) {
    image::RgbaImage::from_pixel(width, height, image::Rgba(color))
        .save(path)
        .unwrap();
}

#[derive(Debug)]
struct FakeGpuiAudioState {
    position: Duration,
    paused: bool,
    stopped: bool,
    volume: f32,
}

impl Default for FakeGpuiAudioState {
    fn default() -> Self {
        Self {
            position: Duration::ZERO,
            paused: true,
            stopped: false,
            volume: 0.8,
        }
    }
}

#[derive(Default)]
struct FakeGpuiAudioPlayback {
    state: Mutex<FakeGpuiAudioState>,
}

impl AudioPlayback for FakeGpuiAudioPlayback {
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(80))
    }

    fn position(&self) -> Duration {
        self.state.lock().unwrap().position
    }

    fn is_paused(&self) -> bool {
        self.state.lock().unwrap().paused
    }

    fn is_empty(&self) -> bool {
        self.state.lock().unwrap().stopped
    }

    fn play(&self) {
        let mut state = self.state.lock().unwrap();
        state.paused = false;
        state.stopped = false;
    }

    fn pause(&self) {
        self.state.lock().unwrap().paused = true;
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        state.paused = true;
        state.stopped = true;
    }

    fn seek(&self, position: Duration) -> ServiceResult<()> {
        let mut state = self.state.lock().unwrap();
        state.position = position;
        state.stopped = false;
        Ok(())
    }

    fn volume(&self) -> f32 {
        self.state.lock().unwrap().volume
    }

    fn set_volume(&self, volume: f32) {
        self.state.lock().unwrap().volume = volume;
    }
}

struct FakeGpuiAudioBackend {
    playback: Arc<FakeGpuiAudioPlayback>,
}

impl AudioBackend for FakeGpuiAudioBackend {
    fn open(&self, _path: &Path) -> ServiceResult<Arc<dyn AudioPlayback>> {
        *self.playback.state.lock().unwrap() = FakeGpuiAudioState::default();
        Ok(self.playback.clone())
    }
}

struct FakeGpuiVideoPlayback {
    status: Mutex<VideoStatus>,
    frame: Mutex<Option<VideoFrame>>,
    stopped: Mutex<bool>,
}

impl FakeGpuiVideoPlayback {
    fn new(path: PathBuf) -> Self {
        Self {
            status: Mutex::new(VideoStatus {
                path,
                duration_ms: Some(80_000),
                position_ms: 0,
                width: 640,
                height: 360,
                playing: false,
                finished: false,
                has_audio: true,
                volume: 0.8,
            }),
            frame: Mutex::new(Some(VideoFrame {
                width: 2,
                height: 2,
                position_ms: 0,
                bgra: vec![
                    0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 255, 255, 255, 255,
                ]
                .into(),
            })),
            stopped: Mutex::new(false),
        }
    }
}

impl VideoPlayback for FakeGpuiVideoPlayback {
    fn status(&self) -> VideoStatus {
        self.status.lock().unwrap().clone()
    }

    fn take_frame(&self) -> Option<VideoFrame> {
        self.frame.lock().unwrap().clone()
    }

    fn play(&self) -> ServiceResult<VideoStatus> {
        let mut status = self.status.lock().unwrap();
        status.playing = true;
        status.finished = false;
        *self.stopped.lock().unwrap() = false;
        Ok(status.clone())
    }

    fn pause(&self) -> ServiceResult<VideoStatus> {
        let mut status = self.status.lock().unwrap();
        status.playing = false;
        Ok(status.clone())
    }

    fn seek(&self, position: Duration) -> ServiceResult<VideoStatus> {
        let mut status = self.status.lock().unwrap();
        status.position_ms = u64::try_from(position.as_millis()).unwrap();
        self.frame.lock().unwrap().as_mut().unwrap().position_ms = status.position_ms;
        Ok(status.clone())
    }

    fn set_volume(&self, volume: f32) -> ServiceResult<VideoStatus> {
        let mut status = self.status.lock().unwrap();
        status.volume = volume.clamp(0.0, 1.0);
        Ok(status.clone())
    }

    fn stop(&self) {
        let mut status = self.status.lock().unwrap();
        status.playing = false;
        *self.stopped.lock().unwrap() = true;
    }
}

struct FakeGpuiVideoBackend {
    playback: Arc<FakeGpuiVideoPlayback>,
}

impl VideoBackend for FakeGpuiVideoBackend {
    fn open(&self, _path: &Path) -> ServiceResult<Arc<dyn VideoPlayback>> {
        *self.playback.stopped.lock().unwrap() = false;
        Ok(self.playback.clone())
    }
}

#[derive(Default)]
struct FakeFinderTagsState {
    tags: Vec<String>,
    get_calls: usize,
    set_calls: usize,
    fail_next_set: bool,
}

#[derive(Clone, Default)]
struct FakeFinderTagsBackend {
    state: Arc<Mutex<FakeFinderTagsState>>,
}

impl FinderTagsBackend for FakeFinderTagsBackend {
    fn supported(&self) -> bool {
        true
    }

    fn get(&self, _path: &Path) -> std::io::Result<Vec<String>> {
        let mut state = self.state.lock().unwrap();
        state.get_calls += 1;
        Ok(state.tags.clone())
    }

    fn set(&self, _path: &Path, tags: &[String]) -> std::io::Result<()> {
        let mut state = self.state.lock().unwrap();
        state.set_calls += 1;
        if state.fail_next_set {
            state.fail_next_set = false;
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "fake Finder tag write denied",
            ));
        }
        state.tags = tags.to_vec();
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RecordedPlatformAction {
    Open(PathBuf),
    Reveal(PathBuf),
    OpenWith(PathBuf, String),
    AppsForFile(PathBuf),
}

#[derive(Default)]
struct FakePlatformActionsState {
    actions: Vec<RecordedPlatformAction>,
    fail_next: Option<&'static str>,
}

#[derive(Clone, Default)]
struct FakePlatformActionsBackend {
    state: Arc<Mutex<FakePlatformActionsState>>,
}

impl FakePlatformActionsBackend {
    fn record(&self, name: &'static str, action: RecordedPlatformAction) -> std::io::Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.fail_next.take() == Some(name) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("fake {name} denied"),
            ));
        }
        state.actions.push(action);
        Ok(())
    }
}

impl PlatformActionsBackend for FakePlatformActionsBackend {
    fn open(&self, path: &Path) -> std::io::Result<()> {
        self.record("open", RecordedPlatformAction::Open(path.to_path_buf()))
    }

    fn reveal(&self, path: &Path) -> std::io::Result<()> {
        self.record("reveal", RecordedPlatformAction::Reveal(path.to_path_buf()))
    }

    fn open_with(&self, path: &Path, app_name: &str) -> std::io::Result<()> {
        self.record(
            "open_with",
            RecordedPlatformAction::OpenWith(path.to_path_buf(), app_name.to_string()),
        )
    }

    fn apps_for_file(&self, path: &Path) -> std::io::Result<Vec<AppInfo>> {
        self.record(
            "apps_for_file",
            RecordedPlatformAction::AppsForFile(path.to_path_buf()),
        )?;
        Ok(vec![AppInfo {
            name: "Fixture App".to_string(),
            path: PathBuf::from("/Applications/Fixture.app"),
            bundle_id: Some("dev.explorie.fixture".to_string()),
            is_default: false,
        }])
    }
}

#[derive(Clone, Default)]
struct FakeGpuiRemoteBackend {
    state: Arc<Mutex<FakeGpuiRemoteState>>,
}

#[derive(Default)]
struct FakeGpuiRemoteState {
    start_attempts: usize,
    started: usize,
    stopped: usize,
    pending_uploads: u64,
    quit_requested: bool,
    fail_start: bool,
    fail_starts_remaining: usize,
}

struct FakeGpuiRemoteProcess {
    state: Arc<Mutex<FakeGpuiRemoteState>>,
    alive: bool,
}

impl RemoteDriveProcess for FakeGpuiRemoteProcess {
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

impl RemoteDriveBackend for FakeGpuiRemoteBackend {
    fn find_rclone(&self, _resources: &ResourcePaths) -> Option<PathBuf> {
        Some(PathBuf::from("fake-rclone"))
    }

    fn rclone_version(&self, _rclone: &Path) -> ServiceResult<String> {
        Ok("fake-rclone 1.0".to_string())
    }

    fn list_remotes(&self, _rclone: &Path) -> ServiceResult<Vec<String>> {
        Ok(vec!["cloud".to_string(), "backup".to_string()])
    }

    fn ensure_capabilities(&self, _rclone: &Path) -> ServiceResult<()> {
        Ok(())
    }

    fn winfsp_available(&self) -> Option<bool> {
        cfg!(windows).then_some(true)
    }

    fn occupied_mount_targets(&self) -> Vec<String> {
        Vec::new()
    }

    fn helper_status(&self) -> Option<String> {
        cfg!(target_os = "macos").then(|| "enabled".to_string())
    }

    fn configure(&self, _rclone: &Path, _resources: &ResourcePaths) -> ServiceResult<()> {
        Ok(())
    }

    fn start_mount(
        &self,
        _request: &RemoteMountRequest,
    ) -> ServiceResult<Box<dyn RemoteDriveProcess>> {
        let mut state = self.state.lock().unwrap();
        state.start_attempts += 1;
        if state.fail_start || state.fail_starts_remaining > 0 {
            state.fail_starts_remaining = state.fail_starts_remaining.saturating_sub(1);
            return Err(
                ServiceError::new(ErrorCode::RemoteUnavailable, "fake remote is offline")
                    .retryable(true),
            );
        }
        state.started += 1;
        state.quit_requested = false;
        drop(state);
        Ok(Box::new(FakeGpuiRemoteProcess {
            state: Arc::clone(&self.state),
            alive: true,
        }))
    }

    fn remote_control(&self, request: &RemoteControlRequest) -> ServiceResult<serde_json::Value> {
        match request.endpoint.as_str() {
            "rc/noopauth" => Ok(serde_json::json!({})),
            "vfs/stats" => Ok(serde_json::json!({
                "diskCache": {
                    "uploadsQueued": self.state.lock().unwrap().pending_uploads,
                    "uploadsInProgress": 0,
                    "erroredFiles": 0
                }
            })),
            "core/quit" => {
                self.state.lock().unwrap().quit_requested = true;
                Ok(serde_json::json!({}))
            }
            endpoint => Err(ServiceError::new(
                ErrorCode::Unsupported,
                format!("unexpected fake endpoint: {endpoint}"),
            )),
        }
    }

    fn mount_helper(&self, _id: &str, _volume_name: &str, _port: u16) -> ServiceResult<()> {
        Ok(())
    }

    fn unmount_helper(&self, _id: &str, _volume_name: &str, _force: bool) -> ServiceResult<()> {
        Ok(())
    }

    fn install_winfsp(&self, _context: &ServiceContext) -> ServiceResult<()> {
        Ok(())
    }

    fn register_helper(&self) -> ServiceResult<String> {
        Ok("enabled".to_string())
    }

    fn unregister_helper(&self) -> ServiceResult<()> {
        Ok(())
    }

    fn open_helper_settings(&self) -> ServiceResult<()> {
        Ok(())
    }
}

fn unused_remote_mount_target() -> String {
    if cfg!(windows) {
        (b'D'..=b'Z')
            .map(|letter| format!("{}:", char::from(letter)))
            .find(|target| !Path::new(&format!("{target}\\")).exists())
            .unwrap_or_else(|| "Z:".to_string())
    } else {
        format!("Explorie Test {}", std::process::id())
    }
}

fn remote_profile(id: &str) -> RemoteDriveProfile {
    RemoteDriveProfile {
        id: id.to_string(),
        name: "Projects".to_string(),
        remote: "cloud".to_string(),
        remote_path: "work".to_string(),
        mount_target: unused_remote_mount_target(),
    }
}

#[test]
fn startup_path_uses_the_first_existing_directory_argument() {
    let directory = fixture_dir();
    let args = [
        OsString::from("explorie-gpui"),
        OsString::from("missing-directory"),
        directory.clone().into_os_string(),
    ];

    assert_eq!(parse_startup_path(args), Some(directory.clone()));
    remove_fixture(&directory);
}

#[test]
fn startup_path_rejects_files_and_missing_arguments() {
    let directory = fixture_dir();
    let file = directory.join("not-a-directory.txt");
    fs::write(&file, b"fixture").unwrap();
    let args = [
        OsString::from("explorie-gpui"),
        file.into_os_string(),
        OsString::from("missing-directory"),
    ];

    assert_eq!(parse_startup_path(args), None);
    remove_fixture(&directory);
}

#[test]
fn photo_location_rows_are_absent_until_explicitly_revealed() {
    let metadata = ImageMetadata {
        camera: Some("Canon EOS R5".to_string()),
        taken_at: Some("2024:03:15 14:30:00".to_string()),
        width: Some(4032),
        height: Some(3024),
        caption: Some("Harbor at dusk".to_string()),
        keywords: vec!["ocean".to_string(), "travel".to_string()],
        gps: Some(explorie_native_services::ImageGps {
            latitude: 37.5,
            longitude: -122.25,
        }),
    };

    let hidden = photo_metadata_rows(&metadata, false);
    assert!(
        hidden
            .iter()
            .all(|(label, _)| !matches!(*label, "Latitude" | "Longitude"))
    );
    let revealed = photo_metadata_rows(&metadata, true);
    assert!(revealed.contains(&("Latitude", "37.500000".to_string())));
    assert!(revealed.contains(&("Longitude", "-122.250000".to_string())));
}

#[test]
fn service_task_handoff_returns_a_real_core_listing() {
    let directory = fixture_dir();
    fs::write(directory.join("fixture.txt"), b"fixture").unwrap();
    fs::create_dir(directory.join("nested")).unwrap();

    let event = pollster::block_on(list_directory_task(
        NativeServices::default(),
        7,
        ListRequest {
            path: directory.clone(),
            calc_dir_size: false,
        },
    ));

    match event {
        DirectoryEvent::Listed {
            generation,
            entries,
            ..
        } => {
            assert_eq!(generation, 7);
            assert_eq!(entries.len(), 2);
            assert!(entries.iter().any(|entry| {
                entry
                    .path
                    .file_name()
                    .is_some_and(|name| name == "fixture.txt")
            }));
            assert!(
                entries
                    .iter()
                    .any(|entry| { entry.path.file_name().is_some_and(|name| name == "nested") })
            );
        }
        DirectoryEvent::Failed { error, .. } => panic!("listing failed: {error}"),
    }

    remove_fixture(&directory);
}

#[test]
fn folder_size_listing_is_opt_in_and_runs_through_native_services() {
    let directory = fixture_dir();
    let nested = directory.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("seven.txt"), b"1234567").unwrap();

    let event = pollster::block_on(list_directory_task(
        NativeServices::default(),
        8,
        ListRequest {
            path: directory.clone(),
            calc_dir_size: true,
        },
    ));

    match event {
        DirectoryEvent::Listed { entries, .. } => {
            let nested = entries
                .iter()
                .find(|entry| entry.path.ends_with("nested"))
                .expect("nested directory entry");
            assert!(nested.is_dir);
            assert_eq!(nested.size, 7);
        }
        DirectoryEvent::Failed { error, .. } => panic!("listing failed: {error}"),
    }

    remove_fixture(&directory);
}

#[test]
fn link_identity_takes_precedence_over_generic_file_kind() {
    let mut entry = FileEntry {
        id: Uuid::new_v4(),
        path: PathBuf::from("linked"),
        size: 42,
        modified: SystemTime::UNIX_EPOCH,
        hidden: false,
        is_dir: true,
        custom: std::collections::HashMap::new(),
        is_symlink: true,
        is_junction: false,
        link_target: Some("target".to_string()),
        has_xattrs: false,
        is_package: false,
        link_target_is_dir: false,
        is_cloud_placeholder: false,
        tags: Vec::new(),
    };
    assert_eq!(entry_link_badge(&entry), Some("↗"));
    assert_eq!(entry_detail(&entry, true, true), "Symlink");

    entry.is_junction = true;
    assert_eq!(entry_link_badge(&entry), Some("⤴"));
    assert_eq!(entry_detail(&entry, true, true), "Junction");
}

#[test]
fn icon_cache_deduplicates_file_kinds_but_tracks_source_specific_identity() {
    let mut first = absolute_entry(PathBuf::from("first.txt"));
    let mut second = absolute_entry(PathBuf::from("second.txt"));
    second.size = 42;
    second.modified = UNIX_EPOCH + Duration::from_secs(10);
    assert_eq!(
        EntryIconKey::for_entry(&first),
        EntryIconKey::for_entry(&second),
        "associated file icons should be shared by extension"
    );

    // Windows executables and macOS applications carry their own icons.
    if cfg!(target_os = "macos") {
        first.path = PathBuf::from("Tool.app");
        first.is_dir = true;
        first.is_package = true;
        second.path = first.path.clone();
        second.is_dir = true;
        second.is_package = true;
    } else {
        first.path = PathBuf::from("tool.exe");
        second.path = first.path.clone();
    }
    assert_ne!(
        EntryIconKey::for_entry(&first),
        EntryIconKey::for_entry(&second)
    );
    first.is_dir = false;
    first.is_package = false;

    first.is_symlink = true;
    first.path = PathBuf::from("linked.txt");
    let original = EntryIconKey::for_entry(&first);
    first.modified = UNIX_EPOCH + Duration::from_secs(20);
    assert_ne!(original, EntryIconKey::for_entry(&first));
}

#[test]
fn native_palette_matches_the_accepted_legacy_visual_tokens() {
    let mut settings = AppSettings::default();
    let dark = UiPalette::for_settings(&settings, WindowAppearance::Dark);
    assert_eq!(dark.window, rgb(0x0f0f0f));
    assert_eq!(dark.topbar, rgb(0x111111));
    assert_eq!(dark.panel, rgb(0x151515));
    assert_eq!(dark.surface, rgb(0x181818));
    assert_eq!(dark.control, rgb(0x1b1b1b));
    assert_eq!(dark.disabled, rgb(0x202020));
    assert_eq!(dark.border, rgb(0x252525));
    assert_eq!(dark.text, rgb(0xdddddd));
    assert_eq!(dark.muted, rgb(0xaaaaaa));
    assert_eq!(dark.tertiary, rgb(0x8a8a8a));
    assert_eq!(
        dark.hover,
        dark.window.blend(with_alpha(rgb(0xffffff), 0.045))
    );
    assert_eq!(dark.accent, rgb(0x7cc7ff));
    assert!(adaptive_hover(dark.control).r > dark.control.r);

    settings.appearance.theme = ThemeMode::Light;
    settings.appearance.accent = AccentColor::Pink;
    let light = UiPalette::for_settings(&settings, WindowAppearance::Dark);
    assert_eq!(light.window, rgb(0xf6f6f6));
    assert_eq!(light.topbar, rgb(0xf3f3f3));
    assert_eq!(light.panel, rgb(0xefefef));
    assert_eq!(light.surface, rgb(0xffffff));
    assert_eq!(light.control, rgb(0xe8e8e8));
    assert_eq!(light.disabled, rgb(0xe2e2e2));
    assert_eq!(light.border, rgb(0xd0d0d0));
    assert_eq!(light.text, rgb(0x222222));
    assert_eq!(light.muted, rgb(0x555555));
    assert_eq!(light.tertiary, rgb(0x707070));
    assert_eq!(
        light.hover,
        light.window.blend(with_alpha(rgb(0x000000), 0.045))
    );
    assert_eq!(light.accent, rgb(0xff8aa0));
    assert!(adaptive_hover(light.control).r < light.control.r);

    settings.appearance.high_contrast = true;
    let contrast = UiPalette::for_settings(&settings, WindowAppearance::Dark);
    assert_eq!(contrast.window, rgb(0xffffff));
    assert_eq!(contrast.topbar, rgb(0xfafafa));
    assert_eq!(contrast.panel, rgb(0xf5f5f5));
    assert_eq!(contrast.control, rgb(0xebebeb));
    assert_eq!(contrast.border, rgb(0x808080));
    assert_eq!(contrast.text, rgb(0x000000));
    assert_eq!(contrast.muted, rgb(0x1a1a1a));
    assert_eq!(contrast.tertiary, rgb(0x333333));
}

#[test]
fn embedded_toolbar_assets_match_the_pinned_legacy_icon_vocabulary() {
    let assets = ExplorieAssets;
    let names = assets.list("icons").unwrap();
    assert!(names.iter().any(|asset| asset.as_ref() == "icon.png"));
    let app_icon = assets
        .load("icons/icon.png")
        .unwrap()
        .expect("embedded PNG app icon");
    assert_eq!(&app_icon[..8], b"\x89PNG\r\n\x1a\n");
    let titlebar_icon = assets
        .load("icons/titlebar-icon.png")
        .unwrap()
        .expect("optimized title-bar icon");
    let titlebar_icon = image::load_from_memory(&titlebar_icon)
        .unwrap()
        .into_rgba8();
    assert_eq!(titlebar_icon.dimensions(), (44, 36));
    let visible = titlebar_icon
        .enumerate_pixels()
        .filter(|(_, _, pixel)| pixel[3] > 8)
        .map(|(x, y, _)| (x, y))
        .collect::<Vec<_>>();
    assert_eq!(visible.iter().map(|(x, _)| *x).min(), Some(0));
    assert_eq!(visible.iter().map(|(x, _)| *x).max(), Some(43));
    assert!(visible.iter().map(|(_, y)| *y).min().unwrap_or(36) <= 1);
    assert!(visible.iter().map(|(_, y)| *y).max().unwrap_or(0) >= 34);
    for name in [
        "app-face.svg",
        "arrow-left.svg",
        "arrow-right.svg",
        "arrow-up.svg",
        "plus.svg",
        "undo.svg",
        "redo.svg",
        "search.svg",
        "list.svg",
        "grid.svg",
        "frame.svg",
        "minus.svg",
        "view-col.svg",
        "home.svg",
        "desktop.svg",
        "download.svg",
        "drive.svg",
        "music.svg",
        "image.svg",
        "video.svg",
        "sort.svg",
        "shield.svg",
        "more-vertical.svg",
    ] {
        assert!(names.iter().any(|asset| asset.as_ref() == name));
        let bytes = assets
            .load(&format!("icons/{name}"))
            .unwrap()
            .expect("embedded toolbar icon");
        let svg = std::str::from_utf8(bytes.as_ref()).unwrap();
        assert!(svg.contains("viewBox=\"0 0 24 24\""));
        assert!(svg.contains("currentColor"));
    }
    assert!(assets.load("icons/not-packaged.svg").unwrap().is_none());
}

#[gpui::test]
fn breadcrumb_background_restores_inline_path_edit_submit_and_cancel(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    // Deep enough to overflow the breadcrumb strip regardless of TMPDIR length.
    let mut target = directory.clone();
    for depth in 0..8 {
        target.push(format!("nested-breadcrumb-folder-{depth}"));
    }
    target.push("Target");
    fs::create_dir_all(&target).unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));

    window.simulate_resize(gpui::size(px(1_200.0), px(720.0)));
    window.run_until_parked();
    let hit_area = window.debug_bounds("breadcrumb-edit-hit-area").unwrap();
    window.simulate_click(hit_area.center(), gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        let editor = view.navigation_ui.breadcrumb_editor.as_ref().unwrap();
        assert_eq!(editor.input, directory.to_string_lossy());
        assert!(editor.replace_on_type);
    });
    let breadcrumbs = window.debug_bounds("breadcrumbs").unwrap();
    let input = window.debug_bounds("breadcrumb-path-input").unwrap();
    assert!(input.left() >= breadcrumbs.left());
    assert!(input.right() <= breadcrumbs.right());
    assert!(input.top() >= breadcrumbs.top());
    assert!(input.bottom() <= breadcrumbs.bottom());

    view.update(window, |view, cx| {
        view.navigation_ui.breadcrumb_editor.as_mut().unwrap().input =
            target.to_string_lossy().into_owned();
        view.submit_breadcrumb_edit(cx);
        assert!(view.navigation_ui.breadcrumb_editor.is_none());
        assert_eq!(view.browser.path(), target.as_path());
    });
    window.run_until_parked();

    view.update(window, |view, cx| {
        view.begin_breadcrumb_edit(cx);
        view.cancel_breadcrumb_edit(cx);
    });
    view.update(window, |view, _| {
        assert!(view.navigation_ui.breadcrumb_editor.is_none())
    });

    let current = build_path_stack(&target).len() - 1;
    let selector = Box::leak(format!("breadcrumb-{current}").into_boxed_str());
    let parent_selector = Box::leak(format!("breadcrumb-{}", current - 1).into_boxed_str());
    let breadcrumbs = window.debug_bounds("breadcrumbs").unwrap();
    let current = window.debug_bounds(selector).unwrap();
    let parent = window.debug_bounds(parent_selector).unwrap();
    assert!(
        current.left() >= breadcrumbs.left() && current.right() <= breadcrumbs.right(),
        "current crumb {current:?} must stay visible inside {breadcrumbs:?}"
    );
    assert!(
        f32::from(current.size.width) >= 32.0,
        "current crumb {current:?} must keep a readable label"
    );
    assert!(
        parent.left() >= breadcrumbs.left() && parent.right() <= current.left(),
        "nearest parent {parent:?} must stay visible before {current:?}"
    );
    window.simulate_click(current.center(), gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert!(view.navigation_ui.breadcrumb_editor.is_some());
        assert_eq!(view.browser.path(), target.as_path());
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn default_shell_uses_single_row_view_controls_and_navigation_first_sidebar(
    cx: &mut TestAppContext,
) {
    let directory = fixture_dir();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(window, |view, cx| {
        view.browser.new_tab();
        view.listing.state = ListingState::Ready;
        cx.notify();
    });

    window.simulate_resize(gpui::size(px(1_024.0), px(768.0)));
    window.run_until_parked();

    assert_eq!(
        f32::from(window.debug_bounds("browser-toolbar").unwrap().size.height),
        40.0
    );
    assert!(window.debug_bounds("view-mode-control").is_some());
    for selector in ["view-list-button", "view-grid-button", "view-column-button"] {
        assert!(
            window.debug_bounds(selector).is_some(),
            "missing visible view control {selector}"
        );
    }
    assert!(window.debug_bounds("view-menu-button").is_none());
    assert!(window.debug_bounds("view-options-button").is_some());
    assert!(window.debug_bounds("toggle-favorite-button").is_some());
    assert!(window.debug_bounds("toggle-current-favorite").is_none());
    assert!(window.debug_bounds("move-tab-left").is_none());
    assert!(window.debug_bounds("move-tab-right").is_none());

    let favorites = window.debug_bounds("favorites-section").unwrap();
    let smart_folders = window.debug_bounds("smart-folders-section").unwrap();
    let locations = window.debug_bounds("locations-section").unwrap();
    let recents = window.debug_bounds("recents-section").unwrap();
    assert!(favorites.top() < smart_folders.top());
    assert!(smart_folders.top() < locations.top());
    assert!(locations.top() < recents.top());

    let grid = window.debug_bounds("view-grid-button").unwrap().center();
    window.simulate_click(grid, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.view_mode(), ViewMode::Grid)
    });

    let view_options = window.debug_bounds("view-options-button").unwrap().center();
    window.simulate_click(view_options, gpui::Modifiers::default());
    window.run_until_parked();
    assert!(window.debug_bounds("view-preview").is_some());
    window.simulate_keystrokes("escape");

    let favorite = window
        .debug_bounds("toggle-favorite-button")
        .unwrap()
        .center();
    window.simulate_click(favorite, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert!(view.browser.is_favorite(&directory));
    });

    remove_fixture(&directory);
}

#[gpui::test]
fn compact_toolbar_popovers_preserve_actions_at_the_minimum_window_size(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    fs::write(directory.join("alpha.txt"), b"alpha").unwrap();
    fs::create_dir(directory.join("Folder")).unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(window, |view, cx| {
        view.browser.replace_entries(vec![
            metadata_entry(directory.join("Folder")),
            metadata_entry(directory.join("alpha.txt")),
        ]);
        view.listing.state = ListingState::Ready;
        cx.notify();
    });

    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();
    #[cfg(any(windows, target_os = "macos"))]
    let content_top = {
        let title_bar = window.debug_bounds("title-bar").unwrap();
        let drag_region = window.debug_bounds("title-bar-drag-region").unwrap();
        assert_eq!(f32::from(title_bar.size.height), 36.0);
        assert_eq!(title_bar.top(), px(0.0));
        assert_eq!(drag_region.top(), title_bar.top());
        assert_eq!(drag_region.bottom(), title_bar.bottom());
        #[cfg(windows)]
        let app_icon = window.debug_bounds("title-bar-app-icon").unwrap();
        #[cfg(windows)]
        {
            assert_eq!(f32::from(app_icon.size.width), 22.0);
            assert_eq!(f32::from(app_icon.size.height), 22.0);
            assert!(app_icon.left() >= drag_region.left());
            assert!(app_icon.right() <= drag_region.right());
        }
        #[cfg(target_os = "macos")]
        assert!(window.debug_bounds("title-bar-folder-name").is_some());
        #[cfg(windows)]
        for selector in ["window-minimize", "window-maximize", "window-close"] {
            let bounds = window.debug_bounds(selector).unwrap();
            assert_eq!(f32::from(bounds.size.width), 44.0);
            assert_eq!(bounds.top(), title_bar.top());
            assert_eq!(bounds.bottom(), title_bar.bottom());
        }
        title_bar.bottom()
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let content_top = {
        assert!(window.debug_bounds("title-bar").is_none());
        px(0.0)
    };

    view.update(window, |view, cx| {
        view.browser.select(directory.join("alpha.txt"));
        cx.notify();
    });
    window.run_until_parked();
    assert_eq!(
        f32::from(window.debug_bounds("operation-panel").unwrap().size.height),
        0.0,
        "selection alone must not expand the contextual operation strip"
    );

    let toolbar = window.debug_bounds("browser-toolbar").unwrap();
    assert_eq!(f32::from(toolbar.size.height), 72.0);
    let sidebar = window.debug_bounds("sidebar").unwrap();
    let tabs = window.debug_bounds("tabs").unwrap();
    assert_eq!(sidebar.top(), content_top);
    assert!(
        toolbar.left() >= sidebar.right(),
        "toolbar {toolbar:?} must stay inside the main area beside sidebar {sidebar:?}"
    );
    assert!(
        tabs.left() >= sidebar.right(),
        "tabs {tabs:?} must stay inside the main area beside sidebar {sidebar:?}"
    );
    assert!(
        toolbar.bottom() <= tabs.top(),
        "legacy hierarchy requires toolbar {toolbar:?} above tabs {tabs:?}"
    );
    let status = window.debug_bounds("watcher-status").unwrap();
    assert!(status.left() >= sidebar.right());
    assert!(sidebar.top() <= toolbar.top());
    assert!(sidebar.bottom() >= status.bottom());
    let breadcrumbs = window.debug_bounds("breadcrumbs").unwrap();
    let leaf_index = build_path_stack(&directory).len() - 1;
    let leaf = window
        .debug_bounds(Box::leak(
            format!("breadcrumb-{leaf_index}").into_boxed_str(),
        ))
        .unwrap();
    assert!(
        leaf.left() >= breadcrumbs.left(),
        "leaf {leaf:?} starts before breadcrumbs {breadcrumbs:?}"
    );
    assert!(
        leaf.right() <= breadcrumbs.right(),
        "leaf {leaf:?} ends after breadcrumbs {breadcrumbs:?}"
    );
    let primary_trigger_y = f32::from(
        window
            .debug_bounds("create-menu-button")
            .unwrap()
            .center()
            .y,
    );
    for selector in ["back", "forward", "up", "refresh", "undo", "redo"] {
        assert_eq!(
            f32::from(window.debug_bounds(selector).unwrap().center().y),
            primary_trigger_y,
            "primary toolbar action {selector} left the navigation row"
        );
    }
    let secondary_trigger_y =
        f32::from(window.debug_bounds("view-menu-button").unwrap().center().y);
    assert!(secondary_trigger_y > primary_trigger_y);
    for selector in ["view-menu-button", "sort", "filter", "more-menu-button"] {
        let bounds = window.debug_bounds(selector).unwrap();
        assert_eq!(f32::from(bounds.size.width), 32.0);
        assert_eq!(f32::from(bounds.size.height), 32.0);
        assert_eq!(f32::from(bounds.center().y), secondary_trigger_y);
    }
    assert!(window.debug_bounds("new-folder").is_none());

    let create = window.debug_bounds("create-menu-button").unwrap();
    window.simulate_click(create.center(), gpui::Modifiers::default());
    window.run_until_parked();
    let create_menu = window.debug_bounds("create-menu").unwrap();
    assert!((f32::from(create_menu.left()) - f32::from(create.left())).abs() <= 2.0);
    assert!(create_menu.top() >= create.bottom() + px(2.0));
    let new_folder = window.debug_bounds("new-folder").unwrap().center();
    window.simulate_click(new_folder, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, cx| {
        assert_eq!(view.overlay.toolbar_menu, ToolbarMenu::Closed);
        assert!(matches!(
            view.mutation.prompt,
            Some(MutationPrompt {
                kind: MutationPromptKind::NewFolder,
                ..
            })
        ));
        view.cancel_mutation_prompt(cx);
    });

    let view_trigger = window.debug_bounds("view-menu-button").unwrap().center();
    window.simulate_click(view_trigger, gpui::Modifiers::default());
    window.run_until_parked();
    assert!(window.debug_bounds("view-menu").is_some());
    assert!(window.debug_bounds("view-preview").is_some());
    let grid = window.debug_bounds("grid-view").unwrap().center();
    window.simulate_click(grid, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.view_mode(), ViewMode::Grid);
        assert_eq!(view.overlay.toolbar_menu, ToolbarMenu::Closed);
    });

    let sort_trigger = window.debug_bounds("sort").unwrap();
    window.simulate_click(sort_trigger.center(), gpui::Modifiers::default());
    window.run_until_parked();
    let sort_menu = window.debug_bounds("sort-menu").unwrap();
    assert!((f32::from(sort_menu.right()) - f32::from(sort_trigger.right())).abs() <= 2.0);
    assert!(sort_menu.top() >= sort_trigger.bottom() + px(2.0));
    let sort_size = window.debug_bounds("toolbar-sort-size").unwrap().center();
    window.simulate_click(sort_size, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.sort_key(), SortKey::Size)
    });

    let filter_trigger = window.debug_bounds("filter").unwrap();
    window.simulate_click(filter_trigger.center(), gpui::Modifiers::default());
    window.run_until_parked();
    let filter_menu = window.debug_bounds("filter-menu").unwrap();
    assert!((f32::from(filter_menu.right()) - f32::from(filter_trigger.right())).abs() <= 2.0);
    assert!(filter_menu.top() >= filter_trigger.bottom() + px(2.0));
    let folders = window
        .debug_bounds("toolbar-filter-folders")
        .unwrap()
        .center();
    window.simulate_click(folders, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.filter(), EntryFilter::Folders)
    });

    let more = window.debug_bounds("more-menu-button").unwrap();
    window.simulate_click(more.center(), gpui::Modifiers::default());
    window.run_until_parked();
    let more_menu = window.debug_bounds("more-menu").unwrap();
    assert!((f32::from(more_menu.right()) - f32::from(more.right())).abs() <= 2.0);
    assert!(more_menu.top() >= more.bottom() + px(2.0));
    for selector in [
        "toolbar-theme",
        "folder-sizes",
        "workspace-manager-button",
        "remote-drive-manager-button",
        "command-palette-button",
        "shortcut-help-button",
    ] {
        assert!(
            window.debug_bounds(selector).is_some(),
            "missing More action {selector}"
        );
    }
    assert!(
        window.debug_bounds("sidebar-settings").is_some(),
        "missing fixed sidebar Settings action"
    );
    window.simulate_keystrokes("escape");
    view.update(window, |view, _| {
        assert_eq!(view.overlay.toolbar_menu, ToolbarMenu::Closed)
    });

    remove_fixture(&directory);
}

#[gpui::test]
fn grid_view_popover_restores_thumbnail_presets_slider_and_geometry(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(window, |view, cx| {
        view.set_view_mode(ViewMode::Grid, cx);
        view.settings.appearance.grid_min_width = 140;
        view.listing.state = ListingState::Ready;
        cx.notify();
    });

    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();
    let trigger = window.debug_bounds("view-menu-button").unwrap();
    window.simulate_click(trigger.center(), gpui::Modifiers::default());
    window.run_until_parked();

    let menu = window.debug_bounds("view-menu").unwrap();
    let control = window.debug_bounds("thumbnail-size-control").unwrap();
    let label = window.debug_bounds("thumbnail-size-label").unwrap();
    let presets = window.debug_bounds("thumbnail-size-presets").unwrap();
    let slider = window.debug_bounds("thumbnail-size-slider").unwrap();
    assert_eq!(f32::from(menu.size.width), 280.0);
    assert!(
        (f32::from(menu.right()) - f32::from(trigger.right())).abs() <= 2.0,
        "view menu {menu:?} must remain right-aligned under trigger {trigger:?}"
    );
    assert!(menu.top() >= trigger.bottom() + px(2.0));
    assert!(control.left() >= menu.left());
    assert!(control.right() <= menu.right());
    assert!(label.top() >= control.top());
    assert!(label.bottom() <= presets.top());
    assert!(presets.bottom() <= slider.top());
    assert!(slider.bottom() <= control.bottom());
    for selector in [
        "thumbnail-size-small",
        "thumbnail-size-medium",
        "thumbnail-size-large",
        "thumbnail-size-extra-large",
    ] {
        let preset = window.debug_bounds(selector).unwrap();
        assert!(preset.left() >= presets.left());
        assert!(preset.right() <= presets.right());
    }

    let extra_large = window
        .debug_bounds("thumbnail-size-extra-large")
        .unwrap()
        .center();
    window.simulate_click(extra_large, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.settings.appearance.grid_min_width, 260);
        assert_eq!(view.overlay.toolbar_menu, ToolbarMenu::View);
    });

    let step_170 = window
        .debug_bounds("thumbnail-size-step-5")
        .unwrap()
        .center();
    window.simulate_click(step_170, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, cx| {
        assert_eq!(view.settings.appearance.grid_min_width, 170);
        view.handle_grid_width_key(
            &KeyDownEvent {
                keystroke: Keystroke::parse("right").unwrap(),
                is_held: false,
                prefer_character_input: false,
            },
            cx,
        );
        assert_eq!(view.settings.appearance.grid_min_width, 180);
        view.handle_grid_width_key(
            &KeyDownEvent {
                keystroke: Keystroke::parse("home").unwrap(),
                is_held: false,
                prefer_character_input: false,
            },
            cx,
        );
        assert_eq!(view.settings.appearance.grid_min_width, 120);
        assert_eq!(view.overlay.toolbar_menu, ToolbarMenu::View);
    });

    remove_fixture(&directory);
}

#[gpui::test]
fn toolbar_history_popovers_jump_clear_and_persist_native_navigation(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let one = directory.join("one");
    let two = directory.join("two");
    let three = directory.join("three");
    for path in [&one, &two, &three] {
        fs::create_dir(path).unwrap();
    }
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(window, |view, cx| {
        assert!(view.browser.navigate(one.clone()));
        assert!(view.browser.navigate(two.clone()));
        assert!(view.browser.navigate(three.clone()));
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();

    let back = window.debug_bounds("back").unwrap().center();
    window.simulate_mouse_down(back, MouseButton::Right, gpui::Modifiers::default());
    window.simulate_mouse_up(back, MouseButton::Right, gpui::Modifiers::default());
    window.run_until_parked();
    let back_menu = window.debug_bounds("back-history-menu").unwrap();
    let back_button = window.debug_bounds("back").unwrap();
    assert!(
        (f32::from(back_menu.left()) - f32::from(back_button.left())).abs() <= 1.0,
        "back menu {back_menu:?} must align under button {back_button:?}"
    );
    assert!(
        back_menu.top() >= back_button.bottom() + px(2.0),
        "back menu {back_menu:?} must open below button {back_button:?}"
    );
    for selector in [
        "back-history-item-0",
        "back-history-item-1",
        "back-history-item-2",
    ] {
        assert!(window.debug_bounds(selector).is_some());
    }
    let older = window.debug_bounds("back-history-item-1").unwrap().center();
    window.simulate_click(older, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), one);
        assert!(view.browser.can_go_back());
        assert!(view.browser.can_go_forward());
        assert_eq!(view.overlay.toolbar_menu, ToolbarMenu::Closed);
    });

    let forward = window.debug_bounds("forward").unwrap().center();
    window.simulate_mouse_down(forward, MouseButton::Right, gpui::Modifiers::default());
    window.simulate_mouse_up(forward, MouseButton::Right, gpui::Modifiers::default());
    window.run_until_parked();
    let forward_menu = window.debug_bounds("forward-history-menu").unwrap();
    let forward_button = window.debug_bounds("forward").unwrap();
    assert!((f32::from(forward_menu.left()) - f32::from(forward_button.left())).abs() <= 1.0);
    assert!(forward_menu.top() >= forward_button.bottom() + px(2.0));
    assert!(window.debug_bounds("forward-history-item-0").is_some());
    assert!(window.debug_bounds("forward-history-item-1").is_some());
    let newest = window
        .debug_bounds("forward-history-item-1")
        .unwrap()
        .center();
    window.simulate_click(newest, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, cx| {
        assert_eq!(view.browser.path(), three);
        view.execute_command(CommandId::ClearHistory, cx);
        assert!(!view.browser.can_go_back());
        assert!(!view.browser.can_go_forward());
        assert_eq!(
            view.toasts
                .current
                .as_ref()
                .map(|toast| toast.message.as_str()),
            Some("Navigation history cleared")
        );
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn list_custom_columns_render_sort_and_reflow_at_supported_window_sizes(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let mut alpha = absolute_entry(directory.join("alpha.txt"));
    alpha.custom = HashMap::from([
        ("status".to_string(), serde_json::json!("Done")),
        ("priority".to_string(), serde_json::json!(2)),
        ("type".to_string(), serde_json::json!("Document")),
        ("category".to_string(), serde_json::json!("Work")),
        ("tags".to_string(), serde_json::json!(["review"])),
    ]);
    let mut beta = absolute_entry(directory.join("beta.txt"));
    beta.custom = HashMap::from([
        ("status".to_string(), serde_json::json!("Blocked")),
        ("priority".to_string(), serde_json::json!(1)),
        ("type".to_string(), serde_json::json!("Code")),
        ("category".to_string(), serde_json::json!("Project")),
    ]);
    let mut gamma = absolute_entry(directory.join("gamma.txt"));
    gamma
        .custom
        .insert("priority".to_string(), serde_json::json!(3));

    let (view, window) = cx.add_window_view(|_, cx| {
        DirectoryWindow::new(directory.clone(), NativeServices::default(), cx)
    });
    view.update(window, |view, cx| {
        view.visuals.icon_loading_enabled = false;
        view.browser.replace_entries(vec![gamma, alpha, beta]);
        view.listing.state = ListingState::Ready;
        cx.notify();
    });

    window.simulate_resize(gpui::size(px(1040.0), px(720.0)));
    window.run_until_parked();
    for selector in [
        "sort-custom-category",
        "sort-custom-priority",
        "sort-custom-status",
        "sort-custom-type",
        "entry-0-custom-category",
        "entry-0-custom-priority",
        "entry-0-custom-status",
        "entry-0-custom-type",
    ] {
        assert!(
            window.debug_bounds(selector).is_some(),
            "missing custom-column surface {selector}"
        );
    }
    assert!(window.debug_bounds("sort-custom-tags").is_none());
    let narrow_width = f32::from(
        window
            .debug_bounds("sort-custom-status")
            .unwrap()
            .size
            .width,
    );
    assert!((48.0..=160.0).contains(&narrow_width));
    assert_eq!(
        window
            .debug_bounds("sort-custom-status")
            .unwrap()
            .size
            .width,
        window
            .debug_bounds("entry-0-custom-status")
            .unwrap()
            .size
            .width
    );

    let status_header = window.debug_bounds("sort-custom-status").unwrap().center();
    window.simulate_click(status_header, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(
            view.browser.sort_key(),
            SortKey::Custom("status".to_string())
        );
        assert_eq!(
            view.browser
                .visible_entries()
                .iter()
                .map(|entry| file_name(entry))
                .collect::<Vec<_>>(),
            ["beta.txt", "alpha.txt", "gamma.txt"]
        );
    });
    window.simulate_click(status_header, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.sort_direction(), SortDirection::Descending);
        assert_eq!(
            view.browser
                .visible_entries()
                .iter()
                .map(|entry| file_name(entry))
                .collect::<Vec<_>>(),
            ["gamma.txt", "alpha.txt", "beta.txt"]
        );
    });

    window.simulate_resize(gpui::size(px(1200.0), px(720.0)));
    window.run_until_parked();
    let wide_width = f32::from(
        window
            .debug_bounds("sort-custom-status")
            .unwrap()
            .size
            .width,
    );
    assert!(wide_width > narrow_width);
    assert!(wide_width <= 160.0);
    let wide_name_width = f32::from(window.debug_bounds("sort-name").unwrap().size.width);
    assert!(
        wide_name_width >= 170.0,
        "wide custom-column layouts must preserve the legacy-priority Name column: {wide_name_width}"
    );

    remove_fixture(&directory);
}

#[test]
fn responsive_grid_metrics_match_legacy_width_height_and_gap_rules() {
    let narrow = grid_layout_metrics(610.0, 140, Density::Comfortable, 1.0);
    assert_eq!(narrow.columns, 4);
    assert_eq!(narrow.item_width, 140.0);
    // Thumbnail, two 23px name lines, a 19px detail line, two 4px gaps, the
    // border and 8px of breathing room.
    assert_eq!(narrow.item_height, 155.0);
    assert_eq!(narrow.gap, 12.0);
    assert_eq!(narrow.row_height, 167.0);

    // Wide cards keep the legacy aspect ratio once it exceeds the content.
    let roomy = grid_layout_metrics(1_010.0, 240, Density::Comfortable, 1.0);
    assert_eq!(roomy.item_height, 204.0);

    // Larger UI scales grow the card with its text.
    let scaled = grid_layout_metrics(1_010.0, 140, Density::Comfortable, 1.4);
    assert!(scaled.item_height > narrow.item_height * 1.3);

    let wide = grid_layout_metrics(1_010.0, 140, Density::Comfortable, 1.0);
    assert_eq!(wide.columns, 6);
    let compact_scaled = grid_layout_metrics(1_010.0, 140, Density::Compact, 1.4);
    assert_eq!(compact_scaled.gap, 11.0);
    assert_eq!(compact_scaled.columns, 6);
    assert_eq!(
        grid_layout_metrics(80.0, 260, Density::Compact, 1.0).columns,
        1
    );
}

#[test]
fn grid_marquee_geometry_uses_inclusive_card_intersection_and_edge_scroll_zones() {
    let metrics = grid_layout_metrics(304.0, 140, Density::Comfortable, 1.0);
    assert_eq!(metrics.columns, 2);
    assert_eq!(
        grid_marquee_hit_indices(
            GridRect::between(
                GridPoint { x: 0.0, y: 0.0 },
                GridPoint { x: 160.0, y: 100.0 },
            ),
            6,
            metrics,
        ),
        vec![0, 1],
        "touching the second card edge counts as an intersection"
    );
    assert_eq!(
        grid_marquee_hit_indices(
            GridRect::between(
                GridPoint {
                    x: 8.0,
                    y: metrics.item_height,
                },
                GridPoint {
                    x: 148.0,
                    y: metrics.row_height,
                },
            ),
            6,
            metrics,
        ),
        vec![0, 2],
        "the legacy inclusive AABB rule includes both touching rows"
    );
    assert_eq!(selection_marquee_scroll_delta(39.0, 500.0), 10.0);
    assert_eq!(selection_marquee_scroll_delta(250.0, 500.0), 0.0);
    assert_eq!(selection_marquee_scroll_delta(461.0, 500.0), -10.0);

    assert_eq!(
        row_marquee_hit_indices(
            GridRect::between(
                GridPoint { x: 0.0, y: 31.0 },
                GridPoint { x: 200.0, y: 65.0 },
            ),
            5,
            32.0,
            280.0,
        ),
        vec![0, 1, 2],
        "row marquee uses the same inclusive intersection rule"
    );

    let mut marquee = SelectionMarquee {
        start_window: GridPoint { x: 20.0, y: 20.0 },
        start_content: GridPoint { x: 20.0, y: 20.0 },
        current_content: GridPoint { x: 20.0, y: 20.0 },
        activated: false,
        additive: false,
        layout: MarqueeLayout::Grid(metrics),
        initial_selection: BTreeSet::new(),
        paths: Vec::new(),
    };
    marquee.update(
        GridPoint { x: 24.0, y: 24.0 },
        GridPoint { x: 24.0, y: 24.0 },
    );
    assert!(!marquee.activated);
    marquee.update(
        GridPoint { x: 25.0, y: 24.0 },
        GridPoint { x: 25.0, y: 24.0 },
    );
    assert!(marquee.activated);
}

#[test]
fn grid_thumbnail_matrix_matches_legacy_and_keys_track_source_identity() {
    for extension in [
        "png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "svgz", "tif", "tiff", "ico", "tga",
        "dds", "hdr", "pnm", "pbm", "pgm", "ppm", "pam", "qoi", "mp4", "webm", "m4v", "mov", "avi",
        "mkv", "wmv", "flv", "m2ts", "mts", "mpeg", "mpg", "3gp", "glb", "gltf", "obj", "stl",
        "ply", "3mf", "fbx",
    ] {
        let entry = absolute_entry(PathBuf::from(format!("media.{extension}")));
        assert!(
            entry_supports_grid_thumbnail(&entry),
            "{extension} should use a Grid thumbnail"
        );
    }
    for extension in ["txt", "key", "zip"] {
        let entry = absolute_entry(PathBuf::from(format!("other.{extension}")));
        assert!(
            !entry_supports_grid_thumbnail(&entry),
            "{extension} should retain its icon"
        );
    }
    // Quick Look thumbnails these on macOS; elsewhere they keep their icon.
    for extension in ["heic", "psd", "pdf", "docx", "pages", "otf", "usdz"] {
        let entry = absolute_entry(PathBuf::from(format!("document.{extension}")));
        assert_eq!(
            entry_supports_grid_thumbnail(&entry),
            cfg!(target_os = "macos"),
            "{extension}"
        );
    }

    let mut entry = absolute_entry(PathBuf::from("photo.png"));
    let original = EntryThumbnailKey::for_entry(&entry, 256);
    entry.size = 42;
    assert_ne!(original, EntryThumbnailKey::for_entry(&entry, 256));
    entry.size = 0;
    entry.modified = UNIX_EPOCH + Duration::from_secs(1);
    assert_ne!(original, EntryThumbnailKey::for_entry(&entry, 256));
    entry.modified = UNIX_EPOCH;
    assert_ne!(original, EntryThumbnailKey::for_entry(&entry, 512));

    entry.is_symlink = true;
    assert!(!entry_supports_grid_thumbnail(&entry));
    entry.is_symlink = false;
    entry.is_junction = true;
    assert!(!entry_supports_grid_thumbnail(&entry));
    entry.is_junction = false;
    entry.is_cloud_placeholder = true;
    assert!(
        !entry_supports_grid_thumbnail(&entry),
        "rendering a cloud placeholder would download it"
    );
    entry.is_cloud_placeholder = false;
    entry.is_dir = true;
    assert!(!entry_supports_grid_thumbnail(&entry));
}

#[cfg(target_os = "macos")]
#[test]
fn macos_icon_keys_give_packages_aliases_and_special_folders_their_own_icon() {
    let source_specific =
        |entry: &FileEntry| matches!(EntryIconKey::for_entry(entry), EntryIconKey::Source { .. });
    let mut app = absolute_entry(PathBuf::from("/Applications/Safari.app"));
    app.is_dir = true;
    app.is_package = true;
    assert!(source_specific(&app));

    let home = dirs::home_dir().unwrap();
    let mut downloads = absolute_entry(home.join("Downloads"));
    downloads.is_dir = true;
    assert!(source_specific(&downloads));
    let mut project = absolute_entry(home.join("Downloads/project"));
    project.is_dir = true;
    assert_eq!(EntryIconKey::for_entry(&project), EntryIconKey::Folder);

    let mut alias = absolute_entry(PathBuf::from("/tmp/Report alias"));
    alias.has_xattrs = true;
    assert!(source_specific(&alias));

    // Windows-only icon sources share their kind's icon on macOS.
    let tool = absolute_entry(PathBuf::from("/tmp/tool.exe"));
    assert_eq!(
        EntryIconKey::for_entry(&tool),
        EntryIconKey::FileKind("exe".to_string())
    );
}

#[gpui::test]
fn grid_thumbnails_render_real_image_and_video_with_recoverable_failure(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let image_path = directory.join("photo.png");
    let malformed_path = directory.join("malformed.png");
    let video_path = directory.join("clip.mp4");
    write_test_png(&image_path, 320, 180, [48, 132, 224, 255]);
    fs::write(&malformed_path, b"not an image").unwrap();

    let ffmpeg_available = std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if ffmpeg_available {
        let generated = std::process::Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x90:rate=15",
                "-t",
                "1",
                "-c:v",
                "mpeg4",
                "-pix_fmt",
                "yuv420p",
                "-y",
            ])
            .arg(&video_path)
            .status()
            .unwrap();
        assert!(generated.success());
    }

    let mut entries = vec![
        metadata_entry(image_path.clone()),
        metadata_entry(malformed_path.clone()),
    ];
    if ffmpeg_available {
        entries.push(metadata_entry(video_path.clone()));
    }
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(cx, |view, cx| {
        view.visuals.thumbnail_loading_enabled = true;
        view.browser.set_view_mode(ViewMode::Grid);
        view.browser.replace_entries(entries);
        view.listing.state = ListingState::Ready;
        cx.notify();
    });

    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            view.visuals.thumbnail_active == 0
                && view.visuals.thumbnail_queue.is_empty()
                && view
                    .visuals
                    .thumbnails
                    .values()
                    .all(|state| !matches!(state, EntryThumbnailState::Loading))
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let cached_image = view.update(cx, |view, _| {
        assert_eq!(view.visuals.thumbnail_active, 0);
        assert!(view.visuals.thumbnail_queue.is_empty());
        assert!(view.visuals.thumbnails.len() <= ENTRY_THUMBNAIL_CACHE_LIMIT);
        let state_for = |path: &Path| {
            view.visuals
                .thumbnails
                .iter()
                .find(|(key, _)| key.path == path)
                .map(|(_, state)| state.clone())
                .expect("thumbnail state")
        };
        let EntryThumbnailState::Ready(cached_image) = state_for(&image_path) else {
            panic!("real PNG should render a thumbnail");
        };
        assert!(cached_image.is_file());
        assert_eq!(state_for(&malformed_path), EntryThumbnailState::Failed);
        if ffmpeg_available {
            assert!(matches!(
                state_for(&video_path),
                EntryThumbnailState::Ready(path) if path.is_file()
            ));
        }
        cached_image
    });

    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    let failed_card = cx
        .debug_bounds("thumbnail-failed-card")
        .expect("failed thumbnail card");
    let failed_primary = cx
        .debug_bounds("thumbnail-failed-primary")
        .expect("failed thumbnail first line");
    let failed_secondary = cx
        .debug_bounds("thumbnail-failed-secondary")
        .expect("failed thumbnail second line");
    assert!(failed_card.contains(&failed_primary.center()));
    assert!(failed_card.contains(&failed_secondary.center()));
    assert!(failed_primary.center().y < failed_secondary.center().y);
    view.update(cx, |view, _| {
        assert!(view.visuals.thumbnail_queue.is_empty());
        assert!(view.visuals.thumbnails.iter().any(|(key, state)| {
            key.path == image_path
                && matches!(state, EntryThumbnailState::Ready(path) if path == &cached_image)
        }));
    });

    remove_fixture(&directory);
}

#[gpui::test]
fn grid_thumbnail_cache_holds_every_visible_cell_without_reloading(cx: &mut TestAppContext) {
    let resources = fixture_dir();
    let entries: Vec<_> = (0..600)
        .map(|index| absolute_entry(PathBuf::from("fixture").join(format!("photo-{index}.png"))))
        .collect();
    let services = NativeServices::new(ResourcePaths::test(&resources));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(PathBuf::from("fixture"), services, cx));
    view.update(cx, |view, cx| {
        view.visuals.thumbnail_loading_enabled = true;
        view.settings.appearance.grid_min_width = 120;
        view.browser.set_view_mode(ViewMode::Grid);
        view.browser.replace_entries(entries);
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    let size = gpui::size(px(2400.0), px(1600.0));
    cx.simulate_resize(size);
    let draw = |cx: &mut gpui::VisualTestContext| {
        cx.draw(gpui::point(px(0.0), px(0.0)), size, |_, _| {
            view.clone().into_element()
        });
    };
    draw(cx);
    let (visible, cached) = view.update(cx, |view, _| {
        let visible = view.last_rendered_items;
        assert!(
            visible > ENTRY_THUMBNAIL_CACHE_LIMIT,
            "the grid must show more cells ({visible}) than the minimum cache size"
        );
        assert!(view.visuals.thumbnails.capacity() >= visible * 2);
        // Every visible cell keeps its entry: none was evicted to make room.
        assert!(view.visuals.thumbnails.len() >= visible);
        (visible, view.visuals.thumbnails.len())
    });
    // Redrawing the same cells finds them all cached: nothing is re-queued.
    draw(cx);
    view.update(cx, |view, _| {
        assert_eq!(view.last_rendered_items, visible);
        assert_eq!(view.visuals.thumbnails.len(), cached);
        assert!(view.visuals.thumbnail_queue.len() + view.visuals.thumbnail_active <= cached);
        // Loads that finish after this are discarded without redrawing.
        view.clear_entry_visuals();
    });
    remove_fixture(&resources);
}

#[gpui::test]
fn grid_resize_reflows_columns_and_keyboard_navigation_uses_live_rows(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let entries = (0..20)
        .map(|index| absolute_entry(directory.join(format!("{index:02}.txt"))))
        .collect();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(cx, |view, cx| {
        view.browser.set_view_mode(ViewMode::Grid);
        view.browser.replace_entries(entries);
        view.listing.state = ListingState::Ready;
        cx.notify();
    });

    let window = cx.windows()[0];
    cx.simulate_window_resize(window, gpui::size(px(800.0), px(600.0)));
    cx.run_until_parked();
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    view.update(cx, |view, cx| {
        assert_eq!(view.layout.grid_columns, 3);
        view.select_next(cx);
        view.select_next(cx);
        assert_eq!(
            view.browser.selected_path(),
            Some(directory.join("03.txt").as_path())
        );
    });

    cx.simulate_window_resize(window, gpui::size(px(1200.0), px(720.0)));
    cx.run_until_parked();
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    view.update(cx, |view, cx| {
        assert_eq!(view.layout.grid_columns, 6);
        view.select_next(cx);
        assert_eq!(
            view.browser.selected_path(),
            Some(directory.join("09.txt").as_path())
        );
        view.column_left(cx);
        assert_eq!(
            view.browser.selected_path(),
            Some(directory.join("08.txt").as_path())
        );
        view.column_right(cx);
        assert_eq!(
            view.browser.selected_path(),
            Some(directory.join("09.txt").as_path())
        );
        view.select_next_range(cx);
        assert_eq!(
            view.browser.selected_path(),
            Some(directory.join("15.txt").as_path())
        );
        assert_eq!(view.browser.selection_count(), 7);

        view.settings.appearance.grid_min_width = 260;
        cx.notify();
    });
    cx.run_until_parked();
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    view.update(cx, |view, _| assert_eq!(view.layout.grid_columns, 3));

    remove_fixture(&directory);
}

#[gpui::test]
fn grid_pointer_marquee_selects_adds_clears_and_cancels_on_resize(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let entries: Vec<_> = (0..20)
        .map(|index| absolute_entry(directory.join(format!("{index:02}.txt"))))
        .collect();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(window, |view, cx| {
        view.browser.set_view_mode(ViewMode::Grid);
        view.browser.replace_entries(entries);
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();

    let surface = window
        .debug_bounds("grid-marquee-surface")
        .expect("rendered Grid marquee surface");
    let start = gpui::point(surface.left() + px(2.0), surface.top() + px(60.0));
    let end = gpui::point(surface.left() + px(300.0), surface.top() + px(100.0));
    window.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_move(end, Some(MouseButton::Left), gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert!(
            view.pointer
                .selection_marquee
                .as_ref()
                .is_some_and(|drag| drag.activated)
        );
        assert_eq!(view.browser.selection_count(), 2);
        assert!(view.browser.is_selected(&directory.join("00.txt")));
        assert!(view.browser.is_selected(&directory.join("01.txt")));
    });
    window.run_until_parked();
    assert!(window.debug_bounds("selection-marquee-overlay").is_some());
    window.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert!(view.pointer.selection_marquee.is_none());
        assert_eq!(view.browser.selection_count(), 2);
    });

    view.update(window, |view, cx| {
        view.browser.select(directory.join("02.txt"));
        cx.notify();
    });
    let additive = gpui::Modifiers {
        control: true,
        ..gpui::Modifiers::default()
    };
    window.simulate_mouse_down(start, MouseButton::Left, additive);
    window.simulate_mouse_move(end, Some(MouseButton::Left), additive);
    window.simulate_mouse_up(end, MouseButton::Left, additive);
    view.update(window, |view, _| {
        assert_eq!(view.browser.selection_count(), 3);
        assert!(view.browser.is_selected(&directory.join("02.txt")));
    });

    window.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_up(start, MouseButton::Left, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(view.browser.selection_count(), 0);
        assert!(view.pointer.selection_marquee.is_none());
    });

    let scroll_handle = view.update(window, |view, _| {
        view.listing.scroll_handle.0.borrow().base_handle.clone()
    });
    scroll_handle.set_offset(gpui::point(px(0.0), px(0.0)));
    let edge_start = gpui::point(surface.left() + px(2.0), surface.top() + px(100.0));
    let edge_end = gpui::point(surface.left() + px(2.0), surface.bottom() - px(2.0));
    window.simulate_mouse_down(edge_start, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_move(
        edge_end,
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    assert_eq!(scroll_handle.offset().y, px(-10.0));
    window.simulate_mouse_up(edge_end, MouseButton::Left, gpui::Modifiers::default());
    scroll_handle.set_offset(gpui::point(px(0.0), px(0.0)));

    window.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_move(
        gpui::point(surface.left() + px(100.0), surface.top() + px(100.0)),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    view.update(window, |view, _| {
        assert!(view.pointer.selection_marquee.is_some())
    });
    window.simulate_resize(gpui::size(px(1200.0), px(720.0)));
    window.run_until_parked();
    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    view.update(window, |view, _| {
        assert_eq!(view.layout.grid_columns, 6);
        assert!(view.pointer.selection_marquee.is_none());
    });

    remove_fixture(&directory);
}

#[gpui::test]
fn favorite_keyboard_and_pointer_reordering_share_persistent_order(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("alt-up", GoUp, Some("browser")),
            KeyBinding::new("alt-up", MoveFavoriteUp, Some("favorite")),
            KeyBinding::new("alt-down", MoveFavoriteDown, Some("favorite")),
        ]);
    });
    let directory = fixture_dir();
    let first = directory.join("first");
    let second = directory.join("second");
    let third = directory.join("third");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    fs::create_dir(&third).unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(window, |view, cx| {
        assert!(view.browser.toggle_favorite(first.clone()));
        assert!(view.browser.toggle_favorite(second.clone()));
        assert!(view.browser.toggle_favorite(third.clone()));
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    window.run_until_parked();

    let first_focus = view.update(window, |view, _| view.favorite_focus_handles[0].clone());
    window.update(|window, cx| window.focus(&first_focus, cx));
    window.simulate_keystrokes("alt-down");
    view.update(window, |view, _| {
        assert_eq!(view.browser.favorites()[0].path(), second.as_path());
        assert_eq!(view.browser.favorites()[1].path(), first.as_path());
    });
    let moved_focus = view.update(window, |view, _| view.favorite_focus_handles[1].clone());
    window.update(|window, _| assert!(moved_focus.is_focused(window)));

    window.simulate_keystrokes("alt-up");
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), directory.as_path());
        assert_eq!(view.browser.favorites()[0].path(), first.as_path());
        assert_eq!(view.browser.favorites()[1].path(), second.as_path());
    });
    window.run_until_parked();

    let source = window
        .debug_bounds("favorite-0")
        .expect("first favorite bounds")
        .center();
    let target = window
        .debug_bounds("favorite-2")
        .expect("third favorite bounds")
        .center();
    window.simulate_mouse_down(source, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_move(target, Some(MouseButton::Left), gpui::Modifiers::default());
    window.simulate_mouse_up(target, MouseButton::Left, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(view.browser.favorites()[0].path(), second.as_path());
        assert_eq!(view.browser.favorites()[1].path(), third.as_path());
        assert_eq!(view.browser.favorites()[2].path(), first.as_path());
    });

    remove_fixture(&directory);
}

#[gpui::test]
fn listing_views_share_bounded_native_icon_loading_and_keep_fallbacks(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let folder = directory.join("folder");
    let first_text = directory.join("first.txt");
    let second_text = directory.join("second.txt");
    let executable = directory.join("tool.exe");
    fs::create_dir(&folder).unwrap();
    fs::write(&first_text, "one").unwrap();
    fs::write(&second_text, "two").unwrap();
    fs::write(&executable, "not a real executable").unwrap();

    let mut entries = vec![
        absolute_entry(folder),
        absolute_entry(first_text),
        absolute_entry(second_text),
        absolute_entry(executable),
    ];
    entries[0].is_dir = true;
    entries[1].size = 3;
    entries[2].size = 3;
    entries[3].size = 21;
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(cx, |view, cx| {
        view.visuals.icon_loading_enabled = true;
        view.browser.replace_entries(entries.clone());
        view.listing.state = ListingState::Ready;
        cx.notify();
    });

    for _ in 0..2 {
        cx.draw(
            gpui::point(px(0.0), px(0.0)),
            gpui::size(px(800.0), px(600.0)),
            |_, _| view.clone().into_element(),
        );
    }
    let icon_loading_deadline = Instant::now() + Duration::from_secs(35);
    while Instant::now() < icon_loading_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            view.visuals.icon_active == 0
                && view.visuals.icon_queue.is_empty()
                && view
                    .visuals
                    .icons
                    .values()
                    .all(|icon| matches!(icon, EntryIconState::Ready(_)))
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    view.update(cx, |view, _| {
        assert_eq!(view.visuals.icons.len(), 3, "text icons should deduplicate");
        assert!(view.visuals.icons.len() <= ENTRY_ICON_CACHE_LIMIT);
        assert!(
            view.visuals.icons.values().all(|icon| match icon {
                EntryIconState::Ready(Some(path)) => path.is_file(),
                EntryIconState::Ready(None) => true,
                EntryIconState::Loading => false,
            }),
            "icon loading should finish with a valid native artifact or fallback"
        );
    });

    view.update(cx, |view, cx| {
        view.browser.set_view_mode(ViewMode::Grid);
        cx.notify();
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    view.update(cx, |view, cx| {
        view.browser.set_view_mode(ViewMode::Column);
        view.column_view.columns = ColumnState::new(&directory);
        assert!(view.column_view.columns.apply_listed(&directory, entries));
        cx.notify();
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    view.update(cx, |view, _| assert_eq!(view.visuals.icons.len(), 3));

    remove_fixture(&directory);
}

#[test]
fn fallback_file_icon_pack_covers_every_semantic_family() {
    let cases = [
        ("photo.jpeg", FallbackFileIconKind::Image, "IMG"),
        ("song.flac", FallbackFileIconKind::Audio, "AUD"),
        ("movie.mov", FallbackFileIconKind::Video, "VID"),
        ("bundle.7z", FallbackFileIconKind::Archive, "ZIP"),
        ("source.rs", FallbackFileIconKind::Code, "DEV"),
        ("manual.pdf", FallbackFileIconKind::Pdf, "PDF"),
        ("letter.docx", FallbackFileIconKind::Document, "DOC"),
        ("budget.xlsx", FallbackFileIconKind::Spreadsheet, "XLS"),
        ("deck.pptx", FallbackFileIconKind::Presentation, "PPT"),
        ("notes.txt", FallbackFileIconKind::Generic, "TXT"),
        ("LICENSE", FallbackFileIconKind::Generic, "FILE"),
    ];

    for (name, expected_kind, expected_label) in cases {
        let spec = fallback_file_icon_spec(Path::new(name), true);
        assert_eq!(spec.kind, expected_kind, "wrong icon family for {name}");
        assert_eq!(spec.label, expected_label, "wrong icon label for {name}");
    }
}

#[gpui::test]
fn fallback_icon_pack_renders_every_family_in_the_listing(cx: &mut TestAppContext) {
    let root = PathBuf::from("fallback-icon-pack");
    let mut entries = vec![
        absolute_entry(root.join("folder")),
        absolute_entry(root.join("photo.jpeg")),
        absolute_entry(root.join("song.flac")),
        absolute_entry(root.join("movie.mov")),
        absolute_entry(root.join("bundle.7z")),
        absolute_entry(root.join("source.rs")),
        absolute_entry(root.join("manual.pdf")),
        absolute_entry(root.join("letter.docx")),
        absolute_entry(root.join("budget.xlsx")),
        absolute_entry(root.join("deck.pptx")),
        absolute_entry(root.join("notes.txt")),
    ];
    entries[0].is_dir = true;

    let (view, window) = cx.add_window_view(|window, cx| {
        let mut view = DirectoryWindow::new(root, NativeServices::default(), cx);
        view.visuals.icon_loading_enabled = false;
        view.browser.replace_entries(entries);
        view.listing.state = ListingState::Ready;
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(900.0), px(720.0)));
    window.run_until_parked();

    for (family, selector) in [
        ("folder", "fallback-icon-folder"),
        ("image", "fallback-icon-image"),
        ("audio", "fallback-icon-audio"),
        ("video", "fallback-icon-video"),
        ("archive", "fallback-icon-archive"),
        ("code", "fallback-icon-code"),
        ("pdf", "fallback-icon-pdf"),
        ("document", "fallback-icon-document"),
        ("spreadsheet", "fallback-icon-spreadsheet"),
        ("presentation", "fallback-icon-presentation"),
        ("generic", "fallback-icon-generic"),
    ] {
        assert!(
            window.debug_bounds(selector).is_some(),
            "missing rendered fallback icon family {family}"
        );
    }

    drop(view);
}

#[gpui::test]
fn restored_column_mode_starts_column_listings_instead_of_list_loading(cx: &mut TestAppContext) {
    let relative_root = PathBuf::from(format!(
        "explorie-column-start-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    let relative = (1..=7).fold(relative_root.clone(), |path, index| {
        path.join(format!("level-{index}"))
    });
    fs::create_dir_all(&relative).unwrap();
    fs::write(relative.join("visible.txt"), "visible").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&relative_root));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(relative.clone(), services, cx));
    view.update(cx, |view, cx| {
        view.browser.set_view_mode(ViewMode::Column);
        view.start_listing(cx);
    });

    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            view.column_view
                .columns
                .columns()
                .last()
                .is_some_and(|column| {
                    !column.loading()
                        && column
                            .visible_entries(&view.browser)
                            .iter()
                            .any(|entry| entry.path.ends_with("visible.txt"))
                })
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    view.update(cx, |view, _| {
        let column = view.column_view.columns.columns().last().unwrap();
        assert!(!column.loading());
        assert!(
            column
                .visible_entries(&view.browser)
                .iter()
                .any(|entry| entry.path.ends_with("visible.txt"))
        );
    });
    for _ in 0..2 {
        cx.draw(
            gpui::point(px(0.0), px(0.0)),
            gpui::size(px(800.0), px(600.0)),
            |_, _| view.clone().into_element(),
        );
    }
    view.update(cx, |view, _| {
            let offset = view.column_view.strip_scroll.offset();
            let max_offset = view.column_view.strip_scroll.max_offset();
            let bounds = view.column_view.strip_scroll.bounds();
            let first = view.column_view.strip_scroll.bounds_for_item(0);
            let last = view
                .column_view.strip_scroll
                .bounds_for_item(view.column_view.columns.columns().len() - 1);
            assert!(
                offset.x < px(0.0),
                "restored Column view should reveal its active leaf: offset={offset:?}, max={max_offset:?}, bounds={bounds:?}, first={first:?}, last={last:?}, attempts={}",
                view.column_view.scroll_to_leaf_attempts
            );
        });
    remove_fixture(&relative_root);
}

#[gpui::test]
fn hundred_thousand_selected_entries_render_without_building_drag_payloads(
    cx: &mut TestAppContext,
) {
    let entries: Vec<_> = (0..100_000)
        .map(|index| FileEntry {
            id: Uuid::new_v4(),
            path: PathBuf::from("fixture").join(format!("file-{index:05}.txt")),
            size: index,
            modified: SystemTime::UNIX_EPOCH,
            hidden: false,
            is_dir: false,
            custom: std::collections::HashMap::new(),
            is_symlink: false,
            is_junction: false,
            link_target: None,
            has_xattrs: false,
            is_package: false,
            link_target_is_dir: false,
            is_cloud_placeholder: false,
            tags: Vec::new(),
        })
        .collect();
    let (view, cx) = cx.add_window_view(|_, cx| {
        DirectoryWindow::new(PathBuf::from("fixture"), NativeServices::default(), cx)
    });
    view.update(cx, |view, cx| {
        view.browser.replace_entries(entries);
        view.browser.select_all();
        view.browser.set_sort(SortKey::Size);
        assert_eq!(view.browser.selection_count(), 100_000);
        view.listing.state = ListingState::Ready;
        cx.notify();
    });

    for mode in [ViewMode::List, ViewMode::Grid] {
        view.update(cx, |view, cx| {
            view.browser.set_view_mode(mode);
            cx.notify();
        });
        MULTI_FILE_DRAG_BUILDS.with(|count| count.set(0));
        cx.draw(
            gpui::point(px(0.0), px(0.0)),
            gpui::size(px(800.0), px(600.0)),
            |_, _| view.clone().into_element(),
        );
        view.update(cx, |view, _| {
            assert_eq!(view.browser.selection_count(), 100_000);
            assert!(
                (1..200).contains(&view.last_rendered_items),
                "{mode:?} rendered {} items",
                view.last_rendered_items
            );
        });
        assert_eq!(MULTI_FILE_DRAG_BUILDS.with(Cell::get), 0);
    }
}

#[test]
fn window_contract_is_explicit() {
    assert_eq!(DEFAULT_WINDOW_WIDTH, 1024.0);
    assert_eq!(DEFAULT_WINDOW_HEIGHT, 768.0);
    assert_eq!(MIN_WINDOW_WIDTH, 800.0);
    assert_eq!(MIN_WINDOW_HEIGHT, 600.0);
    assert_eq!(APP_IDENTIFIER, "com.omershatz.explorie");
}

#[gpui::test]
#[ignore = "explicit large-selection native render benchmark"]
fn records_large_selection_render_baselines(cx: &mut TestAppContext) {
    for count in [10_000, 100_000] {
        for mode in [ViewMode::List, ViewMode::Grid] {
            let entries = (0..count)
                .map(|index| {
                    absolute_entry(PathBuf::from("fixture").join(format!("file-{index:06}.txt")))
                })
                .collect();
            let (view, window) = cx.add_window_view(|_, cx| {
                DirectoryWindow::new(PathBuf::from("fixture"), NativeServices::default(), cx)
            });
            view.update(window, |view, cx| {
                view.browser.replace_entries(entries);
                view.browser.set_view_mode(mode);
                view.browser.select_all();
                view.listing.state = ListingState::Ready;
                cx.notify();
            });
            MULTI_FILE_DRAG_BUILDS.with(|count| count.set(0));
            let started = Instant::now();
            window.draw(
                point(px(0.0), px(0.0)),
                gpui::size(px(800.0), px(600.0)),
                |_, _| view.clone().into_element(),
            );
            let first_draw = started.elapsed();
            let mut scroll_draws = Vec::new();
            for index in [100, 200, 300] {
                view.update(window, |view, cx| {
                    view.listing
                        .scroll_handle
                        .scroll_to_item_strict(index, ScrollStrategy::Top);
                    cx.notify();
                });
                let started = Instant::now();
                window.draw(
                    point(px(0.0), px(0.0)),
                    gpui::size(px(800.0), px(600.0)),
                    |_, _| view.clone().into_element(),
                );
                scroll_draws.push(started.elapsed());
                view.update(window, |view, _| {
                    assert!(view.listing.scroll_handle.0.borrow().base_handle.offset().y < px(0.0));
                });
            }
            view.update(window, |view, _| {
                assert_eq!(view.browser.selection_count(), count)
            });
            assert_eq!(MULTI_FILE_DRAG_BUILDS.with(Cell::get), 0);
            eprintln!(
                "{count} selected | {mode:?} | first draw {first_draw:.2?} | scroll draws {scroll_draws:.2?}"
            );

            // One selected row near the end: the status bar resolves the
            // selected entry on every frame.
            view.update(window, |view, cx| {
                view.browser
                    .select(PathBuf::from("fixture").join(format!("file-{:06}.txt", count - 1)));
                cx.notify();
            });
            let mut single_draws = Vec::new();
            for _ in 0..3 {
                view.update(window, |_, cx| cx.notify());
                let started = Instant::now();
                window.draw(
                    point(px(0.0), px(0.0)),
                    gpui::size(px(800.0), px(600.0)),
                    |_, _| view.clone().into_element(),
                );
                single_draws.push(started.elapsed());
            }
            eprintln!("{count} entries, 1 selected | {mode:?} | redraws {single_draws:.2?}");
        }
    }
}

/// Run with `cargo test --locked -p explorie-gpui records_column_view_render_baselines
/// --release -- --ignored --nocapture`. EXPLORIE_BENCH_COUNTS sets the entries per column.
#[gpui::test]
#[ignore = "explicit column-view native render benchmark"]
fn records_column_view_render_baselines(cx: &mut TestAppContext) {
    let counts =
        std::env::var("EXPLORIE_BENCH_COUNTS").unwrap_or_else(|_| "10000,100000".to_string());
    for count in counts
        .split(',')
        .map(|count| count.parse::<usize>().unwrap())
    {
        let root = PathBuf::from("fixture");
        let leaf = root.join("leaf");
        let mut parent_entries: Vec<_> = (0..count)
            .map(|index| {
                let mut entry = absolute_entry(root.join(format!("folder-{index}")));
                entry.is_dir = true;
                entry
            })
            .collect();
        let mut leaf_folder = absolute_entry(leaf.clone());
        leaf_folder.is_dir = true;
        parent_entries.push(leaf_folder);
        let leaf_entries: Vec<_> = (0..count)
            .map(|index| {
                let mut entry = absolute_entry(leaf.join(format!("file-{index}.txt")));
                entry.size = (index % 1_000) as u64;
                entry
            })
            .collect();
        let selected = leaf.join(format!("file-{}.txt", count - 1));
        let (view, window) = cx.add_window_view(|_, cx| {
            DirectoryWindow::new(leaf.clone(), NativeServices::default(), cx)
        });
        view.update(window, |view, cx| {
            view.browser.set_view_mode(ViewMode::Column);
            view.column_view.columns = ColumnState::new(&leaf);
            assert!(view.column_view.columns.apply_listed(&root, parent_entries));
            assert!(
                view.column_view
                    .columns
                    .apply_listed(&leaf, leaf_entries.clone())
            );
            view.column_view
                .scroll_handles
                .resize_with(2, UniformListScrollHandle::new);
            view.browser.replace_entries(leaf_entries);
            view.browser.select(selected.clone());
            view.sync_column_selection_from_browser();
            view.listing.state = ListingState::Ready;
            cx.notify();
        });
        let started = Instant::now();
        window.draw(
            point(px(0.0), px(0.0)),
            gpui::size(px(1200.0), px(800.0)),
            |_, _| view.clone().into_element(),
        );
        let first_draw = started.elapsed();
        let mut idle_draws = Vec::new();
        for _ in 0..3 {
            view.update(window, |_, cx| cx.notify());
            let started = Instant::now();
            window.draw(
                point(px(0.0), px(0.0)),
                gpui::size(px(1200.0), px(800.0)),
                |_, _| view.clone().into_element(),
            );
            idle_draws.push(started.elapsed());
        }
        let mut scroll_draws = Vec::new();
        for index in [100, 200, 300] {
            view.update(window, |view, cx| {
                view.column_view.scroll_handles[1]
                    .scroll_to_item_strict(index, ScrollStrategy::Top);
                cx.notify();
            });
            let started = Instant::now();
            window.draw(
                point(px(0.0), px(0.0)),
                gpui::size(px(1200.0), px(800.0)),
                |_, _| view.clone().into_element(),
            );
            scroll_draws.push(started.elapsed());
        }
        view.update(window, |view, _| {
            assert_eq!(view.effective_selected_paths(), vec![selected.clone()]);
        });
        eprintln!(
            "{count} per column | Column | first draw {first_draw:.2?} | idle redraws {idle_draws:.2?} | scroll draws {scroll_draws:.2?}"
        );
    }
}

#[test]
fn workspace_window_restore_preserves_valid_multi_monitor_geometry_and_recovers_offscreen() {
    let displays = [
        gpui::Bounds::new(
            gpui::point(px(-1920.0), px(0.0)),
            gpui::size(px(1920.0), px(1080.0)),
        ),
        gpui::Bounds::new(
            gpui::point(px(0.0), px(0.0)),
            gpui::size(px(1920.0), px(1080.0)),
        ),
    ];
    let current = gpui::Bounds::new(
        gpui::point(px(200.0), px(100.0)),
        gpui::size(px(1024.0), px(768.0)),
    );
    let secondary = constrain_workspace_bounds(
        WorkspaceWindowState {
            width: Some(1_000.0),
            height: Some(700.0),
            x: Some(-1_700.0),
            y: Some(100.0),
        },
        current,
        &displays,
    );
    assert_eq!(f32::from(secondary.origin.x), -1_700.0);
    assert_eq!(f32::from(secondary.origin.y), 100.0);
    assert_eq!(f32::from(secondary.size.width), 1_000.0);
    assert_eq!(f32::from(secondary.size.height), 700.0);

    let recovered = constrain_workspace_bounds(
        WorkspaceWindowState {
            width: Some(1_200.0),
            height: Some(700.0),
            x: Some(9_000.0),
            y: Some(9_000.0),
        },
        current,
        &displays,
    );
    assert_eq!(f32::from(recovered.origin.x), 720.0);
    assert_eq!(f32::from(recovered.origin.y), 380.0);
    assert_eq!(f32::from(recovered.size.width), 1_200.0);
    assert_eq!(f32::from(recovered.size.height), 700.0);
}

#[test]
fn file_drop_validation_matches_move_copy_and_directory_safety_contract() {
    let folder = FileDrag::from_entries(vec![FileEntry {
        id: Uuid::new_v4(),
        path: PathBuf::from("root/source/folder"),
        size: 0,
        modified: SystemTime::UNIX_EPOCH,
        hidden: false,
        is_dir: true,
        custom: HashMap::new(),
        is_symlink: false,
        is_junction: false,
        link_target: None,
        has_xattrs: false,
        is_package: false,
        link_target_is_dir: false,
        is_cloud_placeholder: false,
        tags: Vec::new(),
    }]);
    assert!(!valid_file_drop_target(
        Path::new("root/source"),
        &folder,
        FileOperationKind::Move
    ));
    assert!(valid_file_drop_target(
        Path::new("root/source"),
        &folder,
        FileOperationKind::Copy
    ));
    assert!(!valid_file_drop_target(
        Path::new("root/source/folder"),
        &folder,
        FileOperationKind::Move
    ));
    assert!(!valid_file_drop_target(
        Path::new("root/source/folder/nested"),
        &folder,
        FileOperationKind::Move
    ));
    assert!(valid_file_drop_target(
        Path::new("root/elsewhere"),
        &folder,
        FileOperationKind::Move
    ));

    let mixed = FileDrag::from_entries(vec![
        absolute_entry(PathBuf::from("root/source/one.txt")),
        absolute_entry(PathBuf::from("root/other/two.txt")),
    ]);
    assert!(valid_file_drop_target(
        Path::new("root/source"),
        &mixed,
        FileOperationKind::Move
    ));
    assert_eq!(file_drag_scroll_delta(35.0, 500.0), 16.0);
    assert_eq!(file_drag_scroll_delta(250.0, 500.0), 0.0);
    assert_eq!(file_drag_scroll_delta(465.0, 500.0), -16.0);
}

#[gpui::test]
fn file_drag_pointer_moves_copies_and_pins_through_native_targets(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let destination = directory.join("destination");
    let pinned = directory.join("pinned-folder");
    let move_source = directory.join("move.txt");
    let copy_source = directory.join("copy.txt");
    fs::create_dir(&destination).unwrap();
    fs::create_dir(&pinned).unwrap();
    fs::write(&move_source, "move").unwrap();
    fs::write(&copy_source, "copy").unwrap();

    let mut destination_entry = metadata_entry(destination.clone());
    destination_entry.is_dir = true;
    let mut pinned_entry = metadata_entry(pinned.clone());
    pinned_entry.is_dir = true;
    let entries = vec![
        destination_entry,
        pinned_entry,
        metadata_entry(copy_source.clone()),
        metadata_entry(move_source.clone()),
    ];
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(window, |view, cx| {
        view.browser.replace_entries(entries);
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();

    let destination_point = window
        .debug_bounds("entry-0")
        .expect("destination row")
        .center();
    let copy_point = window
        .debug_bounds("entry-2")
        .expect("copy source row")
        .center();
    let move_point = window
        .debug_bounds("entry-3")
        .expect("move source row")
        .center();

    window.simulate_mouse_down(move_point, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_move(
        gpui::point(move_point.x + px(12.0), move_point.y),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    window.simulate_mouse_move(
        destination_point,
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    view.update(window, |view, _| {
        assert_eq!(
            view.pointer.file_drag_sources,
            BTreeSet::from([move_source.clone()])
        );
    });
    window.simulate_mouse_up(
        destination_point,
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
    view.update(window, |view, _| {
        let request = view.operations.latest().unwrap().request();
        assert_eq!(request.kind, FileOperationKind::Move);
        assert_eq!(request.sources, vec![move_source.clone()]);
        assert_eq!(request.destination.as_deref(), Some(destination.as_path()));
        assert!(view.pointer.file_drag_sources.is_empty());
    });

    let copy_modifiers = gpui::Modifiers {
        control: true,
        ..gpui::Modifiers::default()
    };
    window.simulate_mouse_down(copy_point, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_move(
        gpui::point(copy_point.x + px(12.0), copy_point.y),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    window.simulate_mouse_move(destination_point, Some(MouseButton::Left), copy_modifiers);
    window.simulate_mouse_up(destination_point, MouseButton::Left, copy_modifiers);
    view.update(window, |view, _| {
        let request = view.operations.latest().unwrap().request();
        assert_eq!(request.kind, FileOperationKind::Copy);
        assert_eq!(request.sources, vec![copy_source.clone()]);
        assert_eq!(request.destination.as_deref(), Some(destination.as_path()));
    });

    // The onboarding invitation moves rows down; keep the next source visible
    // by minimizing floating operation history before starting another drag.
    let minimize = window.debug_bounds("minimize-operations").unwrap().center();
    window.simulate_click(minimize, gpui::Modifiers::default());
    window.run_until_parked();
    let pinned_point = window
        .debug_bounds("entry-1")
        .expect("pinned folder row")
        .center();
    let favorites_point = window
        .debug_bounds("favorites-file-drop-target")
        .expect("favorites drop target")
        .center();
    window.simulate_mouse_down(pinned_point, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_move(
        gpui::point(pinned_point.x + px(12.0), pinned_point.y),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    window.simulate_mouse_move(
        favorites_point,
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    window.simulate_mouse_up(
        favorites_point,
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
    view.update(window, |view, _| {
        assert!(view.browser.is_favorite(&pinned));
        assert_eq!(
            view.toasts.current.as_ref().map(|toast| toast.kind),
            Some(ToastKind::Success)
        );
    });

    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        if destination.join("move.txt").exists() && destination.join("copy.txt").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(destination.join("move.txt").is_file());
    assert!(destination.join("copy.txt").is_file());
    assert!(!move_source.exists());
    assert!(copy_source.is_file());
    remove_fixture(&directory);
}

#[gpui::test]
fn deferred_file_drag_preserves_mouse_down_selection_across_redraws(cx: &mut TestAppContext) {
    for mode in [ViewMode::List, ViewMode::Grid] {
        for copy in [false, true] {
            for selected_source in [false, true] {
                let directory = fixture_dir();
                let destination = directory.join("destination");
                fs::create_dir(&destination).unwrap();
                let alpha = directory.join("alpha.txt");
                let beta = directory.join("beta.txt");
                let solo = directory.join("solo.txt");
                for path in [&alpha, &beta, &solo] {
                    fs::write(path, "drag fixture").unwrap();
                }
                let mut folder = metadata_entry(destination.clone());
                folder.is_dir = true;
                let services = NativeServices::new(ResourcePaths::test(&directory));
                let (view, window) = cx
                    .add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
                view.update(window, |view, cx| {
                    view.browser.replace_entries(vec![
                        folder,
                        metadata_entry(alpha.clone()),
                        metadata_entry(beta.clone()),
                        metadata_entry(solo.clone()),
                    ]);
                    view.browser.set_view_mode(mode);
                    view.browser
                        .replace_selection([alpha.clone(), beta.clone()]);
                    view.listing.state = ListingState::Ready;
                    cx.notify();
                });
                MULTI_FILE_DRAG_BUILDS.with(|count| count.set(0));
                window.simulate_resize(gpui::size(px(800.0), px(600.0)));
                window.run_until_parked();
                assert_eq!(MULTI_FILE_DRAG_BUILDS.with(Cell::get), 0);

                let (source_selector, target_selector) = if mode == ViewMode::List {
                    (
                        if selected_source {
                            "entry-1"
                        } else {
                            "entry-3"
                        },
                        "entry-0",
                    )
                } else {
                    (
                        if selected_source {
                            "grid-entry-1"
                        } else {
                            "grid-entry-3"
                        },
                        "grid-entry-0",
                    )
                };
                let source = window.debug_bounds(source_selector).unwrap().center();
                let target = window.debug_bounds(target_selector).unwrap().center();
                window.simulate_mouse_down(source, MouseButton::Left, gpui::Modifiers::default());
                // A normal click still collapses selection; the drag must keep the pre-click set.
                view.update(window, |view, _| {
                    assert_eq!(view.browser.selection_count(), 1)
                });
                window.run_until_parked();
                assert_eq!(MULTI_FILE_DRAG_BUILDS.with(Cell::get), 0);
                let modifiers = gpui::Modifiers {
                    control: copy,
                    ..gpui::Modifiers::default()
                };
                window.simulate_mouse_move(
                    point(source.x + px(12.0), source.y),
                    Some(MouseButton::Left),
                    modifiers,
                );
                window.simulate_mouse_move(target, Some(MouseButton::Left), modifiers);
                assert_eq!(
                    MULTI_FILE_DRAG_BUILDS.with(Cell::get),
                    usize::from(selected_source)
                );
                window.simulate_mouse_up(target, MouseButton::Left, modifiers);
                let expected = if selected_source {
                    vec![alpha.clone(), beta.clone()]
                } else {
                    vec![solo.clone()]
                };
                view.update(window, |view, _| {
                    let request = view
                        .operations
                        .latest()
                        .expect("queued file drop")
                        .request();
                    assert_eq!(
                        request.kind,
                        if copy {
                            FileOperationKind::Copy
                        } else {
                            FileOperationKind::Move
                        }
                    );
                    assert_eq!(request.sources, expected);
                    assert_eq!(request.destination.as_deref(), Some(destination.as_path()));
                    assert!(view.pointer.file_drag_selection.is_none());
                });
                let poll_deadline = Instant::now() + POLL_TIMEOUT;
                while Instant::now() < poll_deadline {
                    window.run_until_parked();
                    if expected
                        .iter()
                        .all(|path| destination.join(path.file_name().unwrap()).exists())
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                for path in &expected {
                    assert_eq!(
                        fs::read_to_string(destination.join(path.file_name().unwrap())).unwrap(),
                        "drag fixture"
                    );
                    assert_eq!(path.exists(), copy);
                }
                remove_fixture(&directory);
            }
        }
    }
}

#[gpui::test]
fn file_drag_hover_spring_loads_folder_only_after_legacy_delay(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let destination = directory.join("destination");
    let source = directory.join("source.txt");
    fs::create_dir(&destination).unwrap();
    fs::write(&source, "hover").unwrap();
    let mut destination_entry = metadata_entry(destination.clone());
    destination_entry.is_dir = true;
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(window, |view, cx| {
        view.browser
            .replace_entries(vec![destination_entry, metadata_entry(source.clone())]);
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();
    let target = window.debug_bounds("entry-0").unwrap().center();
    let source_point = window.debug_bounds("entry-1").unwrap().center();
    window.simulate_mouse_down(source_point, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_move(
        gpui::point(source_point.x + px(12.0), source_point.y),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    window.simulate_mouse_move(target, Some(MouseButton::Left), gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(
            view.pointer.file_drag_hover_target,
            Some(FileDragHoverTarget::Folder(destination.clone()))
        );
    });

    window.executor().advance_clock(Duration::from_millis(699));
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), directory.as_path())
    });
    window.executor().advance_clock(Duration::from_millis(1));
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), destination.as_path())
    });
    window.simulate_mouse_up(target, MouseButton::Left, gpui::Modifiers::default());

    remove_fixture(&directory);
}

#[gpui::test]
fn file_drag_pointer_refuses_a_managed_remote_root_before_queueing(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let source = directory.join("source.txt");
    fs::write(&source, "remote safety").unwrap();
    let resources = ResourcePaths::test(&directory);
    let backend = Arc::new(FakeGpuiRemoteBackend::default());
    let services = NativeServices::with_remote_backend(
        resources,
        Arc::clone(&backend) as Arc<dyn RemoteDriveBackend>,
    );
    let profile = remote_profile(&Uuid::new_v4().to_string());
    let connected = services.remotes.connect_blocking(profile).unwrap();
    let remote_root = connected.mount_path.expect("managed mount path");
    let remotes = services.remotes.clone();
    let remote_id = connected.id;
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(window, |view, cx| {
        assert!(view.browser.navigate(remote_root.clone()));
        view.browser
            .replace_entries(vec![metadata_entry(source.clone())]);
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();

    let source_point = window.debug_bounds("entry-0").unwrap().center();
    let listing = window.debug_bounds("listing-drop-surface").unwrap();
    let drop_point = listing.center();
    window.simulate_mouse_down(source_point, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_move(
        gpui::point(source_point.x + px(12.0), source_point.y),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    window.simulate_mouse_move(
        drop_point,
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    window.simulate_mouse_up(drop_point, MouseButton::Left, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert!(view.operations.latest().is_none());
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|message| message.contains("managed remote-drive root"))
        );
    });
    assert!(source.is_file());
    remotes.disconnect_blocking(&remote_id, true).unwrap();
    remove_fixture(&directory);
}

#[gpui::test]
fn native_clipboard_copy_reaches_the_filesystem_and_operation_queue(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let source_dir = directory.join("source");
    let destination = directory.join("destination");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&destination).unwrap();
    let source = source_dir.join("proof.txt");
    fs::write(&source, "native operation proof").unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let subscription = services.subscribe_async();
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(destination.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.browser.replace_entries(vec![FileEntry {
            id: Uuid::new_v4(),
            path: source.clone(),
            size: 22,
            modified: SystemTime::UNIX_EPOCH,
            hidden: false,
            is_dir: false,
            custom: std::collections::HashMap::new(),
            is_symlink: false,
            is_junction: false,
            link_target: None,
            has_xattrs: false,
            is_package: false,
            link_target_is_dir: false,
            is_cloud_placeholder: false,
            tags: Vec::new(),
        }]);
        view.browser.select(source.clone());
        view.copy_selected(cx);
        view.paste(cx);
    });

    let mut completed = None;
    for _ in 0..20 {
        let event = pollster::block_on(subscription.next());
        if let ServiceEvent::FileOperation(event) = event {
            let is_completed = matches!(
                event.state,
                explorie_native_services::FileOperationState::Completed
            );
            view.update(cx, |view, cx| {
                view.apply_file_operation_event(event, cx);
            });
            if is_completed {
                completed = Some(());
                break;
            }
        }
    }

    assert_eq!(completed, Some(()));
    assert_eq!(
        fs::read_to_string(destination.join("proof.txt")).unwrap(),
        "native operation proof"
    );
    view.update(cx, |view, _| {
        assert_eq!(view.operations.active_count(), 0);
        assert_eq!(
            view.operations.latest().unwrap().status(),
            OperationStatus::Completed
        );
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn retry_control_copies_only_the_unresolved_batch_suffix(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let source_dir = directory.join("source");
    let destination = directory.join("destination");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&destination).unwrap();
    let first = source_dir.join("first.txt");
    let second = source_dir.join("second.txt");
    fs::write(&first, "first").unwrap();
    fs::write(&second, "second").unwrap();
    fs::write(destination.join("second.txt"), "existing").unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let subscription = services.subscribe_async();
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(destination.clone(), services, cx));
    let entry = |path: PathBuf| FileEntry {
        id: Uuid::new_v4(),
        path,
        size: 6,
        modified: SystemTime::UNIX_EPOCH,
        hidden: false,
        is_dir: false,
        custom: std::collections::HashMap::new(),
        is_symlink: false,
        is_junction: false,
        link_target: None,
        has_xattrs: false,
        is_package: false,
        link_target_is_dir: false,
        is_cloud_placeholder: false,
        tags: Vec::new(),
    };

    view.update(cx, |view, cx| {
        view.browser
            .replace_entries(vec![entry(first.clone()), entry(second.clone())]);
        view.browser.select_all();
        view.copy_selected(cx);
        view.paste(cx);
    });
    for _ in 0..30 {
        let ServiceEvent::FileOperation(event) = pollster::block_on(subscription.next()) else {
            continue;
        };
        let failed = matches!(
            event.state,
            explorie_native_services::FileOperationState::Failed
        );
        view.update(cx, |view, cx| view.apply_file_operation_event(event, cx));
        if failed {
            break;
        }
    }

    assert_eq!(
        fs::read_to_string(destination.join("first.txt")).unwrap(),
        "first"
    );
    assert_eq!(
        fs::read_to_string(destination.join("second.txt")).unwrap(),
        "existing"
    );
    view.update(cx, |view, _| {
        assert!(view.operations.latest_retryable_id().is_some());
        assert!(view.undo_ledger.can_undo(SystemTime::now()));
    });

    fs::remove_file(destination.join("second.txt")).unwrap();
    view.update(cx, |view, cx| view.retry_latest_operation(cx));
    for _ in 0..30 {
        let ServiceEvent::FileOperation(event) = pollster::block_on(subscription.next()) else {
            continue;
        };
        let completed = matches!(
            event.state,
            explorie_native_services::FileOperationState::Completed
        );
        view.update(cx, |view, cx| view.apply_file_operation_event(event, cx));
        if completed {
            break;
        }
    }

    assert_eq!(
        fs::read_to_string(destination.join("second.txt")).unwrap(),
        "second"
    );
    assert!(!destination.join(kept_name("first.txt")).exists());
    view.update(cx, |view, _| {
        assert!(view.operations.latest_retryable_id().is_none());
        assert_eq!(
            view.operations.latest().unwrap().status(),
            OperationStatus::Completed
        );
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn cancelled_copy_removes_stage_and_retry_completes_once(cx: &mut TestAppContext) {
    const FILE_SIZE: u64 = 32 * 1024 * 1024;
    let directory = fixture_dir();
    let source_dir = directory.join("source");
    let destination = directory.join("destination");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&destination).unwrap();
    let source = source_dir.join("large.bin");
    fs::write(&source, vec![0x5a; FILE_SIZE as usize]).unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let subscription = services.subscribe_async();
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(destination.clone(), services, cx));
    view.update(cx, |view, cx| {
        view.start_file_operation(
            FileOperationRequest {
                kind: FileOperationKind::Copy,
                sources: vec![source.clone()],
                destination: Some(destination.clone()),
                conflict_policy: ConflictPolicy::Error,
            },
            cx,
        );
        // Core owns deterministic mid-copy cleanup coverage. Cancel immediately here so this
        // integration test exercises the service/UI cancellation and retry path without
        // depending on filesystem throughput.
        view.cancel_latest_operation(cx);
    });

    let cancelled = loop {
        let ServiceEvent::FileOperation(event) = pollster::block_on(subscription.next()) else {
            continue;
        };
        let is_cancelled = matches!(
            event.state,
            explorie_native_services::FileOperationState::Cancelled
        );
        let is_terminal = !matches!(
            event.state,
            explorie_native_services::FileOperationState::Running
        );
        if is_terminal {
            view.update(cx, |view, cx| view.apply_file_operation_event(event, cx));
            break is_cancelled;
        }
    };
    assert!(cancelled, "copy did not reach the cancelled terminal state");
    assert_eq!(fs::metadata(&source).unwrap().len(), FILE_SIZE);
    assert!(!destination.join("large.bin").exists());
    assert!(fs::read_dir(&destination).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("explorie-copy")
    }));
    view.update(cx, |view, _| {
        assert_eq!(
            view.operations.latest().unwrap().status(),
            OperationStatus::Cancelled
        );
        assert!(view.operations.latest_retryable_id().is_some());
    });

    view.update(cx, |view, cx| view.retry_latest_operation(cx));
    let mut completed = false;
    loop {
        let ServiceEvent::FileOperation(event) = pollster::block_on(subscription.next()) else {
            continue;
        };
        let is_completed = matches!(
            event.state,
            explorie_native_services::FileOperationState::Completed
        );
        let is_terminal = !matches!(
            event.state,
            explorie_native_services::FileOperationState::Running
        );
        if is_terminal {
            view.update(cx, |view, cx| view.apply_file_operation_event(event, cx));
        }
        if is_completed {
            completed = true;
            break;
        }
        if is_terminal {
            break;
        }
    }
    view.update(cx, |view, _| {
        assert!(
            completed,
            "retry ended as {:?}: {:?}",
            view.operations.latest().map(|operation| operation.status()),
            view.operations
                .latest()
                .and_then(|operation| operation.error())
        );
    });
    assert_eq!(
        fs::metadata(destination.join("large.bin")).unwrap().len(),
        FILE_SIZE
    );
    assert!(!destination.join(kept_name("large.bin")).exists());
    view.update(cx, |view, _| {
        assert!(view.operations.latest_retryable_id().is_none());
        assert_eq!(
            view.operations.latest().unwrap().status(),
            OperationStatus::Completed
        );
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn conflict_prompt_skips_one_then_keeps_both_for_the_next_unresolved_item(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let source_dir = directory.join("source");
    let destination = directory.join("destination");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&destination).unwrap();
    let first = source_dir.join("first.txt");
    let second = source_dir.join("second.txt");
    let third = source_dir.join("third.txt");
    fs::write(&first, "first-new").unwrap();
    fs::write(&second, "second-new").unwrap();
    fs::write(&third, "third-new").unwrap();
    fs::write(destination.join("second.txt"), "second-existing").unwrap();
    fs::write(destination.join("third.txt"), "third-existing").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(destination.clone(), services, cx));
    view.update(window, |view, cx| {
        view.start_service_events(cx);
        view.start_file_operation(
            FileOperationRequest {
                kind: FileOperationKind::Copy,
                sources: vec![first.clone(), second.clone(), third.clone()],
                destination: Some(destination.clone()),
                conflict_policy: ConflictPolicy::Error,
            },
            cx,
        );
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            !view.operation_ui.conflict_prompts.is_empty()
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(window, |view, _| {
        let prompt = view.operation_ui.conflict_prompts.front().unwrap();
        assert_eq!(prompt.request.sources, vec![second.clone(), third.clone()]);
        assert_eq!(prompt.current_source(), Some(second.as_path()));
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();
    let backdrop = window.debug_bounds("file-conflict-backdrop").unwrap();
    let dialog = window.debug_bounds("file-conflict-dialog").unwrap();
    let header = window.debug_bounds("file-conflict-header").unwrap();
    let content = window.debug_bounds("file-conflict-content").unwrap();
    let footer = window.debug_bounds("file-conflict-footer").unwrap();
    assert_eq!(f32::from(backdrop.size.width), 800.0);
    assert_eq!(f32::from(backdrop.size.height), 600.0);
    assert_eq!(f32::from(dialog.size.width), 560.0);
    assert_eq!(dialog.center().x, backdrop.center().x);
    assert!((f32::from(dialog.center().y) - f32::from(backdrop.center().y)).abs() <= 0.5);
    assert_eq!(f32::from(header.size.height), 46.0);
    assert_eq!(f32::from(footer.size.height), 46.0);
    assert_eq!(header.top(), dialog.top() + px(1.0));
    assert_eq!(header.bottom(), content.top());
    assert_eq!(content.bottom(), footer.top());
    assert_eq!(footer.bottom() + px(1.0), dialog.bottom());
    let skip = window.debug_bounds("conflict-skip").unwrap().center();
    window.simulate_click(skip, gpui::Modifiers::default());

    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        let third_is_current = view.update(window, |view, _| {
            view.operation_ui
                .conflict_prompts
                .front()
                .and_then(FileConflictPrompt::current_source)
                == Some(third.as_path())
        });
        if third_is_current {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(window, |view, _| {
        assert_eq!(
            view.operation_ui
                .conflict_prompts
                .front()
                .and_then(FileConflictPrompt::current_source),
            Some(third.as_path())
        );
    });
    window.run_until_parked();
    let keep_both = window.debug_bounds("conflict-keep-both").unwrap().center();
    window.simulate_click(keep_both, gpui::Modifiers::default());
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if destination.join(kept_name("third.txt")).is_file()
            && view.update(window, |view, _| {
                view.operation_ui.conflict_prompts.is_empty()
            })
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(
        fs::read_to_string(destination.join("first.txt")).unwrap(),
        "first-new"
    );
    assert_eq!(
        fs::read_to_string(destination.join("second.txt")).unwrap(),
        "second-existing"
    );
    assert!(!destination.join(kept_name("second.txt")).exists());
    assert_eq!(
        fs::read_to_string(destination.join("third.txt")).unwrap(),
        "third-existing"
    );
    assert_eq!(
        fs::read_to_string(destination.join(kept_name("third.txt"))).unwrap(),
        "third-new"
    );
    view.update(window, |view, _| {
        assert!(view.operation_ui.conflict_prompts.is_empty());
        assert!(view.operation_ui.conflict_continuations.is_empty());
        assert!(view.operations.latest_retryable_id().is_none());
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn conflict_prompt_apply_to_all_replaces_every_unresolved_item(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let source_dir = directory.join("source");
    let destination = directory.join("destination");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&destination).unwrap();
    let first = source_dir.join("first.txt");
    let second = source_dir.join("second.txt");
    fs::write(&first, "first-new").unwrap();
    fs::write(&second, "second-new").unwrap();
    fs::write(destination.join("first.txt"), "first-existing").unwrap();
    fs::write(destination.join("second.txt"), "second-existing").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(destination.clone(), services, cx));
    view.update(window, |view, cx| {
        view.start_service_events(cx);
        view.start_file_operation(
            FileOperationRequest {
                kind: FileOperationKind::Copy,
                sources: vec![first.clone(), second.clone()],
                destination: Some(destination.clone()),
                conflict_policy: ConflictPolicy::Error,
            },
            cx,
        );
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            !view.operation_ui.conflict_prompts.is_empty()
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();
    window.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::KeyDown(KeyDownEvent {
                keystroke: Keystroke::parse("a").unwrap(),
                is_held: true,
                prefer_character_input: false,
            }),
            cx,
        );
    });
    view.update(window, |view, _| {
        assert!(
            !view
                .operation_ui
                .conflict_prompts
                .front()
                .unwrap()
                .apply_to_all
        )
    });
    let apply_all = window.debug_bounds("conflict-apply-all").unwrap().center();
    window.simulate_click(apply_all, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert!(
            view.operation_ui
                .conflict_prompts
                .front()
                .unwrap()
                .apply_to_all
        )
    });
    let replace = window.debug_bounds("conflict-replace").unwrap().center();
    window.simulate_click(replace, gpui::Modifiers::default());
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if fs::read_to_string(destination.join("first.txt"))
            .is_ok_and(|contents| contents == "first-new")
            && fs::read_to_string(destination.join("second.txt"))
                .is_ok_and(|contents| contents == "second-new")
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        fs::read_to_string(destination.join("first.txt")).unwrap(),
        "first-new"
    );
    assert_eq!(
        fs::read_to_string(destination.join("second.txt")).unwrap(),
        "second-new"
    );
    view.update(window, |view, _| {
        assert!(view.operation_ui.conflict_prompts.is_empty());
        assert!(view.operations.latest_retryable_id().is_none());
        assert!(!view.undo_ledger.can_undo(SystemTime::now()));
    });
    // Replaced files go to the Recycle Bin, whose shell handles can linger.
    remove_fixture(&directory);
}

#[gpui::test]
fn native_new_folder_prompt_applies_a_safe_filesystem_mutation(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.prompt_new_folder(cx);
        view.mutation.prompt.as_mut().unwrap().input = "GPUI proof".to_string();
        view.submit_mutation_prompt(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| !view.mutation.in_progress) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(directory.join("GPUI proof").is_dir());
    view.update(cx, |view, _| {
        assert!(view.mutation.prompt.is_none());
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|message| message.contains("Created or renamed"))
        );
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn permanent_delete_requires_exact_confirmation_and_never_records_undo_or_retry(
    cx: &mut TestAppContext,
) {
    let directory = fixture_dir();
    let file = directory.join("doomed.txt");
    let folder = directory.join("doomed-folder");
    fs::write(&file, "delete me").unwrap();
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("nested.txt"), "delete me too").unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    let entry = |path: PathBuf, is_dir| FileEntry {
        id: Uuid::new_v4(),
        path,
        size: 9,
        modified: SystemTime::UNIX_EPOCH,
        hidden: false,
        is_dir,
        custom: std::collections::HashMap::new(),
        is_symlink: false,
        is_junction: false,
        link_target: None,
        has_xattrs: false,
        is_package: false,
        link_target_is_dir: false,
        is_cloud_placeholder: false,
        tags: Vec::new(),
    };

    view.update(cx, |view, cx| view.start_preview_helpers(cx));
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| !view.preview.helpers_loading) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, _| {
        assert_eq!(
            view.preview
                .helpers
                .iter()
                .map(|helper| helper.name.as_str())
                .collect::<Vec<_>>(),
            vec!["FFmpeg", "LibreOffice", "ImageMagick"]
        );
        assert!(view.preview.helpers.iter().all(|helper| {
            helper.available == helper.error.is_none() && !helper.extensions.is_empty()
        }));
    });

    view.update(cx, |view, cx| {
        view.browser.replace_entries(vec![
            entry(file.clone(), false),
            entry(folder.clone(), true),
        ]);
        view.browser.select_all();
        view.prompt_permanent_delete_selected(cx);
        view.mutation.prompt.as_mut().unwrap().input = "delete".to_string();
        view.submit_mutation_prompt(cx);
    });

    assert!(file.is_file());
    assert!(folder.is_dir());
    view.update(cx, |view, _| {
        assert!(matches!(
            view.mutation.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(MutationPromptKind::PermanentDelete { .. })
        ));
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|message| message.contains("DELETE exactly"))
        );
    });

    view.update(cx, |view, cx| {
        view.mutation.prompt.as_mut().unwrap().input = "DELETE".to_string();
        view.submit_mutation_prompt(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        let finished = view.update(cx, |view, _| {
            !view.mutation.in_progress
                && view.mutation.prompt.is_none()
                && view
                    .status_message
                    .as_deref()
                    .is_some_and(|message| message.contains("cannot be undone"))
        });
        if finished && !file.exists() && !folder.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let status = view.update(cx, |view, _| view.status_message.clone());
    assert!(!file.exists(), "file remained after deletion: {status:?}");
    assert!(
        !folder.exists(),
        "folder remained after deletion: {status:?}"
    );
    view.update(cx, |view, _| {
        assert!(view.mutation.prompt.is_none());
        assert!(!view.mutation.in_progress);
        assert!(!view.undo_ledger.can_undo(SystemTime::now()));
        assert!(view.operations.latest_retryable_id().is_none());
        assert!(view.operations.latest().is_none());
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|message| message.contains("cannot be undone"))
        );
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn native_archive_prompts_create_and_extract_a_real_zip(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let source = directory.join("source.txt");
    fs::write(&source, "archive proof").unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services.clone(), cx));
    let entry = |path: PathBuf| FileEntry {
        id: Uuid::new_v4(),
        path,
        size: 13,
        modified: SystemTime::UNIX_EPOCH,
        hidden: false,
        is_dir: false,
        custom: std::collections::HashMap::new(),
        is_symlink: false,
        is_junction: false,
        link_target: None,
        has_xattrs: false,
        is_package: false,
        link_target_is_dir: false,
        is_cloud_placeholder: false,
        tags: Vec::new(),
    };

    view.update(cx, |view, cx| {
        view.apply_service_event(
            ServiceEvent::ArchiveProgress(ArchiveProgressEvent {
                operation_id: "proof".to_string(),
                processed_bytes: 5,
                total_bytes: 10,
                current_path: source.to_string_lossy().into_owned(),
            }),
            cx,
        );
        assert_eq!(
            view.operation_ui
                .archive_progress
                .as_ref()
                .map(|progress| progress.processed_bytes),
            Some(5)
        );
        view.browser.replace_entries(vec![entry(source.clone())]);
        view.browser.select(source.clone());
        view.prompt_create_archive(cx);
        view.cycle_archive_format(cx);
        view.cycle_archive_format(cx);
        view.cycle_archive_format(cx);
        view.cycle_archive_format(cx);
        view.cycle_archive_compression(cx);
        assert!(matches!(
            view.mutation.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(MutationPromptKind::ArchiveName {
                format: ArchiveFormat::Zip,
                compression_level: CompressionLevel::Best,
                ..
            })
        ));
        view.mutation.prompt.as_mut().unwrap().input = "bundle".to_string();
        view.submit_mutation_prompt(cx);
        assert!(matches!(
            view.mutation.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(MutationPromptKind::ArchivePassword { .. })
        ));
        view.submit_mutation_prompt(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| !view.mutation.in_progress) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let archive = directory.join("bundle.zip");
    assert!(archive.is_file());
    view.update(cx, |view, cx| {
        view.browser.replace_entries(vec![entry(archive.clone())]);
        view.browser.select(archive.clone());
        view.inspect_selected_archive(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| !view.preview.archive_inspection_loading) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, _| {
        let (_, info) = view
            .preview
            .archive_inspection
            .as_ref()
            .expect("archive inspection");
        assert_eq!(info.entry_count, 1);
        assert_eq!(info.entries[0].path, "source.txt");
    });
    view.update(cx, |view, cx| {
        view.prompt_extract_archive(cx);
        view.mutation.prompt.as_mut().unwrap().input = "unpacked".to_string();
        view.submit_mutation_prompt(cx);
        assert!(matches!(
            view.mutation.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(MutationPromptKind::ExtractPassword { .. })
        ));
        view.submit_mutation_prompt(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| !view.mutation.in_progress) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let extracted = directory.join("unpacked").join("source.txt");
    let extraction_state = view.update(cx, |view, _| {
        (
            view.status_message.clone(),
            view.mutation
                .prompt
                .as_ref()
                .and_then(|prompt| prompt.error.clone()),
        )
    });
    assert!(
        extracted.is_file(),
        "extraction did not produce {}: {:?}",
        extracted.display(),
        extraction_state
    );
    assert_eq!(fs::read_to_string(extracted).unwrap(), "archive proof");
    view.update(cx, |view, _| {
        assert!(view.mutation.prompt.is_none());
        assert!(!view.undo_ledger.can_undo(SystemTime::now()));
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|message| message.contains("Extracted"))
        );
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn native_sevenzip_fallback_inspection_and_extraction(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let source = directory.join("source.txt");
    fs::write(&source, "7-Zip native proof").unwrap();
    // The fallback suffix selects the full engine; 7-Zip identifies the
    // actual ZIP contents itself. Core fixtures separately exercise XZ/WIM.
    let archive = directory.join("bundle.cab");
    explorie_core::archive::create_zip_archive(&[source], &archive, CompressionLevel::Normal)
        .unwrap();
    assert_eq!(preview_panel::route(&archive, false), PreviewRoute::Archive);
    assert_eq!(archive_base_name(&archive), "bundle");
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(cx, |view, cx| view.inspect_archive(archive.clone(), cx));
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| !view.preview.archive_inspection_loading) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, cx| {
        let (_, info) = view
            .preview
            .archive_inspection
            .as_ref()
            .expect("7-Zip archive inspection");
        assert_eq!(info.entries[0].path, "source.txt");
        view.prompt_extract_archive_path(archive.clone(), cx);
        view.submit_mutation_prompt(cx);
        assert!(matches!(
            view.mutation.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(MutationPromptKind::ExtractPassword { .. })
        ));
        view.submit_mutation_prompt(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| !view.mutation.in_progress) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        fs::read_to_string(directory.join("bundle/source.txt")).unwrap(),
        "7-Zip native proof"
    );
    view.update(cx, |view, _| {
        assert!(view.mutation.prompt.is_none());
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|message| message.contains("Extracted"))
        );
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn native_text_preview_is_bounded_generation_safe_and_recovers_from_failure(
    cx: &mut TestAppContext,
) {
    let directory = fixture_dir();
    let first = directory.join("first.txt");
    let second = directory.join("second.txt");
    let image_path = directory.join("pixel.png");
    let missing = directory.join("missing.txt");
    fs::write(&first, vec![b'a'; 600 * 1024]).unwrap();
    fs::write(&second, "second preview wins").unwrap();
    fs::write(
        &image_path,
        [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 8, 215, 99, 248, 207,
            192, 240, 31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ],
    )
    .unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    let entry = |path: PathBuf| FileEntry {
        id: Uuid::new_v4(),
        path,
        size: 600 * 1024,
        modified: SystemTime::UNIX_EPOCH,
        hidden: false,
        is_dir: false,
        custom: std::collections::HashMap::new(),
        is_symlink: false,
        is_junction: false,
        link_target: None,
        has_xattrs: false,
        is_package: false,
        link_target_is_dir: false,
        is_cloud_placeholder: false,
        tags: Vec::new(),
    };

    view.update(cx, |view, cx| {
        view.browser
            .replace_entries(vec![entry(first.clone()), entry(second.clone())]);
        view.browser.select(first.clone());
        view.preview_selected(cx);
        view.browser.select(second.clone());
        view.preview_selected(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(view.preview.state, PreviewState::Ready { .. })
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, _| match &view.preview.state {
        PreviewState::Ready {
            path,
            content: PreviewContent::Text(preview),
        } => {
            assert_eq!(path, &second);
            assert_eq!(&*preview.text, "second preview wins");
            assert!(!preview.truncated);
        }
        state => panic!("unexpected preview state: {state:?}"),
    });

    view.update(cx, |view, cx| {
        view.select_previous(cx);
        assert_eq!(view.browser.selected_path(), Some(first.as_path()));
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(view.preview.state, PreviewState::Ready { .. })
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, _| match &view.preview.state {
        PreviewState::Ready {
            content: PreviewContent::Text(preview),
            ..
        } => {
            assert_eq!(
                preview.text.len(),
                preview_panel::text_preview_bytes() as usize
            );
            assert!(preview.truncated);
        }
        state => panic!("unexpected preview state: {state:?}"),
    });

    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );

    view.update(cx, |view, cx| {
        view.browser
            .replace_entries(vec![entry(image_path.clone())]);
        view.browser.select(image_path.clone());
        view.preview_selected(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(
                &view.preview.state,
                PreviewState::Ready {
                    path,
                    content: PreviewContent::Image(render_path),
                } if path == &image_path && render_path == &image_path
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, _| {
        assert!(matches!(
            &view.preview.state,
            PreviewState::Ready {
                path,
                content: PreviewContent::Image(render_path),
            } if path == &image_path && render_path == &image_path
        ));
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(
                view.preview.photo_metadata,
                PhotoMetadataState::Ready { .. }
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, _| {
        assert!(matches!(
            &view.preview.photo_metadata,
            PhotoMetadataState::Ready { path, metadata }
                if path == &image_path
                    && metadata.width == Some(1)
                    && metadata.height == Some(1)
        ));
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );

    view.update(cx, |view, cx| {
        view.browser.replace_entries(vec![entry(missing.clone())]);
        view.browser.select(missing.clone());
        view.preview_selected(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(view.preview.state, PreviewState::Failed { .. })
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, cx| {
        assert!(matches!(
            &view.preview.state,
            PreviewState::Failed { path, error }
                if path == &missing && error.code == ErrorCode::NotFound
        ));
        view.close_preview(cx);
        assert!(matches!(view.preview.state, PreviewState::Closed));
    });

    assert_eq!(fs::metadata(&first).unwrap().len(), 600 * 1024);
    remove_fixture(&directory);
}

#[gpui::test]
fn native_code_preview_highlights_wraps_and_honors_script_safety(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let rust_path = directory.join("preview.rs");
    let markdown_path = directory.join("notes.md");
    let script_path = directory.join("cleanup.ps1");
    fs::write(
        &rust_path,
        "// stays local\npub fn answer() -> u32 { 42 }\nlet label = \"explorie\";\n",
    )
    .unwrap();
    fs::write(
        &markdown_path,
        "# Preview parity\n\nThis paragraph wraps instead of forcing horizontal scrolling.",
    )
    .unwrap();
    fs::write(&script_path, "Write-Output \"local\"\n").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.browser.replace_entries(vec![
            absolute_entry(rust_path.clone()),
            absolute_entry(markdown_path.clone()),
            absolute_entry(script_path.clone()),
        ]);
        view.browser.select(rust_path.clone());
        view.preview_selected(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(
                &view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Text(preview),
                    ..
                } if preview.language.as_deref() == Some("Rust")
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, _| {
        let PreviewState::Ready {
            content: PreviewContent::Text(preview),
            ..
        } = &view.preview.state
        else {
            panic!("Rust preview should be ready");
        };
        assert!(!preview.wrapped);
        assert!((0..preview.line_count()).any(|line| {
            preview
                .line_syntax_spans(line)
                .iter()
                .any(|(_, kind)| *kind == TextHighlightKind::Keyword)
        }));
        let service_highlights = view
            .services
            .previews
            .read_text(rust_path.clone(), preview_panel::text_preview_bytes())
            .wait()
            .unwrap()
            .highlights
            .len();
        assert_eq!(preview.highlight_count(), service_highlights);
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );

    view.update(cx, |view, cx| {
        assert!(!view.settings.behavior.preview_executable_scripts);
        view.browser.select(script_path.clone());
        view.preview_selected(cx);
        assert!(matches!(
            view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::BlockedScript,
                ..
            }
        ));
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    view.update(cx, |view, cx| view.toggle_script_preview(cx));
    wait_for_view(&view, cx, "the allowed script's text preview", |view| {
        matches!(
            view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::Text(_),
                ..
            }
        )
    });
    view.update(cx, |view, cx| {
        assert!(view.settings.behavior.preview_executable_scripts);
        assert!(matches!(
            view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::Text(_),
                ..
            }
        ));
        view.toggle_script_preview(cx);
        assert!(matches!(
            view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::BlockedScript,
                ..
            }
        ));
        view.browser.select(markdown_path.clone());
        view.preview_selected(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(
                &view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Rich(_),
                    ..
                }
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, _| {
        assert!(matches!(
            &view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::Rich(preview),
                ..
            } if preview.subtitle.contains("Rendered Markdown")
                && preview.blocks.iter().any(|block| block.text.contains("wraps"))
        ));
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );

    view.update(cx, |view, cx| view.close_preview(cx));
    remove_fixture(&directory);
}

#[gpui::test]
fn native_pdf_preview_renders_pages_navigates_and_recovers(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let pdf_path = directory.join("document.pdf");
    let malformed_path = directory.join("malformed.pdf");
    fs::write(&pdf_path, minimal_pdf(2)).unwrap();
    fs::write(&malformed_path, b"not a PDF").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services.clone(), cx));

    view.update(cx, |view, cx| {
        view.browser.replace_entries(vec![
            absolute_entry(pdf_path.clone()),
            absolute_entry(malformed_path.clone()),
        ]);
        view.browser.select(pdf_path.clone());
        view.preview_selected(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(
                &view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Pdf { page, .. },
                    ..
                } if page.page_index == 0
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, cx| {
        let PreviewState::Ready {
            path,
            content: PreviewContent::Pdf { page, tool },
        } = &view.preview.state
        else {
            panic!("PDF preview should be ready: {:?}", view.preview.state);
        };
        assert_eq!(path, &pdf_path);
        assert_eq!(page.page_count, 2);
        assert_eq!(page.page_index, 0);
        assert!(page.image_path.is_file());
        assert!(tool.is_none());
        assert!(view.preview.pdf_fit);
        view.adjust_pdf_zoom(25, cx);
        assert_eq!(view.preview.pdf_zoom_percent, 125);
        assert!(!view.preview.pdf_fit);
        view.fit_pdf_page(cx);
        assert!(view.preview.pdf_fit);
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );

    view.update(cx, |view, cx| view.move_pdf_page(1, cx));
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(
                &view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Pdf { page, .. },
                    ..
                } if page.page_index == 1
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, cx| {
        assert!(matches!(
            &view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::Pdf { page, .. },
                ..
            } if page.page_index == 1
        ));
        view.browser.select(malformed_path.clone());
        view.preview_selected(cx);
    });
    wait_for_view(&view, cx, "the malformed PDF's fallback", |view| {
        matches!(
            &view.preview.state,
            PreviewState::Ready {
                path,
                content: PreviewContent::Fallback { .. },
            } if path == &malformed_path
        )
    });
    view.update(cx, |view, cx| {
        assert!(
            matches!(
                &view.preview.state,
                PreviewState::Ready {
                    path,
                    content: PreviewContent::Fallback {
                        error: Some(error),
                        ..
                    },
                } if path == &malformed_path && error.code == ErrorCode::InvalidInput
            ),
            "unexpected malformed PDF preview state: {:?}",
            view.preview.state
        );
        fs::write(&malformed_path, minimal_pdf(1)).unwrap();
        view.retry_preview(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(
                &view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Pdf { page, .. },
                    ..
                } if page.page_count == 1
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, cx| {
        assert!(matches!(
            &view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::Pdf { page, .. },
                ..
            } if page.page_count == 1
        ));
        view.close_preview(cx);
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn clearing_preview_cache_refreshes_the_open_cached_preview(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let pdf_path = directory.join("cached.pdf");
    fs::write(&pdf_path, minimal_pdf(1)).unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let cache_dir = services.previews.cache_dir().to_path_buf();
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services.clone(), cx));

    view.update(cx, |view, cx| {
        view.browser
            .replace_entries(vec![absolute_entry(pdf_path.clone())]);
        view.browser.select(pdf_path.clone());
        view.preview_selected(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(
                &view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Pdf { page, .. },
                    ..
                } if page.image_path.is_file()
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    fs::create_dir_all(&cache_dir).unwrap();
    let sentinel = cache_dir.join("stale-preview-sentinel");
    fs::write(&sentinel, b"stale").unwrap();
    let generation = view.update(cx, |view, cx| {
        assert!(matches!(
            &view.preview.state,
            PreviewState::Ready {
                path,
                content: PreviewContent::Pdf { page, .. },
            } if path == &pdf_path && page.image_path.is_file()
        ));
        let generation = view.preview.generation;
        view.clear_preview_cache(cx);
        generation
    });

    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            view.preview.generation > generation
                && !sentinel.exists()
                && matches!(
                    &view.preview.state,
                    PreviewState::Ready {
                        path,
                        content: PreviewContent::Pdf { page, .. },
                    } if path == &pdf_path && page.image_path.is_file()
                )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    view.update(cx, |view, _| {
        assert_eq!(view.preview.generation, generation.wrapping_add(1));
        assert!(!sentinel.exists());
        assert!(matches!(
            &view.preview.state,
            PreviewState::Ready {
                path,
                content: PreviewContent::Pdf { page, .. },
            } if path == &pdf_path && page.image_path.is_file()
        ));
        assert_eq!(
            view.status_message.as_deref(),
            Some("Preview cache cleared; refreshing preview…")
        );
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );

    remove_fixture(&directory);
}

#[gpui::test]
fn native_audio_preview_renders_controls_and_owns_playback_lifecycle(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let audio_path = directory.join("local-track.mp3");
    let text_path = directory.join("notes.txt");
    fs::write(&audio_path, b"fake decoder input").unwrap();
    fs::write(&text_path, "next preview").unwrap();
    let playback = Arc::new(FakeGpuiAudioPlayback::default());
    let services = NativeServices::with_audio_backend(
        ResourcePaths::test(&directory),
        Arc::new(FakeGpuiAudioBackend {
            playback: Arc::clone(&playback),
        }),
    );
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services.clone(), cx));

    view.update(cx, |view, cx| {
        view.browser.replace_entries(vec![
            absolute_entry(audio_path.clone()),
            absolute_entry(text_path.clone()),
        ]);
        view.browser.select(audio_path.clone());
        view.preview_selected(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(
                view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Audio,
                    ..
                }
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, cx| {
        assert!(matches!(
            &view.preview.state,
            PreviewState::Ready {
                path,
                content: PreviewContent::Audio,
            } if path == &audio_path
        ));
        let status = view
            .media
            .read(cx)
            .audio_status
            .as_ref()
            .expect("audio status");
        assert_eq!(status.duration_ms, Some(80_000));
        assert!(!status.playing, "audio previews must not autoplay");
        view.media
            .update(cx, |media, cx| media.toggle_audio_playback(cx));
        assert!(view.media.read(cx).audio_status.as_ref().unwrap().playing);
        view.media
            .update(cx, |media, cx| media.seek_audio_fraction(0.5, cx));
        assert_eq!(
            view.media
                .read(cx)
                .audio_status
                .as_ref()
                .unwrap()
                .position_ms,
            40_000
        );
        view.media
            .update(cx, |media, cx| media.adjust_audio_volume(-0.3, cx));
        assert!(
            (view.media.read(cx).audio_status.as_ref().unwrap().volume - 0.5).abs() < f32::EPSILON
        );
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );

    view.update(cx, |view, cx| {
        playback.state.lock().unwrap().stopped = true;
        view.media
            .update(cx, |media, _| media.refresh_audio_status());
        assert!(view.media.read(cx).audio_status.as_ref().unwrap().finished);
        view.media
            .update(cx, |media, cx| media.toggle_audio_playback(cx));
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, cx| {
            view.media
                .read(cx)
                .audio_status
                .as_ref()
                .is_some_and(|status| status.playing)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, cx| {
        assert!(view.media.read(cx).audio_status.as_ref().unwrap().playing);
    });

    view.update(cx, |view, cx| {
        view.set_preview_tab(PreviewTab::Metadata, cx);
        assert!(playback.state.lock().unwrap().paused);
        view.set_preview_tab(PreviewTab::Preview, cx);
        view.media
            .update(cx, |media, cx| media.toggle_audio_playback(cx));
        view.browser.select(text_path.clone());
        view.preview_selected(cx);
        assert!(playback.state.lock().unwrap().stopped);
        assert!(view.media.read(cx).audio_status.is_none());
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(
                view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Text(_),
                    ..
                }
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, _| {
        assert!(matches!(
            view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::Text(_),
                ..
            }
        ));
    });

    remove_fixture(&directory);
}

#[gpui::test]
fn native_video_preview_renders_controls_and_owns_decoder_lifecycle(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let video_path = directory.join("local-clip.mp4");
    let text_path = directory.join("notes.txt");
    fs::write(&video_path, b"fake decoder input").unwrap();
    fs::write(&text_path, "next preview").unwrap();
    let playback = Arc::new(FakeGpuiVideoPlayback::new(video_path.clone()));
    let services = NativeServices::with_video_backend(
        ResourcePaths::test(&directory),
        Arc::new(FakeGpuiVideoBackend {
            playback: Arc::clone(&playback),
        }),
    );
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services.clone(), cx));

    view.update(cx, |view, cx| {
        view.browser.replace_entries(vec![
            absolute_entry(video_path.clone()),
            absolute_entry(text_path.clone()),
        ]);
        view.browser.select(video_path.clone());
        view.preview_selected(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(
                view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Video,
                    ..
                }
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, cx| {
        assert!(matches!(
            &view.preview.state,
            PreviewState::Ready {
                path,
                content: PreviewContent::Video,
            } if path == &video_path
        ));
        assert!(!view.media.read(cx).video_status.as_ref().unwrap().playing);
        assert_eq!(
            view.media
                .read(cx)
                .video_status
                .as_ref()
                .unwrap()
                .duration_ms,
            Some(80_000)
        );
        assert!(view.media.read(cx).video_frame.is_some());
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );

    view.update(cx, |view, cx| {
        view.media
            .update(cx, |media, cx| media.toggle_video_playback(cx));
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, cx| {
            view.media
                .read(cx)
                .video_status
                .as_ref()
                .is_some_and(|status| status.playing)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, cx| {
        assert!(view.media.read(cx).video_status.as_ref().unwrap().playing);
        view.media
            .update(cx, |media, cx| media.seek_video_fraction(0.5, cx));
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, cx| {
            view.media
                .read(cx)
                .video_status
                .as_ref()
                .is_some_and(|status| status.position_ms == 40_000)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, cx| {
        assert_eq!(
            view.media
                .read(cx)
                .video_status
                .as_ref()
                .unwrap()
                .position_ms,
            40_000
        );
        assert_eq!(view.media.read(cx).video_frame_position_ms, Some(40_000));
        view.media
            .update(cx, |media, cx| media.adjust_video_volume(-0.3, cx));
        assert!(
            (view.media.read(cx).video_status.as_ref().unwrap().volume - 0.5).abs() < f32::EPSILON
        );
        view.set_preview_tab(PreviewTab::Metadata, cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, cx| {
            view.media
                .read(cx)
                .video_status
                .as_ref()
                .is_some_and(|status| !status.playing)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, cx| {
        assert!(!view.media.read(cx).video_status.as_ref().unwrap().playing);
        view.set_preview_tab(PreviewTab::Preview, cx);
        view.browser.select(text_path.clone());
        view.preview_selected(cx);
        assert!(*playback.stopped.lock().unwrap());
        assert!(view.media.read(cx).video_status.is_none());
        assert!(view.media.read(cx).video_frame.is_none());
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(
                view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Text(_),
                    ..
                }
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    services.video.stop();
    remove_fixture(&directory);
}

#[gpui::test]
fn real_ffmpeg_video_preview_recovers_renders_and_plays(cx: &mut TestAppContext) {
    let available = std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
        && std::process::Command::new("ffprobe")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
    if !available {
        eprintln!("skipping rendered FFmpeg video test: helpers unavailable");
        return;
    }

    let directory = fixture_dir();
    let video_path = directory.join("real-clip.mp4");
    fs::write(&video_path, b"malformed video").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services.clone(), cx));
    view.update(cx, |view, cx| {
        view.browser
            .replace_entries(vec![absolute_entry(video_path.clone())]);
        view.browser.select(video_path.clone());
        view.preview_selected(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(
                view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Fallback { .. },
                    ..
                }
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, _| {
        assert!(matches!(
            view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::Fallback {
                    error: Some(ServiceError {
                        code: ErrorCode::InvalidInput,
                        ..
                    }),
                    ..
                },
                ..
            }
        ));
    });

    let generated = std::process::Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x90:rate=15",
            "-t",
            "1",
            "-c:v",
            "mpeg4",
            "-pix_fmt",
            "yuv420p",
            "-y",
        ])
        .arg(&video_path)
        .status()
        .unwrap();
    assert!(generated.success());
    view.update(cx, |view, cx| view.retry_preview(cx));
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, cx| {
            matches!(
                view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Video,
                    ..
                }
            ) && view.media.read(cx).video_frame.is_some()
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, cx| {
        assert!(matches!(
            view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::Video,
                ..
            }
        ));
        assert_eq!(
            view.media
                .read(cx)
                .video_status
                .as_ref()
                .map(|status| (status.width, status.height)),
            Some((160, 90))
        );
        assert!(view.media.read(cx).video_frame.is_some());
        view.media
            .update(cx, |media, cx| media.toggle_video_playback(cx));
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, cx| {
            view.media
                .update(cx, |media, cx| media.refresh_video_frame(cx));
            view.media
                .read(cx)
                .video_status
                .as_ref()
                .is_some_and(|status| status.playing && status.position_ms > 0)
                && view
                    .media
                    .read(cx)
                    .video_frame_position_ms
                    .is_some_and(|position| position > 0)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, cx| {
        assert!(
            view.media
                .read(cx)
                .video_status
                .as_ref()
                .is_some_and(|status| status.position_ms > 0)
        );
        assert!(
            view.media
                .read(cx)
                .video_frame_position_ms
                .is_some_and(|position| position > 0)
        );
        view.close_preview(cx);
        assert!(view.media.read(cx).video_status.is_none());
    });
    services.video.stop();
    remove_fixture(&directory);
}

#[gpui::test]
fn native_preview_inspector_renders_tabs_and_persists_custom_fields(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let file = directory.join("report.txt");
    fs::write(&file, "preview inspector proof").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    let mut entry = absolute_entry(file.clone());
    entry.size = fs::metadata(&file).unwrap().len();
    entry
        .custom
        .insert("status".to_string(), serde_json::json!("Todo"));

    view.update(cx, |view, cx| {
        view.browser.replace_entries(vec![entry]);
        view.browser.select(file.clone());
        view.preview_selected(cx);
        view.set_preview_tab(PreviewTab::Metadata, cx);
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    view.update(cx, |view, cx| {
        assert_eq!(view.preview.tab, PreviewTab::Metadata);
        view.set_preview_tab(PreviewTab::CustomFields, cx);
        view.begin_add_custom_field(cx);
        for character in ["p", "r", "i", "o", "r", "i", "t", "y"] {
            view.handle_custom_field_key(
                &KeyDownEvent {
                    keystroke: Keystroke::parse(character).unwrap().with_simulated_ime(),
                    is_held: false,
                    prefer_character_input: false,
                },
                cx,
            );
        }
        view.handle_custom_field_key(
            &KeyDownEvent {
                keystroke: Keystroke::parse("tab").unwrap(),
                is_held: false,
                prefer_character_input: false,
            },
            cx,
        );
        for character in ["h", "i", "g", "h"] {
            view.handle_custom_field_key(
                &KeyDownEvent {
                    keystroke: Keystroke::parse(character).unwrap().with_simulated_ime(),
                    is_held: false,
                    prefer_character_input: false,
                },
                cx,
            );
        }
        view.handle_custom_field_key(
            &KeyDownEvent {
                keystroke: Keystroke::parse("enter").unwrap(),
                is_held: false,
                prefer_character_input: false,
            },
            cx,
        );
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            view.preview
                .custom_fields_editor
                .as_ref()
                .is_some_and(|editor| !editor.pending && editor.fields.contains_key("priority"))
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let schema: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join(".explorie.json")).unwrap()).unwrap();
    assert_eq!(schema["report.txt"]["status"], "Todo");
    assert_eq!(schema["report.txt"]["priority"], "high");
    view.update(cx, |view, cx| {
        let editor = view.preview.custom_fields_editor.as_ref().unwrap();
        assert_eq!(editor.fields["priority"], "high");
        assert!(editor.draft.is_none());
        view.remove_custom_field("status", cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            view.preview
                .custom_fields_editor
                .as_ref()
                .is_some_and(|editor| !editor.pending && !editor.fields.contains_key("status"))
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    let schema: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join(".explorie.json")).unwrap()).unwrap();
    assert!(schema["report.txt"].get("status").is_none());
    assert_eq!(schema["report.txt"]["priority"], "high");
    assert!(file.is_file());
    remove_fixture(&directory);
}

#[gpui::test]
fn preview_navigation_skips_folders_and_dispatches_pointer_and_arrow_paths(
    cx: &mut TestAppContext,
) {
    let directory = fixture_dir();
    let first = directory.join("a.txt");
    let folder = directory.join("between");
    let second = directory.join("b.txt");
    fs::write(&first, "first preview").unwrap();
    fs::create_dir(&folder).unwrap();
    fs::write(&second, "second preview").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(directory.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });
    let mut folder_entry = absolute_entry(folder);
    folder_entry.is_dir = true;
    view.update(window, |view, cx| {
        view.browser.replace_entries(vec![
            folder_entry,
            absolute_entry(first.clone()),
            absolute_entry(second.clone()),
        ]);
        view.browser.select(first.clone());
        view.settings.view.show_preview_panel = true;
        view.sync_pinned_preview(cx);
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if window.debug_bounds("preview-file-counter").is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(window.debug_bounds("preview-previous").is_some());
    assert!(window.debug_bounds("preview-next").is_some());
    assert!(window.debug_bounds("preview-file-counter").is_some());
    let narrow_sidebar = window.debug_bounds("sidebar").unwrap();
    let narrow_preview = window.debug_bounds("preview-panel").unwrap();
    assert!(narrow_preview.left() >= narrow_sidebar.right());
    assert_eq!(
        f32::from(window.debug_bounds("browser-toolbar").unwrap().size.height),
        72.0
    );
    window.simulate_resize(gpui::size(px(1200.0), px(720.0)));
    window.run_until_parked();
    assert!(window.debug_bounds("preview-previous").is_some());
    assert!(window.debug_bounds("preview-next").is_some());
    assert!(window.debug_bounds("preview-file-counter").is_some());
    let wide_toolbar = window.debug_bounds("browser-toolbar").unwrap();
    let wide_tabs = window.debug_bounds("tabs").unwrap();
    let wide_surface = window.debug_bounds("file-surface").unwrap();
    let wide_preview = window.debug_bounds("preview-panel").unwrap();
    assert_eq!(f32::from(wide_toolbar.size.height), 40.0);
    assert!(wide_toolbar.bottom() <= wide_tabs.top());
    assert!(wide_preview.left() >= wide_surface.right());
    assert!(wide_preview.top() >= wide_tabs.bottom());
    view.update(window, |view, _| {
        let (position, total, previous, next) = view.preview_navigation(&first);
        assert_eq!((position, total), (1, 2));
        assert!(previous.is_none());
        assert_eq!(next.as_deref(), Some(second.as_path()));
    });

    let next = window.debug_bounds("preview-next").unwrap().center();
    window.simulate_click(next, gpui::Modifiers::default());
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            view.preview.state.path() == Some(second.as_path())
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(window, |view, _| {
        assert_eq!(view.browser.selected_path(), Some(second.as_path()));
        assert_eq!(view.preview_navigation(&second).0, 2);
    });

    window.simulate_click(next, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, cx| {
        assert_eq!(view.preview.state.path(), Some(second.as_path()));
        view.column_left(cx);
        assert_eq!(view.preview.state.path(), Some(first.as_path()));
        view.select_next(cx);
        assert_eq!(view.preview.state.path(), Some(second.as_path()));
        view.close_preview(cx);
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn quick_look_terminal_states_are_centered_in_the_preview_canvas(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let file = directory.join("missing.txt");
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(directory.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });
    view.update(window, |view, cx| {
        view.quick_look.open = true;
        view.quick_look.paths = vec![file.clone()];
        view.preview.state = PreviewState::Failed {
            path: file.clone(),
            error: ServiceError::new(
                ErrorCode::InvalidInput,
                "Preview detection requires a regular file",
            ),
        };
        cx.notify();
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();

    let content = window.debug_bounds("quick-look-content").unwrap();
    let retry = window.debug_bounds("retry-preview").unwrap();
    assert!(
        (f32::from(retry.center().x) - f32::from(content.center().x)).abs() <= 1.0,
        "retry action should be horizontally centered in the preview canvas"
    );
    assert!(
        (f32::from(retry.center().y) - f32::from(content.center().y)).abs() <= 80.0,
        "retry action should stay near the vertical center of the preview canvas"
    );

    view.update(window, |view, cx| {
        view.preview.state = PreviewState::Loading { path: file.clone() };
        cx.notify();
    });
    window.run_until_parked();
    assert_eq!(
        window.debug_bounds("preview-loading").unwrap().center(),
        window.debug_bounds("quick-look-content").unwrap().center()
    );

    view.update(window, |view, cx| {
        view.preview.state = PreviewState::Ready {
            path: file.clone(),
            content: PreviewContent::BlockedScript,
        };
        cx.notify();
    });
    window.run_until_parked();
    assert_eq!(
        window
            .debug_bounds("preview-blocked-script")
            .unwrap()
            .center(),
        window.debug_bounds("quick-look-content").unwrap().center()
    );

    remove_fixture(&directory);
}

#[gpui::test]
fn quick_look_restores_legacy_modal_geometry_navigation_and_pinned_preview_return(
    cx: &mut TestAppContext,
) {
    let directory = fixture_dir();
    let first = directory.join("alpha.txt");
    let folder = directory.join("between");
    let second = directory.join("beta.txt");
    fs::write(&first, "alpha preview").unwrap();
    fs::create_dir(&folder).unwrap();
    fs::write(&second, "beta preview").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(directory.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });
    let mut folder_entry = absolute_entry(folder);
    folder_entry.is_dir = true;
    view.update(window, |view, cx| {
        view.install_shortcut_bindings(cx);
        view.browser.replace_entries(vec![
            folder_entry,
            absolute_entry(first.clone()),
            absolute_entry(second.clone()),
        ]);
        view.browser.select(first.clone());
        cx.notify();
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();

    window.update(|window, cx| {
        let result = window.dispatch_event(
            gpui::PlatformInput::KeyDown(KeyDownEvent {
                keystroke: Keystroke::parse("space").unwrap(),
                is_held: false,
                prefer_character_input: false,
            }),
            cx,
        );
        assert!(!result.propagate);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if window.debug_bounds("quick-look-modal").is_some()
            && view.update(window, |view, _| {
                matches!(view.preview.state, PreviewState::Ready { .. })
            })
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    let backdrop = window.debug_bounds("quick-look-backdrop").unwrap();
    let modal = window.debug_bounds("quick-look-modal").unwrap();
    let header = window.debug_bounds("quick-look-header").unwrap();
    let content = window.debug_bounds("quick-look-content").unwrap();
    assert_eq!(f32::from(backdrop.size.width), 800.0);
    assert_eq!(f32::from(backdrop.size.height), 600.0);
    assert_eq!(f32::from(modal.size.width), 776.0);
    assert_eq!(f32::from(modal.size.height), 576.0);
    assert_eq!(modal.center(), backdrop.center());
    assert_eq!(f32::from(header.size.height), 54.0);
    assert_eq!(content.top(), header.bottom());
    assert!(window.debug_bounds("preview-panel").is_none());
    view.update(window, |view, _| {
        assert!(view.quick_look.open);
        assert_eq!(view.preview_navigation(&first).0, 1);
    });

    window.update(|window, cx| {
        let duplicate = window.dispatch_event(
            gpui::PlatformInput::KeyDown(KeyDownEvent {
                keystroke: Keystroke::parse("space").unwrap(),
                is_held: false,
                prefer_character_input: false,
            }),
            cx,
        );
        assert!(!duplicate.propagate);
        let held = window.dispatch_event(
            gpui::PlatformInput::KeyDown(KeyDownEvent {
                keystroke: Keystroke::parse("space").unwrap(),
                is_held: true,
                prefer_character_input: false,
            }),
            cx,
        );
        assert!(!held.propagate);
        let released = window.dispatch_event(
            gpui::PlatformInput::KeyUp(gpui::KeyUpEvent {
                keystroke: Keystroke::parse("space").unwrap(),
            }),
            cx,
        );
        assert!(!released.propagate);
    });
    window.run_until_parked();
    view.update(window, |view, _| {
        assert!(view.quick_look.open);
        assert_eq!(view.browser.path(), directory.as_path());
        assert_eq!(view.browser.selected_path(), Some(first.as_path()));
    });

    window.simulate_keystrokes("right");
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            view.preview.state.path() == Some(second.as_path())
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(window, |view, _| {
        assert!(view.quick_look.open);
        assert_eq!(view.browser.selected_path(), Some(second.as_path()));
        assert_eq!(view.preview_navigation(&second).0, 2);
    });

    let info = window.debug_bounds("quick-look-info").unwrap().center();
    window.simulate_click(info, gpui::Modifiers::default());
    window.run_until_parked();
    assert!(window.debug_bounds("quick-look-info-drawer").is_some());

    window.update(|window, cx| {
        let pressed = window.dispatch_event(
            gpui::PlatformInput::KeyDown(KeyDownEvent {
                keystroke: Keystroke::parse("space").unwrap(),
                is_held: false,
                prefer_character_input: false,
            }),
            cx,
        );
        assert!(!pressed.propagate);
        let released = window.dispatch_event(
            gpui::PlatformInput::KeyUp(gpui::KeyUpEvent {
                keystroke: Keystroke::parse("space").unwrap(),
            }),
            cx,
        );
        assert!(!released.propagate);
    });
    window.run_until_parked();
    view.update(window, |view, _| {
        assert!(!view.quick_look.open);
        assert!(!view.quick_look.info_open);
        assert!(matches!(view.preview.state, PreviewState::Closed));
        assert_eq!(view.browser.path(), directory.as_path());
        assert_eq!(view.browser.selected_path(), Some(second.as_path()));
    });
    assert!(window.debug_bounds("quick-look-backdrop").is_none());

    view.update(window, |view, cx| {
        view.settings.view.show_preview_panel = true;
        view.browser.select(first.clone());
        view.sync_pinned_preview(cx);
        view.toggle_quick_look_selected(cx);
    });
    window.run_until_parked();
    assert!(window.debug_bounds("quick-look-modal").is_some());
    let close = window.debug_bounds("quick-look-close").unwrap().center();
    window.simulate_click(close, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert!(!view.quick_look.open);
        assert_eq!(view.preview.state.path(), Some(first.as_path()));
    });
    assert!(window.debug_bounds("preview-panel").is_some());

    view.update(window, |view, cx| view.toggle_quick_look_selected(cx));
    window.run_until_parked();
    let outside = gpui::point(px(4.0), backdrop.center().y);
    window.simulate_click(outside, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| assert!(!view.quick_look.open));

    remove_fixture(&directory);
}

#[gpui::test]
fn quick_look_preserves_multi_selection_and_uses_it_as_the_navigation_set(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let first = directory.join("alpha.txt");
    let skipped = directory.join("between.txt");
    let last = directory.join("omega.txt");
    fs::write(&first, "alpha preview").unwrap();
    fs::write(&skipped, "not selected").unwrap();
    fs::write(&last, "omega preview").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(directory.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });
    view.update(window, |view, cx| {
        view.install_shortcut_bindings(cx);
        view.browser.replace_entries(vec![
            absolute_entry(first.clone()),
            absolute_entry(skipped.clone()),
            absolute_entry(last.clone()),
        ]);
        view.browser
            .replace_selection(vec![first.clone(), last.clone()]);
        view.toggle_quick_look_selected(cx);
    });
    window.simulate_resize(gpui::size(px(900.0), px(640.0)));
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if window.debug_bounds("quick-look-index").is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    view.update(window, |view, _| {
        assert_eq!(view.quick_look.paths, vec![first.clone(), last.clone()]);
        assert_eq!(view.browser.selection_count(), 2);
        assert_eq!(view.preview_navigation(&first).1, 2);
    });
    let index = window.debug_bounds("quick-look-index").unwrap().center();
    window.simulate_click(index, gpui::Modifiers::default());
    window.run_until_parked();
    assert!(window.debug_bounds("quick-look-index-sheet").is_some());
    assert!(window.debug_bounds("quick-look-index-item-0").is_some());
    assert!(window.debug_bounds("quick-look-index-item-1").is_some());

    window.simulate_keystrokes("escape");
    window.run_until_parked();
    view.update(window, |view, _| assert!(view.quick_look.open));
    assert!(window.debug_bounds("quick-look-index-sheet").is_none());

    window.simulate_keystrokes("right");
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            view.preview.state.path() == Some(last.as_path())
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(window, |view, _| {
        assert_eq!(view.browser.selection_count(), 2);
        assert!(view.browser.is_selected(&first));
        assert!(view.browser.is_selected(&last));
    });

    remove_fixture(&directory);
}

#[gpui::test]
fn column_view_owns_a_terminal_preview_without_stealing_hierarchy_navigation(
    cx: &mut TestAppContext,
) {
    let directory = fixture_dir();
    let first = directory.join("a-very-long-file-name-for-auto-fit.txt");
    let second = directory.join("second.txt");
    fs::write(&first, "first preview").unwrap();
    fs::write(&second, "second preview").unwrap();
    let entries = vec![
        absolute_entry(first.clone()),
        absolute_entry(second.clone()),
    ];
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(directory.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });
    view.update(window, |view, cx| {
        view.browser.set_view_mode(ViewMode::Column);
        view.settings.view.show_preview_panel = true;
        view.browser.replace_entries(entries.clone());
        view.column_view.columns = ColumnState::new(&directory);
        assert!(view.column_view.columns.apply_listed(&directory, entries));
        view.browser.select(first.clone());
        view.set_column_selection(first.clone());
        view.sync_pinned_preview(cx);
        view.auto_fit_column_view_width(directory.clone(), cx);
    });
    window.simulate_resize(gpui::size(px(1100.0), px(720.0)));
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if window.debug_bounds("column-preview").is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(window.debug_bounds("column-preview").is_some());
    assert!(window.debug_bounds("preview-panel").is_none());
    window.simulate_resize(gpui::size(px(800.0), px(720.0)));
    window.run_until_parked();
    let strip = view.update(window, |view, _| view.column_view.strip_scroll.clone());
    let initial_max_offset = strip.max_offset().x;
    assert!(initial_max_offset > px(0.0));
    view.update(window, |view, cx| {
        let offset = view.column_view.strip_scroll.offset();
        view.column_view
            .strip_scroll
            .set_offset(gpui::point(-initial_max_offset, offset.y));
        cx.notify();
    });
    window.run_until_parked();

    let resizer = window.debug_bounds("preview-panel-resizer").unwrap();
    let start = resizer.center();
    let end = gpui::point(start.x - px(100.0), start.y);
    window.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_move(end, Some(MouseButton::Left), gpui::Modifiers::default());
    window.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::default());
    window.run_until_parked();

    view.update(window, |view, _| {
        assert_eq!(view.layout.preview_panel_width, 460.0);
    });
    let final_max_offset = strip.max_offset().x;
    assert!(final_max_offset > initial_max_offset);
    assert_eq!(strip.offset().x, -final_max_offset);
    assert_eq!(
        window.debug_bounds("column-preview").unwrap().right(),
        strip.bounds().right()
    );
    view.update(window, |view, cx| {
        assert!(
            view.browser
                .column_view_width(&directory)
                .is_some_and(|width| f32::from(width) > DEFAULT_COLUMN_VIEW_WIDTH)
        );
        assert_eq!(view.preview.state.path(), Some(first.as_path()));
        view.column_right(cx);
        assert_eq!(view.preview.state.path(), Some(first.as_path()));
        assert_eq!(view.browser.path(), directory.as_path());
    });

    remove_fixture(&directory);
}

#[gpui::test]
fn native_photo_metadata_renders_with_private_gps_disclosure(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let photo = directory.join("photo.jpg");
    fs::write(&photo, [0xff, 0xd8, 0xff, 0xd9]).unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    let metadata = ImageMetadata {
        camera: Some("Canon EOS R5".to_string()),
        taken_at: Some("2024:03:15 14:30:00".to_string()),
        width: Some(4032),
        height: Some(3024),
        caption: Some("Harbor at dusk".to_string()),
        keywords: vec!["ocean".to_string(), "travel".to_string()],
        gps: Some(explorie_native_services::ImageGps {
            latitude: 37.5,
            longitude: -122.25,
        }),
    };

    view.update(cx, |view, cx| {
        view.browser
            .replace_entries(vec![absolute_entry(photo.clone())]);
        view.browser.select(photo.clone());
        view.preview.state = PreviewState::Ready {
            path: photo.clone(),
            content: PreviewContent::Archive,
        };
        view.preview.tab = PreviewTab::Metadata;
        view.preview.photo_metadata = PhotoMetadataState::Ready {
            path: photo.clone(),
            metadata: metadata.clone(),
        };
        assert!(!view.preview.photo_gps_revealed);
        assert!(
            photo_metadata_rows(&metadata, false)
                .iter()
                .all(|(label, _)| !matches!(*label, "Latitude" | "Longitude"))
        );
        cx.notify();
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    view.update(cx, |view, cx| {
        view.toggle_photo_gps(cx);
        assert!(view.preview.photo_gps_revealed);
        let PhotoMetadataState::Ready { metadata, .. } = &view.preview.photo_metadata else {
            panic!("photo metadata should remain ready");
        };
        assert!(
            photo_metadata_rows(metadata, true)
                .iter()
                .any(|(label, value)| *label == "Latitude" && value == "37.500000")
        );
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    view.update(cx, |view, cx| {
        let second = directory.join("second.jpg");
        fs::write(&second, [0xff, 0xd8, 0xff, 0xd9]).unwrap();
        view.browser
            .replace_entries(vec![absolute_entry(second.clone())]);
        view.browser.select(second.clone());
        view.start_preview(second, cx);
        assert!(!view.preview.photo_gps_revealed);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            !matches!(
                view.preview.photo_metadata,
                PhotoMetadataState::Loading { .. }
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    remove_fixture(&directory);
}

#[gpui::test]
fn native_finder_tags_load_render_add_remove_and_retain_failed_edits(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let file = directory.join("tagged.txt");
    fs::write(&file, "local Finder tag fixture").unwrap();
    let backend = FakeFinderTagsBackend::default();
    backend.state.lock().unwrap().tags = vec!["Important\n6".to_string(), "Plain".to_string()];
    let services = NativeServices::with_finder_tags_backend(
        ResourcePaths::test(&directory),
        Arc::new(backend.clone()),
    );
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(directory.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });

    view.update(window, |view, cx| {
        view.browser
            .replace_entries(vec![absolute_entry(file.clone())]);
        view.browser.select(file.clone());
        view.settings.view.show_preview_panel = true;
        view.preview_selected(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            !view.preview.finder_tags.loading && view.preview.finder_tags.tags.len() == 2
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(window, |view, cx| {
        assert_eq!(
            view.preview.finder_tags.path.as_deref(),
            Some(file.as_path())
        );
        assert_eq!(view.preview.finder_tags.tags[0].name, "Important");
        assert_eq!(view.preview.finder_tags.tags[0].color_index, 6);
        view.set_preview_tab(PreviewTab::Metadata, cx);
    });
    window.simulate_resize(gpui::size(px(1200.0), px(720.0)));
    window.run_until_parked();
    assert!(
        window.debug_bounds("finder-tags").is_some(),
        "Finder tags should render inside metadata (panel: {:?}, metadata: {:?})",
        window.debug_bounds("preview-panel"),
        window.debug_bounds("preview-metadata")
    );
    assert!(window.debug_bounds("finder-tag-0").is_some());
    // The metadata scrolls within the whole inspector instead of a short
    // fixed-height box that clips the tag editor.
    let panel = window.debug_bounds("preview-panel").unwrap();
    let metadata = window.debug_bounds("preview-metadata").unwrap();
    assert!(
        metadata.bottom() >= panel.bottom() - px(2.0),
        "metadata {metadata:?} should reach the bottom of the inspector {panel:?}"
    );

    assert!(window.debug_bounds("add-finder-tag").is_some());
    view.update(window, |view, cx| view.begin_add_finder_tag(cx));
    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    assert!(window.debug_bounds("finder-tag-editor").is_some());
    window.simulate_keystrokes("r e v i e w");
    view.update(window, |view, cx| {
        for _ in 0..4 {
            view.cycle_finder_tag_color(1, cx);
        }
    });
    window.simulate_keystrokes("enter");
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            !view.preview.finder_tags.saving && view.preview.finder_tags.tags.len() == 3
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(window, |view, _| {
        assert!(view.preview.finder_tags.editor.is_none());
        assert_eq!(view.preview.finder_tags.tags[2].raw, "review\n4");
        assert_eq!(view.status_message.as_deref(), Some("Finder tags updated"));
    });
    assert_eq!(
        backend.state.lock().unwrap().tags,
        vec![
            "Important\n6".to_string(),
            "Plain".to_string(),
            "review\n4".to_string()
        ]
    );

    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    assert!(window.debug_bounds("remove-finder-tag-0").is_some());
    view.update(window, |view, cx| {
        view.remove_finder_tag("Important\n6", cx)
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            !view.preview.finder_tags.saving && view.preview.finder_tags.tags.len() == 2
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        backend.state.lock().unwrap().tags,
        vec!["Plain".to_string(), "review\n4".to_string()]
    );

    backend.state.lock().unwrap().fail_next_set = true;
    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    assert!(window.debug_bounds("add-finder-tag").is_some());
    view.update(window, |view, cx| view.begin_add_finder_tag(cx));
    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    window.simulate_keystrokes("d e n i e d enter");
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            !view.preview.finder_tags.saving && view.preview.finder_tags.error.is_some()
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(window, |view, _| {
        assert!(view.preview.finder_tags.editor.is_some());
        assert!(
            view.preview
                .finder_tags
                .error
                .as_deref()
                .is_some_and(|error| error.contains("fake Finder tag write denied"))
        );
    });
    assert_eq!(backend.state.lock().unwrap().set_calls, 3);
    assert_eq!(
        backend.state.lock().unwrap().tags,
        vec!["Plain".to_string(), "review\n4".to_string()]
    );
    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    assert!(window.debug_bounds("finder-tag-editor").is_some());

    remove_fixture(&directory);
}

#[gpui::test]
fn rendered_metadata_actions_dispatch_native_calls_and_retain_denied_status(
    cx: &mut TestAppContext,
) {
    let directory = fixture_dir();
    let file = directory.join("platform-actions.txt");
    fs::write(&file, "native platform action fixture").unwrap();
    let backend = FakePlatformActionsBackend::default();
    let services = NativeServices::with_platform_actions_backend(
        ResourcePaths::test(&directory),
        Arc::new(backend.clone()),
    );
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(directory.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });

    view.update(window, |view, cx| {
        view.browser
            .replace_entries(vec![metadata_entry(file.clone())]);
        view.browser.select(file.clone());
        view.settings.view.show_preview_panel = true;
        view.preview_selected(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            view.preview.state.path() == Some(file.as_path())
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(window, |view, cx| {
        view.set_preview_tab(PreviewTab::Metadata, cx)
    });
    window.simulate_resize(gpui::size(px(1200.0), px(720.0)));
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if window.debug_bounds("preview-metadata").is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(window.debug_bounds("preview-metadata").is_some());

    for (selector, expected_count) in [("metadata-open", 1), ("metadata-reveal", 2)] {
        let point = window
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} should render"))
            .center();
        window.simulate_click(point, gpui::Modifiers::default());
        let poll_deadline = Instant::now() + POLL_TIMEOUT;
        while Instant::now() < poll_deadline {
            window.run_until_parked();
            if backend.state.lock().unwrap().actions.len() >= expected_count {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[cfg(windows)]
    {
        let point = window
            .debug_bounds("metadata-open-with")
            .expect("Open with should render on Windows")
            .center();
        window.simulate_click(point, gpui::Modifiers::default());
    }
    let expected_successes = if cfg!(windows) { 3 } else { 2 };
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if backend.state.lock().unwrap().actions.len() >= expected_successes {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let expected_third = if cfg!(windows) {
        Some(RecordedPlatformAction::OpenWith(
            file.clone(),
            String::new(),
        ))
    } else {
        None
    };
    let actions = backend.state.lock().unwrap().actions.clone();
    assert_eq!(actions[0], RecordedPlatformAction::Open(file.clone()));
    assert_eq!(actions[1], RecordedPlatformAction::Reveal(file.clone()));
    if let Some(expected) = expected_third {
        assert_eq!(actions[2], expected);
    }
    let native_action_count = actions.len();
    drop(actions);

    let quick_look = window
        .debug_bounds("metadata-quick-look")
        .expect("Explorie Quick Look should render on every platform")
        .center();
    window.simulate_click(quick_look, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, cx| {
        assert!(view.quick_look.open);
        assert_eq!(
            backend.state.lock().unwrap().actions.len(),
            native_action_count
        );
        view.settings.view.show_preview_panel = true;
        view.close_quick_look(cx);
    });

    view.update(window, |view, _| {
        view.settings.behavior.enable_error_reporting = true;
    });
    backend.state.lock().unwrap().fail_next = Some("reveal");
    let reveal = window
        .debug_bounds("metadata-reveal")
        .expect("Reveal should remain rendered")
        .center();
    window.simulate_click(reveal, gpui::Modifiers::default());
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            view.status_message
                .as_deref()
                .is_some_and(|message| message.contains("fake reveal denied"))
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(window, |view, _| {
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|message| message.contains("Unable to reveal")
                    && message.contains("fake reveal denied"))
        );
    });
    assert_eq!(
        backend.state.lock().unwrap().actions.len(),
        expected_successes,
        "denied actions must not be recorded as successful side effects"
    );

    view.update(window, |view, cx| {
        assert_eq!(view.error_reports.len(), 1);
        view.open_control_surface(ControlSurface::Diagnostics, cx);
    });
    window.run_until_parked();
    assert!(window.debug_bounds("error-report-0").is_some());
    assert!(window.debug_bounds("copy-error-reports").is_some());
    assert!(window.debug_bounds("clear-error-reports").is_some());

    let copy = window.debug_bounds("copy-error-reports").unwrap().center();
    window.simulate_click(copy, gpui::Modifiers::default());
    let error_json = window
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("error-report clipboard text");
    assert!(error_json.contains("Reveal failed"));
    assert!(error_json.contains("fake reveal denied"));
    assert!(!error_json.contains(&directory.display().to_string()));

    let clear = window.debug_bounds("clear-error-reports").unwrap().center();
    window.simulate_click(clear, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| assert_eq!(view.error_reports.len(), 0));
    assert!(window.debug_bounds("error-reports-empty").is_some());

    remove_fixture(&directory);
}

#[gpui::test]
fn native_context_menu_matches_core_legacy_actions_across_all_views(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let first = directory.join("a.txt");
    let archive = directory.join("b.zip");
    let folder = directory.join("folder");
    let second = directory.join("z.txt");
    fs::write(&first, "first").unwrap();
    fs::write(&archive, b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0").unwrap();
    fs::create_dir(&folder).unwrap();
    fs::write(&second, "second").unwrap();
    let mut folder_entry = metadata_entry(folder.clone());
    folder_entry.is_dir = true;
    let entries = vec![
        metadata_entry(first.clone()),
        metadata_entry(archive.clone()),
        folder_entry,
        metadata_entry(second.clone()),
    ];
    let backend = FakePlatformActionsBackend::default();
    let services = NativeServices::with_platform_actions_backend(
        ResourcePaths::test(&directory),
        Arc::new(backend.clone()),
    );
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(directory.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });
    view.update(window, |view, cx| {
        view.browser.replace_entries(entries.clone());
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    window.simulate_resize(gpui::size(px(1200.0), px(720.0)));
    window.run_until_parked();

    let first_index = view.update(window, |view, _| {
        view.browser
            .visible_entries()
            .iter()
            .position(|entry| entry.path == first)
            .unwrap()
    });
    let first_point = window
        .debug_bounds(Box::leak(format!("entry-{first_index}").into_boxed_str()))
        .unwrap()
        .center();
    window.simulate_mouse_down(first_point, MouseButton::Right, gpui::Modifiers::default());
    window.simulate_mouse_up(first_point, MouseButton::Right, gpui::Modifiers::default());
    assert!(window.debug_bounds("file-context-menu").is_some());
    for selector in [
        "context-menu-open",
        "context-menu-preview",
        "context-menu-rename",
        "context-menu-copy",
        "context-menu-cut",
        "context-menu-delete",
        "context-menu-reveal",
        "context-menu-compress",
    ] {
        assert!(
            window.debug_bounds(selector).is_some(),
            "{selector} should render for one file"
        );
    }
    assert!(window.debug_bounds("context-menu-quick-look").is_none());
    #[cfg(windows)]
    assert!(window.debug_bounds("context-menu-open-with").is_some());
    #[cfg(target_os = "macos")]
    {
        let poll_deadline = Instant::now() + POLL_TIMEOUT;
        while Instant::now() < poll_deadline {
            window.run_until_parked();
            if view.update(window, |view, _| {
                view.context_menu
                    .menu
                    .as_ref()
                    .is_some_and(|menu| !menu.open_with_apps.is_empty())
            }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        window.draw(
            gpui::point(px(0.0), px(0.0)),
            gpui::size(px(1200.0), px(720.0)),
            |_, _| view.clone().into_element(),
        );
        assert!(
            window
                .debug_bounds("context-menu-open-with-toggle")
                .is_some()
        );
        view.update(window, |view, cx| {
            view.execute_context_menu_action(ContextMenuAction::ToggleOpenWith, cx)
        });
        window.draw(
            gpui::point(px(0.0), px(0.0)),
            gpui::size(px(1200.0), px(720.0)),
            |_, _| view.clone().into_element(),
        );
        assert!(window.debug_bounds("context-menu-open-with-app").is_some());
    }

    backend.state.lock().unwrap().actions.clear();

    let reveal = window.debug_bounds("context-menu-reveal").unwrap().center();
    window.simulate_click(reveal, gpui::Modifiers::default());
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if !backend.state.lock().unwrap().actions.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        backend.state.lock().unwrap().actions,
        vec![RecordedPlatformAction::Reveal(first.clone())]
    );
    view.update(window, |view, _| assert!(view.context_menu.menu.is_none()));
    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    window.simulate_mouse_down(first_point, MouseButton::Right, gpui::Modifiers::default());
    window.simulate_mouse_up(first_point, MouseButton::Right, gpui::Modifiers::default());
    window.simulate_keystrokes("down down down enter");
    view.update(window, |view, _| {
        let clipboard = view
            .clipboard
            .state
            .as_ref()
            .expect("keyboard Copy should run");
        assert_eq!(clipboard.kind, ClipboardKind::Copy);
        assert_eq!(clipboard.paths.as_slice(), std::slice::from_ref(&first));
    });
    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );

    view.update(window, |view, cx| {
        view.browser.select(first.clone());
        view.browser.toggle_selection(second.clone());
        cx.notify();
    });
    window.run_until_parked();
    window.simulate_mouse_down(first_point, MouseButton::Right, gpui::Modifiers::default());
    window.simulate_mouse_up(first_point, MouseButton::Right, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(view.browser.selection_count(), 2)
    });
    assert!(window.debug_bounds("context-menu-batch-rename").is_some());
    let copy = window.debug_bounds("context-menu-copy").unwrap().center();
    window.simulate_click(copy, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(view.clipboard.state.as_ref().unwrap().paths.len(), 2)
    });
    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );

    let folder_index = view.update(window, |view, _| {
        view.browser
            .visible_entries()
            .iter()
            .position(|entry| entry.path == folder)
            .unwrap()
    });
    let folder_selector = Box::leak(format!("entry-{folder_index}").into_boxed_str());
    let folder_point = window.debug_bounds(folder_selector).unwrap().center();
    window.simulate_mouse_down(folder_point, MouseButton::Right, gpui::Modifiers::default());
    window.simulate_mouse_up(folder_point, MouseButton::Right, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(view.browser.selection_count(), 1);
        assert!(view.browser.is_selected(&folder));
    });
    let favorite = window
        .debug_bounds("context-menu-favorite")
        .unwrap()
        .center();
    window.simulate_click(favorite, gpui::Modifiers::default());
    view.update(window, |view, _| assert!(view.browser.is_favorite(&folder)));
    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );

    let listing = window.debug_bounds("listing-drop-surface").unwrap();
    let empty_point = gpui::point(listing.right() - px(4.0), listing.bottom() - px(4.0));
    window.simulate_mouse_down(empty_point, MouseButton::Right, gpui::Modifiers::default());
    window.simulate_mouse_up(empty_point, MouseButton::Right, gpui::Modifiers::default());
    assert!(window.debug_bounds("context-menu-paste").is_some());
    view.update(window, |view, _| {
        assert!(
            view.context_menu
                .menu
                .as_ref()
                .is_some_and(|menu| menu.paths.is_empty())
        );
        assert_eq!(
            view.context_menu_actions(),
            vec![(ContextMenuAction::Paste, false)]
        );
    });
    window.simulate_keystrokes("escape");
    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    view.update(window, |view, _| assert!(view.context_menu.menu.is_none()));

    view.update(window, |view, cx| {
        view.browser.set_view_mode(ViewMode::Grid);
        cx.notify();
    });
    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    let archive_index = view.update(window, |view, _| {
        view.browser
            .visible_entries()
            .iter()
            .position(|entry| entry.path == archive)
            .unwrap()
    });
    let grid_selector = Box::leak(format!("grid-entry-{archive_index}").into_boxed_str());
    let grid_point = window.debug_bounds(grid_selector).unwrap().center();
    window.simulate_mouse_down(grid_point, MouseButton::Right, gpui::Modifiers::default());
    window.simulate_mouse_up(grid_point, MouseButton::Right, gpui::Modifiers::default());
    view.update(window, |view, _| {
        let actions = view.context_menu_actions();
        assert!(
            actions
                .iter()
                .any(|(action, _)| *action == ContextMenuAction::InspectArchive)
        );
        assert!(
            actions
                .iter()
                .any(|(action, _)| *action == ContextMenuAction::ExtractArchive)
        );
    });
    window.simulate_keystrokes("escape");

    let column_root = PathBuf::from("column-context-menu");
    view.update(window, |view, cx| {
        view.browser.set_view_mode(ViewMode::Column);
        view.column_view.columns = ColumnState::new(&column_root);
        assert!(view.column_view.columns.apply_listed(&column_root, entries));
        cx.notify();
    });
    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    let column_index = view.update(window, |view, _| {
        view.column_view.columns.columns()[0]
            .visible_entries(&view.browser)
            .iter()
            .position(|entry| entry.path == second)
            .unwrap()
    });
    let column_selector = Box::leak(format!("column-entry-0-{column_index}").into_boxed_str());
    let column_point = window.debug_bounds(column_selector).unwrap().center();
    window.simulate_mouse_down(column_point, MouseButton::Right, gpui::Modifiers::default());
    window.simulate_mouse_up(column_point, MouseButton::Right, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(
            view.context_menu.menu.as_ref().unwrap().paths.as_slice(),
            std::slice::from_ref(&second)
        );
        assert!(
            view.context_menu_actions()
                .iter()
                .any(|(action, _)| *action == ContextMenuAction::Preview)
        );
    });
    window.simulate_keystrokes("escape");

    let ancestor_root = PathBuf::from("ancestor-context-menu");
    let ancestor_folder = ancestor_root.join("child");
    let mut ancestor_entry = absolute_entry(ancestor_folder.clone());
    ancestor_entry.is_dir = true;
    let ancestor_file = absolute_entry(ancestor_folder.join("base.txt"));
    view.update(window, |view, cx| {
        view.column_view.columns = ColumnState::new(&ancestor_folder);
        assert!(
            view.column_view
                .columns
                .apply_listed(&ancestor_root, vec![ancestor_entry])
        );
        assert!(
            view.column_view
                .columns
                .apply_listed(&ancestor_folder, vec![ancestor_file])
        );
        cx.notify();
    });
    window.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1200.0), px(720.0)),
        |_, _| view.clone().into_element(),
    );
    let ancestor_point = window.debug_bounds("column-entry-0-0").unwrap().center();
    window.simulate_mouse_down(
        ancestor_point,
        MouseButton::Right,
        gpui::Modifiers::default(),
    );
    window.simulate_mouse_up(
        ancestor_point,
        MouseButton::Right,
        gpui::Modifiers::default(),
    );
    view.update(window, |view, _| {
        assert_eq!(
            view.context_menu.menu.as_ref().unwrap().paths.as_slice(),
            std::slice::from_ref(&ancestor_folder)
        );
    });
    let ancestor_copy = window.debug_bounds("context-menu-copy").unwrap().center();
    window.simulate_click(ancestor_copy, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(
            view.clipboard.state.as_ref().unwrap().paths.as_slice(),
            std::slice::from_ref(&ancestor_folder)
        );
    });

    remove_fixture(&directory);
}

#[gpui::test]
fn native_website_link_prompt_collects_name_and_validated_url(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.prompt_new_website_link(cx);
        view.mutation.prompt.as_mut().unwrap().input = "OpenAI".to_string();
        view.submit_mutation_prompt(cx);
        assert!(matches!(
            view.mutation.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(MutationPromptKind::NewWebsiteLinkUrl { .. })
        ));
        view.mutation.prompt.as_mut().unwrap().input = "https://openai.com".to_string();
        view.submit_mutation_prompt(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| !view.mutation.in_progress) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(
        fs::read_to_string(directory.join("OpenAI.url")).unwrap(),
        "[InternetShortcut]\nURL=https://openai.com\n"
    );
    view.update(cx, |view, _| {
        assert!(view.mutation.prompt.is_none());
        assert!(view.undo_ledger.can_undo(SystemTime::now()));
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn native_settings_import_applies_and_persists_browser_preferences(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let resources = explorie_native_services::ResourcePaths::test(&directory);
    fs::create_dir_all(&resources.config_dir).unwrap();
    let legacy = std::collections::BTreeMap::from([
        ("explorie:viewMode".to_string(), "grid".to_string()),
        ("explorie:showHidden".to_string(), "true".to_string()),
        ("explorie:filterMode".to_string(), "files".to_string()),
        ("explorie:showFolderSizes".to_string(), "true".to_string()),
        ("explorie:sortKey".to_string(), "size".to_string()),
        ("explorie:sortDir".to_string(), "desc".to_string()),
        ("explorie:listRowHeight".to_string(), "52".to_string()),
        (
            "explorie:confirmBeforeDelete".to_string(),
            "false".to_string(),
        ),
    ]);
    fs::write(
        resources.config_dir.join("legacy-local-storage.json"),
        serde_json::to_vec_pretty(&legacy).unwrap(),
    )
    .unwrap();
    let services = NativeServices::new(resources.clone());
    let (view, _window) = cx
        .add_window_view(|_, cx| DirectoryWindow::restore(directory.clone(), false, services, cx));

    view.update(cx, |view, cx| {
        assert_eq!(view.browser.view_mode(), ViewMode::Grid);
        assert!(view.browser.show_hidden());
        assert_eq!(view.browser.filter(), EntryFilter::Files);
        assert_eq!(view.browser.sort_key(), SortKey::Size);
        assert_eq!(view.browser.sort_direction(), SortDirection::Descending);
        assert!(view.calculate_folder_sizes);
        assert_eq!(view.settings.appearance.list_row_height, 52);
        assert!(!view.settings.behavior.confirm_before_delete);
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|message| message.contains("Imported 8"))
        );
        view.toggle_hidden(cx);
        view.cycle_filter(cx);
        view.set_view_mode(ViewMode::List, cx);
        view.settings_store.as_ref().unwrap().flush();
    });

    let settings_path = resources.config_dir.join("settings-v1.json");
    let mut persisted = None;
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        persisted = fs::read(&settings_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<AppSettings>(&bytes).ok())
            .filter(|settings| settings.view.view_mode == ViewMode::List);
        if persisted.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let persisted = persisted.expect("settings writer did not persist the updated preferences");
    assert!(!persisted.view.show_hidden);
    assert_eq!(persisted.view.filter_mode, EntryFilter::All);
    assert_eq!(persisted.view.view_mode, ViewMode::List);
    assert_eq!(persisted.legacy_values, legacy);
    remove_fixture(&directory);
}

#[gpui::test]
fn native_settings_controls_persist_across_restart_and_render_light_high_contrast(
    cx: &mut TestAppContext,
) {
    let directory = fixture_dir();
    let resources = explorie_native_services::ResourcePaths::test(&directory);
    let services = NativeServices::new(resources.clone());
    let (view, cx) = cx
        .add_window_view(|_, cx| DirectoryWindow::restore(directory.clone(), false, services, cx));

    view.update(cx, |view, cx| {
        view.toggle_settings_panel(cx);
        view.cycle_theme(cx);
        view.cycle_accent(cx);
        view.cycle_density(cx);
        view.cycle_ui_scale(cx);
        view.cycle_list_row_height(cx);
        view.cycle_grid_width(cx);
        view.cycle_font(cx);
        view.cycle_border_radius(cx);
        view.cycle_icon_size(cx);
        view.cycle_undo_timeout(cx);
        view.toggle_system_files(cx);
        view.toggle_status_bar(cx);
        view.toggle_preview_panel(cx);
        view.toggle_confirm_delete(cx);
        view.toggle_script_preview(cx);
        view.toggle_error_reporting(cx);
        view.toggle_remote_drives(cx);
        view.toggle_reduce_motion(cx);
        view.toggle_high_contrast(cx);
        view.settings_store.as_ref().unwrap().flush();
    });

    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    let saved: AppSettings =
        serde_json::from_slice(&fs::read(resources.config_dir.join("settings-v1.json")).unwrap())
            .unwrap();
    assert_eq!(saved.appearance.theme, ThemeMode::Light);
    assert_eq!(saved.appearance.accent, AccentColor::Green);
    assert_eq!(saved.appearance.density, Density::Compact);
    assert!(saved.appearance.high_contrast);
    assert!(saved.view.show_system_files);
    assert!(saved.view.show_preview_panel);
    assert!(!saved.view.show_status_bar);
    assert_eq!(saved.behavior.undo_timeout_minutes, 15);

    let restart_services = NativeServices::new(resources.clone());
    let (restarted, restarted_cx) = cx.add_window_view(|_, cx| {
        DirectoryWindow::restore(directory.clone(), false, restart_services, cx)
    });
    restarted.update(restarted_cx, |view, _| {
        assert_eq!(view.settings, saved);
        assert!(view.browser.show_system_files());
        assert!(!view.settings_ui.panel_open);
        assert_eq!(view.settings.appearance.theme, ThemeMode::Light);
        assert!(view.settings.appearance.high_contrast);
        assert!(!view.settings.behavior.confirm_before_delete);
        assert!(view.settings.behavior.preview_executable_scripts);
        assert!(view.settings.behavior.enable_error_reporting);
        assert!(view.settings.behavior.remote_drives_enabled);
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn sidebar_pointer_and_keyboard_resize_persist_globally_across_restart(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let resources = ResourcePaths::test(&directory);
    let services = NativeServices::new(resources.clone());
    let (view, window) = cx
        .add_window_view(|_, cx| DirectoryWindow::restore(directory.clone(), false, services, cx));
    window.simulate_resize(gpui::size(px(900.0), px(650.0)));
    window.run_until_parked();

    let resizer = window
        .debug_bounds("sidebar-resizer")
        .expect("native sidebar resizer");
    let start = gpui::point(resizer.center().x, resizer.center().y);
    let end = gpui::point(start.x + px(70.0), start.y);
    window.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_move(end, Some(MouseButton::Left), gpui::Modifiers::default());
    window.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(view.layout.sidebar_width, 290.0);
        assert_eq!(view.settings.view.sidebar_width, 290.0);
        assert!(view.pointer.sidebar_resize.is_none());
        view.settings_store.as_ref().unwrap().flush();
    });

    window.simulate_keystrokes("left");
    view.update(window, |view, _| {
        assert_eq!(view.layout.sidebar_width, 280.0);
        assert_eq!(view.settings.view.sidebar_width, 280.0);
        view.settings_store.as_ref().unwrap().flush();
    });
    window.run_until_parked();
    let resized = window.debug_bounds("sidebar-resizer").unwrap();
    assert_eq!(f32::from(resized.left()), 272.0);
    assert_eq!(f32::from(resized.right()), 280.0);

    let saved: AppSettings =
        serde_json::from_slice(&fs::read(resources.config_dir.join("settings-v1.json")).unwrap())
            .unwrap();
    assert_eq!(saved.view.sidebar_width, 280.0);

    let restart_services = NativeServices::new(resources);
    let (restarted, restarted_window) = cx.add_window_view(|_, cx| {
        DirectoryWindow::restore(directory.clone(), false, restart_services, cx)
    });
    restarted_window.simulate_resize(gpui::size(px(900.0), px(650.0)));
    restarted_window.run_until_parked();
    restarted.update(restarted_window, |view, _| {
        assert_eq!(view.layout.sidebar_width, 280.0);
        assert_eq!(view.settings.view.sidebar_width, 280.0);
    });
    assert_eq!(
        f32::from(
            restarted_window
                .debug_bounds("sidebar-resizer")
                .unwrap()
                .left()
        ),
        272.0
    );
    assert_eq!(
        f32::from(
            restarted_window
                .debug_bounds("sidebar-resizer")
                .unwrap()
                .right()
        ),
        280.0
    );
    remove_fixture(&directory);
}

#[gpui::test]
fn custom_accent_and_font_validate_render_persist_and_cancel(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("escape", ClearSelection, Some("browser")),
            KeyBinding::new("enter", OpenSelected, Some("browser")),
        ]);
    });
    let directory = fixture_dir();
    let resources = ResourcePaths::test(&directory);
    let services = NativeServices::new(resources.clone());
    let (view, window) = cx
        .add_window_view(|_, cx| DirectoryWindow::restore(directory.clone(), false, services, cx));
    window.simulate_resize(gpui::size(px(900.0), px(650.0)));
    let focus = view.update(window, |view, cx| {
        view.settings_ui.panel_open = true;
        view.settings.appearance.accent = AccentColor::Pink;
        view.cycle_accent(cx);
        view.focus_handle.clone()
    });
    window.update(|window, cx| window.focus(&focus, cx));
    window.run_until_parked();
    assert!(window.debug_bounds("appearance-value-editor").is_some());
    assert!(window.debug_bounds("appearance-value-input").is_some());

    view.update(window, |view, cx| {
        let editor = view.settings_ui.appearance_value_editor.as_mut().unwrap();
        editor.input = "#12".to_string();
        view.submit_appearance_value_editor(cx);
        assert!(
            view.settings_ui
                .appearance_value_editor
                .as_ref()
                .unwrap()
                .error
                .is_some()
        );
        assert_eq!(view.settings.appearance.accent, AccentColor::Pink);

        view.settings_ui
            .appearance_value_editor
            .as_mut()
            .unwrap()
            .input = "#34a853".to_string();
        view.submit_appearance_value_editor(cx);
        assert!(view.settings_ui.appearance_value_editor.is_none());
        assert_eq!(view.settings.appearance.accent, AccentColor::Custom);
        assert_eq!(view.settings.appearance.accent_custom, "#34A853");
        assert_eq!(
            UiPalette::for_settings(&view.settings, WindowAppearance::Dark).accent,
            rgb(0x34a853)
        );

        view.settings.appearance.font = FontChoice::Serif;
        view.cycle_font(cx);
        assert!(matches!(
            view.settings_ui
                .appearance_value_editor
                .as_ref()
                .map(|editor| editor.kind),
            Some(AppearanceValueKind::Font)
        ));
        view.settings_ui
            .appearance_value_editor
            .as_mut()
            .unwrap()
            .input = "Segoe UI Variable".to_string();
    });
    window.run_until_parked();
    window.simulate_keystrokes("enter");
    view.update(window, |view, _| {
        assert_eq!(view.settings.appearance.font, FontChoice::Custom);
        assert_eq!(font_family(&view.settings), "Segoe UI Variable");
        view.settings_store.as_ref().unwrap().flush();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("settings-panel").is_some());

    let saved: AppSettings =
        serde_json::from_slice(&fs::read(resources.config_dir.join("settings-v1.json")).unwrap())
            .unwrap();
    assert_eq!(saved.appearance.accent, AccentColor::Custom);
    assert_eq!(saved.appearance.accent_custom, "#34A853");
    assert_eq!(saved.appearance.font, FontChoice::Custom);
    assert_eq!(saved.appearance.font_custom, "Segoe UI Variable");

    let restart_services = NativeServices::new(resources);
    let (restarted, restarted_window) = cx.add_window_view(|_, cx| {
        DirectoryWindow::restore(directory.clone(), false, restart_services, cx)
    });
    let focus = restarted.update(restarted_window, |view, cx| {
        assert_eq!(view.settings, saved);
        view.settings_ui.panel_open = true;
        view.cycle_accent(cx);
        view.settings_ui
            .appearance_value_editor
            .as_mut()
            .unwrap()
            .input = "#FF0000".to_string();
        view.focus_handle.clone()
    });
    restarted_window.update(|window, cx| window.focus(&focus, cx));
    restarted_window.run_until_parked();
    restarted_window.simulate_keystrokes("escape");
    restarted.update(restarted_window, |view, _| {
        assert!(view.settings_ui.appearance_value_editor.is_none());
        assert_eq!(view.settings.appearance.accent_custom, "#34A853");
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn named_themes_validate_import_export_apply_delete_and_restart(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("escape", ClearSelection, Some("browser")),
            KeyBinding::new("enter", OpenSelected, Some("browser")),
        ]);
    });
    let directory = fixture_dir();
    let resources = ResourcePaths::test(&directory);
    let services = NativeServices::new(resources.clone());
    let (view, window) = cx
        .add_window_view(|_, cx| DirectoryWindow::restore(directory.clone(), false, services, cx));
    let focus = view.update(window, |view, cx| {
        view.settings_ui.panel_open = true;
        view.settings_ui.tab = SettingsTab::Themes;
        view.settings.appearance.theme = ThemeMode::Light;
        view.settings.appearance.accent = AccentColor::Orange;
        view.settings.appearance.density = Density::Compact;
        view.settings.appearance.high_contrast = true;
        view.open_named_theme_editor(cx);
        view.settings_ui.named_theme_editor.as_mut().unwrap().input = "Default".to_string();
        view.submit_named_theme_editor(cx);
        assert!(view.settings.named_themes.is_empty());
        assert!(
            view.settings_ui
                .named_theme_editor
                .as_ref()
                .unwrap()
                .error
                .is_some()
        );
        view.settings_ui.named_theme_editor.as_mut().unwrap().input = "Night Shift".to_string();
        view.focus_handle.clone()
    });
    window.update(|window, cx| window.focus(&focus, cx));
    window.run_until_parked();
    assert!(window.debug_bounds("named-theme-editor").is_some());
    assert!(window.debug_bounds("named-theme-name-input").is_some());

    window.simulate_keystrokes("enter");
    view.update(window, |view, cx| {
        assert!(view.settings_ui.named_theme_editor.is_none());
        let saved = view
            .settings
            .named_themes
            .get("Night Shift")
            .unwrap()
            .clone();
        assert_eq!(saved.theme, ThemeMode::Light);
        assert_eq!(saved.accent, AccentColor::Orange);
        assert_eq!(saved.density, Density::Compact);

        view.copy_current_theme(cx);
        let exported = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap();
        assert!(!exported.contains("highContrast"));
        assert_eq!(serde_json::from_str::<ThemeSpec>(&exported).unwrap(), saved);

        view.settings.appearance = AppearanceSettings::default();
        view.settings.appearance.high_contrast = false;
        view.apply_named_theme("Night Shift", cx);
        assert_eq!(view.settings.appearance.theme, ThemeMode::Light);
        assert_eq!(view.settings.appearance.accent, AccentColor::Orange);
        assert!(!view.settings.appearance.high_contrast);

        view.settings.appearance.font = FontChoice::Serif;
        view.update_named_theme("Night Shift", cx);
        assert_eq!(
            view.settings.named_themes["Night Shift"].font,
            FontChoice::Serif
        );
        view.copy_all_named_themes(cx);
        let exported = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap();
        let all: BTreeMap<String, ThemeSpec> = serde_json::from_str(&exported).unwrap();
        assert_eq!(all, view.settings.named_themes);

        cx.write_to_clipboard(ClipboardItem::new_string("{not theme json".to_string()));
        view.import_named_themes_from_clipboard(cx);
        assert_eq!(view.settings.named_themes.len(), 1);

        let mut imported = ThemeSpec::from_appearance(&AppearanceSettings::default());
        imported.accent = AccentColor::Purple;
        cx.write_to_clipboard(ClipboardItem::new_string(
            serde_json::to_string(&imported).unwrap(),
        ));
        view.import_named_themes_from_clipboard(cx);
        assert_eq!(
            view.settings.named_themes["Imported theme"].accent,
            AccentColor::Purple
        );

        view.delete_named_theme("Night Shift", cx);
        assert!(!view.settings.named_themes.contains_key("Night Shift"));
        view.settings_store.as_ref().unwrap().flush();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("settings-theme-import").is_some());

    let persisted: AppSettings =
        serde_json::from_slice(&fs::read(resources.config_dir.join("settings-v1.json")).unwrap())
            .unwrap();
    assert_eq!(persisted.named_themes.len(), 1);
    assert_eq!(
        persisted.named_themes["Imported theme"].accent,
        AccentColor::Purple
    );

    let restart_services = NativeServices::new(resources);
    let (restarted, restarted_window) = cx.add_window_view(|_, cx| {
        DirectoryWindow::restore(directory.clone(), false, restart_services, cx)
    });
    let focus = restarted.update(restarted_window, |view, cx| {
        assert_eq!(view.settings, persisted);
        view.open_named_theme_editor(cx);
        view.settings_ui.named_theme_editor.as_mut().unwrap().input = "Discard me".to_string();
        view.focus_handle.clone()
    });
    restarted_window.update(|window, cx| window.focus(&focus, cx));
    restarted_window.run_until_parked();
    restarted_window.simulate_keystrokes("escape");
    restarted.update(restarted_window, |view, _| {
        assert!(view.settings_ui.named_theme_editor.is_none());
        assert_eq!(view.settings.named_themes, persisted.named_themes);
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn settings_keybinding_opens_the_native_panel_and_escape_closes_it(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("ctrl-,", ToggleSettingsPanel, Some("browser")),
            KeyBinding::new("escape", ClearSelection, Some("browser")),
        ]);
    });
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(PathBuf::from("sample"), NativeServices::default(), cx);
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(1040.0), px(720.0)));
    window.run_until_parked();

    window.simulate_keystrokes("ctrl-,");
    view.update(window, |view, _| assert!(view.settings_ui.panel_open));
    window.run_until_parked();

    let backdrop = window.debug_bounds("settings-backdrop").unwrap();
    let panel = window.debug_bounds("settings-panel").unwrap();
    let header = window.debug_bounds("settings-header").unwrap();
    let content = window.debug_bounds("settings-content").unwrap();
    let tabs = window.debug_bounds("settings-tabs").unwrap();
    let active = window.debug_bounds("settings-active-section").unwrap();
    let footer = window.debug_bounds("settings-footer").unwrap();
    assert_eq!(f32::from(backdrop.size.width), 1040.0);
    assert_eq!(f32::from(backdrop.size.height), 720.0);
    assert_eq!(f32::from(panel.size.width), 992.0);
    assert_eq!(f32::from(panel.size.height), 640.0);
    assert_eq!(f32::from(header.size.height), 46.0);
    assert_eq!(f32::from(footer.size.height), 46.0);
    assert_eq!(f32::from(tabs.size.width), 172.0);
    assert_eq!(panel.top(), px(40.0));
    assert_eq!(panel.left(), px(24.0));
    assert_eq!(header.top(), panel.top() + px(1.0));
    assert_eq!(header.bottom(), content.top());
    assert_eq!(content.bottom(), footer.top());
    assert_eq!(footer.bottom() + px(1.0), panel.bottom());
    assert!(active.left() > tabs.right());
    assert!(window.debug_bounds("settings-general").is_some());
    assert!(window.debug_bounds("settings-hidden").is_some());

    for (tab_index, section_selector, control_selector) in [
        (1, "settings-integration", "settings-refresh-helpers"),
        (2, "settings-plugins", "settings-plugins"),
        (3, "settings-appearance", "settings-theme-options"),
        (4, "settings-themes", "settings-theme-save"),
        (5, "settings-shortcuts", "settings-shortcuts-reset"),
        (6, "settings-about", "settings-update-action"),
    ] {
        let tab_selector = Box::leak(format!("settings-tab-{tab_index}").into_boxed_str());
        let target = window.debug_bounds(tab_selector).unwrap().center();
        window.simulate_click(target, gpui::Modifiers::default());
        window.run_until_parked();
        assert!(
            window.debug_bounds(section_selector).is_some(),
            "settings tab {tab_index} did not render {section_selector}"
        );
        assert!(
            window.debug_bounds(control_selector).is_some(),
            "settings tab {tab_index} did not expose {control_selector}"
        );
        if tab_index == 3 {
            let slider = window.debug_bounds("settings-ui-scale").unwrap();
            let track = window.debug_bounds("settings-ui-scale-track").unwrap();
            assert!(track.left() > slider.left());
            assert!(track.right() < slider.right());
            assert!(track.top() > slider.top());
            assert!(track.bottom() < slider.bottom());
        }
    }

    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();
    let compact_panel = window.debug_bounds("settings-panel").unwrap();
    let compact_tabs = window.debug_bounds("settings-tabs").unwrap();
    let compact_active = window.debug_bounds("settings-active-section").unwrap();
    assert_eq!(f32::from(compact_panel.size.width), 768.0);
    assert_eq!(f32::from(compact_panel.size.height), 568.0);
    assert_eq!(compact_panel.left(), px(16.0));
    assert_eq!(compact_panel.top(), px(16.0));
    assert_eq!(f32::from(compact_tabs.size.height), 48.0);
    assert!(compact_active.top() > compact_tabs.bottom());
    assert_eq!(compact_active.left(), compact_tabs.left());

    window.simulate_keystrokes("escape");
    view.update(window, |view, _| assert!(!view.settings_ui.panel_open));
    window.run_until_parked();
    assert!(window.debug_bounds("settings-panel").is_none());
}

#[gpui::test]
fn settings_scroll_does_not_reach_the_listing_behind_the_modal(cx: &mut TestAppContext) {
    let entries = (0..100)
        .map(|index| absolute_entry(PathBuf::from(format!("sample/{index:03}.txt"))))
        .collect::<Vec<_>>();
    let (view, window) = cx.add_window_view(|window, cx| {
        let mut view = DirectoryWindow::new(PathBuf::from("sample"), NativeServices::default(), cx);
        view.browser.replace_entries(entries);
        view.listing.state = ListingState::Ready;
        view.settings_ui.panel_open = true;
        view.settings_ui.tab = SettingsTab::Appearance;
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();

    let listing_scroll = view.update(window, |view, _| {
        view.listing.scroll_handle.0.borrow().base_handle.clone()
    });
    listing_scroll.set_offset(gpui::point(px(0.0), px(0.0)));
    let settings = window.debug_bounds("settings-active-section").unwrap();
    window.simulate_event(gpui::ScrollWheelEvent {
        position: settings.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-120.0))),
        ..Default::default()
    });

    assert_eq!(listing_scroll.offset().y, px(0.0));
}

#[gpui::test]
fn column_vertical_scroll_does_not_move_the_horizontal_strip(cx: &mut TestAppContext) {
    let root = PathBuf::from("column-scroll");
    let child = root.join("child");
    let leaf = child.join("leaf");
    let mut child_entry = absolute_entry(child.clone());
    child_entry.is_dir = true;
    let mut leaf_entry = absolute_entry(leaf.clone());
    leaf_entry.is_dir = true;
    let leaf_entries = (0..100)
        .map(|index| absolute_entry(leaf.join(format!("{index:03}.txt"))))
        .collect::<Vec<_>>();
    let (view, window) = cx.add_window_view(|window, cx| {
        let mut view = DirectoryWindow::new(leaf.clone(), NativeServices::default(), cx);
        view.browser.set_view_mode(ViewMode::Column);
        view.browser.replace_entries(leaf_entries.clone());
        view.column_view.columns = ColumnState::new(&leaf);
        assert!(
            view.column_view
                .columns
                .apply_listed(&root, vec![child_entry])
        );
        assert!(
            view.column_view
                .columns
                .apply_listed(&child, vec![leaf_entry])
        );
        assert!(view.column_view.columns.apply_listed(&leaf, leaf_entries));
        view.column_view.scroll_handles = view
            .column_view
            .columns
            .columns()
            .iter()
            .map(|_| UniformListScrollHandle::new())
            .collect();
        view.column_view.scroll_to_leaf_attempts = 0;
        view.listing.state = ListingState::Ready;
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();
    assert!(window.debug_bounds("column-entry-selected-0-0").is_some());
    assert!(window.debug_bounds("column-entry-selected-1-0").is_some());

    let strip = view.update(window, |view, _| view.column_view.strip_scroll.clone());
    assert!(strip.max_offset().x > px(0.0));
    let start_x = px(-(f32::from(strip.max_offset().x) / 2.0));
    strip.set_offset(gpui::point(start_x, px(0.0)));
    let column_bounds = window.debug_bounds("column-marquee-surface-2").unwrap();
    window.simulate_event(gpui::ScrollWheelEvent {
        position: column_bounds.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-120.0))),
        ..Default::default()
    });

    assert_eq!(strip.offset().x, start_x);
}

#[gpui::test]
fn column_keyboard_navigation_repaints_the_selection_indicator(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.bind_keys([KeyBinding::new("down", SelectNext, Some("browser"))]);
    });
    let root = PathBuf::from("column-keyboard");
    let first = absolute_entry(root.join("a.txt"));
    let second = absolute_entry(root.join("b.txt"));
    let entries = vec![first.clone(), second.clone()];
    let (view, window) = cx.add_window_view(|window, cx| {
        let mut view = DirectoryWindow::new(root.clone(), NativeServices::default(), cx);
        view.browser.set_view_mode(ViewMode::Column);
        view.browser.replace_entries(entries.clone());
        view.browser.select(first.path.clone());
        view.column_view.columns = ColumnState::new(&root);
        assert!(view.column_view.columns.apply_listed(&root, entries));
        view.column_view.scroll_handles = vec![UniformListScrollHandle::new()];
        view.sync_column_selection_from_browser();
        view.settings.view.show_preview_panel = false;
        view.preview.state = PreviewState::Closed;
        view.listing.state = ListingState::Ready;
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();
    assert!(window.debug_bounds("column-entry-selected-0-0").is_some());

    window.simulate_keystrokes("down");
    window.run_until_parked();

    view.update(window, |view, _| {
        assert_eq!(view.browser.selected_path(), Some(second.path.as_path()));
        assert!(view.column_view.selection.contains(&second.path));
    });
    assert!(window.debug_bounds("column-entry-selected-0-0").is_none());
    assert!(window.debug_bounds("column-entry-selected-0-1").is_some());
}

#[gpui::test]
fn visible_hidden_items_keep_a_dimmed_content_state_in_every_view(cx: &mut TestAppContext) {
    let root = PathBuf::from("hidden-item-state");
    let mut hidden = absolute_entry(root.join(".secret.txt"));
    hidden.hidden = true;
    let (view, window) = cx.add_window_view(|window, cx| {
        let mut view = DirectoryWindow::new(root.clone(), NativeServices::default(), cx);
        view.browser
            .apply_common_preferences(true, false, EntryFilter::All);
        view.browser.replace_entries(vec![hidden.clone()]);
        view.listing.state = ListingState::Ready;
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(900.0), px(640.0)));
    window.run_until_parked();
    assert!(window.debug_bounds("list-hidden-entry-0").is_some());

    view.update(window, |view, cx| {
        view.browser.set_view_mode(ViewMode::Grid);
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("grid-hidden-entry-0").is_some());

    view.update(window, |view, cx| {
        view.browser.set_view_mode(ViewMode::Column);
        view.column_view.columns = ColumnState::new(&root);
        assert!(view.column_view.columns.apply_listed(&root, vec![hidden]));
        view.column_view.scroll_handles = vec![UniformListScrollHandle::new()];
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("column-hidden-entry-0-0").is_some());
}

#[gpui::test]
fn quick_look_decodes_photoshop_documents_to_images(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let source = directory.join("design.psd");
    fs::write(&source, minimal_psd(320, 180, [40, 120, 220, 255])).unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) = cx.add_window_view(|window, cx| {
        let mut view = DirectoryWindow::new(directory.clone(), services, cx);
        view.browser
            .replace_entries(vec![metadata_entry(source.clone())]);
        view.browser.select(source.clone());
        view.listing.state = ListingState::Ready;
        view.open_quick_look(source.clone(), vec![source.clone()], cx);
        window.focus(&view.focus_handle, cx);
        view
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            matches!(view.preview.state, PreviewState::Ready { .. })
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    view.update(window, |view, _| match &view.preview.state {
        PreviewState::Ready {
            path,
            content: PreviewContent::Image(preview),
        } => {
            assert_eq!(path, &source);
            assert_eq!(
                preview.extension().and_then(|value| value.to_str()),
                Some("png")
            );
            assert!(preview.is_file());
        }
        state => panic!("PSD Quick Look did not produce an image: {state:?}"),
    });
    remove_fixture(&directory);
}

#[cfg(feature = "preview-3d")]
#[gpui::test]
fn quick_look_renders_and_orbits_models_with_a_cached_grid_thumbnail(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let source = directory.join("triangle.obj");
    fs::write(&source, "v -1 -1 0\nv 1 -1 0\nv 0 1 0\nf 1 2 3\n").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let thumbnail = services
        .previews
        .thumbnail(source.clone(), 256)
        .wait()
        .unwrap()
        .expect("OBJ should produce a cached thumbnail");
    assert!(thumbnail.is_file());

    let (view, window) = cx.add_window_view(|window, cx| {
        let mut view = DirectoryWindow::new(directory.clone(), services, cx);
        view.browser
            .replace_entries(vec![metadata_entry(source.clone())]);
        view.browser.select(source.clone());
        view.listing.state = ListingState::Ready;
        view.open_quick_look(source.clone(), vec![source.clone()], cx);
        window.focus(&view.focus_handle, cx);
        view
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            matches!(
                view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Model,
                    ..
                }
            )
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let first_frame = view.update(window, |view, cx| {
        assert!(
            matches!(
                view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Model,
                    ..
                }
            ),
            "OBJ Quick Look did not produce a model preview"
        );
        let model = view.media.read(cx).model.clone().expect("model preview");
        assert_eq!(model.triangle_count, 1);
        let frame = Arc::clone(&model.frame.rgba);
        view.media.update(cx, |media, cx| {
            media.adjust_model_camera(0.4, 0.0, 1.0, 0.0, 0.0, cx)
        });
        frame
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, cx| {
            view.media.read(cx).model_task.is_none()
                && view.media.read(cx).model_camera == view.media.read(cx).model_rendered_camera
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(window, |view, cx| {
        assert!(matches!(
            view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::Model,
                ..
            }
        ));
        let model = view
            .media
            .read(cx)
            .model
            .clone()
            .expect("rotated model preview was not retained");
        assert_ne!(first_frame, model.frame.rgba);
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn shortcut_rebinding_rejects_conflicts_dispatches_live_and_survives_restart(
    cx: &mut TestAppContext,
) {
    let directory = fixture_dir();
    let resources = explorie_native_services::ResourcePaths::test(&directory);
    fs::create_dir_all(&resources.config_dir).unwrap();
    let mut settings = AppSettings::default();
    settings
        .shortcut_bindings
        .insert("settings-open".to_string(), "secondary-alt-k".to_string());
    fs::write(
        resources.config_dir.join("settings-v1.json"),
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .unwrap();

    let services = NativeServices::new(resources.clone());
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            let view = cx.new(|cx| {
                let view = DirectoryWindow::restore(directory.clone(), false, services, cx);
                view.install_shortcut_bindings(cx);
                view
            });
            window.focus(&view.focus_handle(cx), cx);
            view
        })
        .unwrap()
    });

    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-,").unwrap());
    window
        .update(cx, |view, _, _| assert!(!view.settings_ui.panel_open))
        .unwrap();
    cx.dispatch_keystroke(*window, secondary_keystroke("alt-k"));
    window
        .update(cx, |view, _, _| assert!(view.settings_ui.panel_open))
        .unwrap();

    window
        .update(cx, |view, _, cx| {
            view.open_shortcut_editor("settings-open", cx);
            view.handle_shortcut_editor_key(
                &KeyDownEvent {
                    keystroke: secondary_keystroke("c"),
                    is_held: false,
                    prefer_character_input: false,
                },
                cx,
            );
            assert!(
                view.settings_ui
                    .shortcut_editor
                    .as_ref()
                    .and_then(|editor| editor.error.as_deref())
                    .is_some_and(|error| error.contains("conflicts"))
            );
            view.handle_shortcut_editor_key(
                &KeyDownEvent {
                    keystroke: secondary_keystroke("alt-j"),
                    is_held: false,
                    prefer_character_input: false,
                },
                cx,
            );
            assert!(
                view.settings_ui
                    .shortcut_editor
                    .as_ref()
                    .unwrap()
                    .error
                    .is_none()
            );
            view.submit_shortcut_editor(cx);
            assert_eq!(
                view.commands()
                    .into_iter()
                    .find(|command| command.id == CommandId::OpenSettings)
                    .and_then(|command| command.shortcut),
                Some(display_binding("secondary-alt-j"))
            );
            view.close_settings_panel(cx);
            view.settings_store.as_ref().unwrap().flush();
        })
        .unwrap();

    cx.dispatch_keystroke(*window, secondary_keystroke("alt-k"));
    window
        .update(cx, |view, _, _| assert!(!view.settings_ui.panel_open))
        .unwrap();
    cx.dispatch_keystroke(*window, secondary_keystroke("alt-j"));
    window
        .update(cx, |view, _, _| assert!(view.settings_ui.panel_open))
        .unwrap();

    let persisted: AppSettings =
        serde_json::from_slice(&fs::read(resources.config_dir.join("settings-v1.json")).unwrap())
            .unwrap();
    assert_eq!(
        persisted
            .shortcut_bindings
            .get("settings-open")
            .map(String::as_str),
        Some("secondary-alt-j")
    );

    let restart_services = NativeServices::new(resources);
    let restarted_window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            let view = cx.new(|cx| {
                let view = DirectoryWindow::restore(directory.clone(), false, restart_services, cx);
                view.install_shortcut_bindings(cx);
                view
            });
            window.focus(&view.focus_handle(cx), cx);
            view
        })
        .unwrap()
    });
    cx.dispatch_keystroke(*restarted_window, secondary_keystroke("alt-j"));
    restarted_window
        .update(cx, |view, _, _| {
            assert!(view.settings_ui.panel_open);
            assert_eq!(
                binding_for(&view.settings.shortcut_bindings, "settings-open")
                    .as_deref()
                    .map(display_binding),
                Some(display_binding("secondary-alt-j"))
            );
        })
        .unwrap();
    remove_fixture(&directory);
}

#[gpui::test]
fn status_bar_restores_listing_context_operations_and_responsive_detail(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let folder = directory.join("Folder");
    let report = directory.join("report.txt");
    let photo = directory.join("photo.png");
    let hidden = directory.join("secret.txt");
    fs::create_dir(&folder).unwrap();
    fs::write(&report, vec![b'r'; 2_048]).unwrap();
    fs::write(&photo, vec![b'p'; 1_024]).unwrap();
    fs::write(&hidden, vec![b's'; 512]).unwrap();

    let mut folder_entry = metadata_entry(folder);
    folder_entry.is_dir = true;
    let report_entry = metadata_entry(report.clone());
    let photo_entry = metadata_entry(photo);
    let mut hidden_entry = metadata_entry(hidden);
    hidden_entry.hidden = true;

    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    let request = FileOperationRequest {
        kind: FileOperationKind::Copy,
        sources: vec![report.clone()],
        destination: Some(directory.join("destination")),
        conflict_policy: ConflictPolicy::Rename,
    };
    view.update(window, |view, cx| {
        view.browser
            .replace_entries(vec![folder_entry, report_entry, photo_entry, hidden_entry]);
        view.browser.set_filter(EntryFilter::Files);
        view.browser.set_search_query("report".to_string());
        view.browser.select(report.clone());
        view.disk.info = Some(DiskInfo {
            mount_point: "C:\\".to_string(),
            total_space: 1_000_000_000,
            available_space: 625_000_000,
            name: "Fixture disk".to_string(),
        });
        view.operations.track("status-running".into(), request);
        assert!(view.operations.apply(FileOperationEvent {
            job_id: "status-running".into(),
            state: explorie_native_services::FileOperationState::Running,
            progress: Some(FileOperationProgress {
                processed_entries: 1,
                total_entries: 4,
                processed_bytes: 512,
                total_bytes: 2_048,
                current_path: Some(report),
            }),
            result: None,
            retryable_sources: Vec::new(),
            error: None,
        }));
        view.operation_ui.panel_hidden = true;
        view.operation_ui.panel_minimized = true;
        view.watcher.status = WatchStatus::Watching;
        view.listing.state = ListingState::Ready;
        cx.notify();
    });

    window.simulate_resize(gpui::size(px(1_200.0), px(720.0)));
    window.run_until_parked();
    for selector in [
        "status-item-summary",
        "status-filter-summary",
        "status-operation-summary",
        "status-selection-summary",
    ] {
        let bounds = window
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing wide status detail: {selector}"));
        assert!(f32::from(bounds.size.width) > 0.0, "empty {selector}");
    }
    let left = window.debug_bounds("status-left-cluster").unwrap();
    let right = window.debug_bounds("status-right-cluster").unwrap();
    assert!(left.right() <= right.right());
    assert!(
        window
            .debug_bounds("status-operation-summary")
            .unwrap()
            .right()
            <= window
                .debug_bounds("status-selection-summary")
                .unwrap()
                .left(),
        "wide status clusters must not overlap"
    );
    assert_eq!(
        f32::from(window.debug_bounds("watcher-status").unwrap().size.height),
        28.0
    );
    assert!(window.debug_bounds("status-disk-summary").is_none());
    assert!(window.debug_bounds("status-watcher-summary").is_none());
    assert!(window.debug_bounds("status-view-summary").is_none());
    assert_eq!(
        f32::from(window.debug_bounds("operation-panel").unwrap().size.height),
        0.0
    );

    let operation = window.debug_bounds("status-operation-summary").unwrap();
    window.simulate_click(operation.center(), gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert!(!view.operation_ui.panel_hidden);
        assert!(!view.operation_ui.panel_minimized);
    });
    assert!(
        f32::from(window.debug_bounds("operation-panel").unwrap().size.height) > 40.0,
        "status operation summary should reveal the full operation panel"
    );

    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();
    assert!(window.debug_bounds("status-item-summary").is_some());
    assert!(window.debug_bounds("status-filter-summary").is_some());
    assert!(window.debug_bounds("status-operation-summary").is_some());
    assert!(window.debug_bounds("status-disk-summary").is_none());
    assert!(window.debug_bounds("status-watcher-summary").is_none());
    assert!(window.debug_bounds("status-view-summary").is_none());
    let compact_status = window.debug_bounds("watcher-status").unwrap();
    assert_eq!(f32::from(compact_status.size.height), 28.0);
    for selector in [
        "status-item-summary",
        "status-operation-summary",
        "status-selection-summary",
    ] {
        let bounds = window.debug_bounds(selector).unwrap();
        assert!(
            bounds.left() >= compact_status.left(),
            "{selector} escaped left"
        );
        assert!(
            bounds.right() <= compact_status.right(),
            "{selector} escaped right"
        );
    }
    let compact_operation = window.debug_bounds("status-operation-summary").unwrap();
    let compact_selection = window.debug_bounds("status-selection-summary").unwrap();
    assert!(
        compact_operation.right() <= compact_selection.left(),
        "compact status clusters must not overlap: operation={compact_operation:?}, selection={compact_selection:?}, status={compact_status:?}"
    );
    view.update(window, |view, cx| {
        view.watcher.status = WatchStatus::Unavailable("watch failed".to_string());
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("status-watcher-summary").is_some());
    remove_fixture(&directory);
}

#[gpui::test]
fn operation_history_is_a_bounded_floating_panel_with_minimize_and_close(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();
    let file_surface_before = window.debug_bounds("file-surface").unwrap();

    let request = FileOperationRequest {
        kind: FileOperationKind::Copy,
        sources: vec![directory.join("alpha.txt")],
        destination: Some(directory.join("destination")),
        conflict_policy: ConflictPolicy::Rename,
    };
    view.update(window, |view, cx| {
        view.operations.track("running".into(), request.clone());
        assert!(view.operations.apply(FileOperationEvent {
            job_id: "running".into(),
            state: explorie_native_services::FileOperationState::Running,
            progress: Some(FileOperationProgress {
                processed_entries: 1,
                total_entries: 2,
                processed_bytes: 5,
                total_bytes: 10,
                current_path: Some(directory.join("alpha.txt")),
            }),
            result: None,
            retryable_sources: Vec::new(),
            error: None,
        }));
        view.operations.track("finished".into(), request);
        assert!(view.operations.apply(FileOperationEvent {
            job_id: "finished".into(),
            state: explorie_native_services::FileOperationState::Completed,
            progress: Some(FileOperationProgress {
                processed_entries: 1,
                total_entries: 1,
                processed_bytes: 10,
                total_bytes: 10,
                current_path: None,
            }),
            result: Some(FileOperationResult {
                processed_entries: 1,
                processed_bytes: 10,
                targets: vec![directory.join("destination/alpha.txt")],
                target_snapshots: Vec::new(),
            }),
            retryable_sources: Vec::new(),
            error: None,
        }));
        cx.notify();
    });
    window.run_until_parked();

    let panel = window.debug_bounds("operation-panel").unwrap();
    let header = window.debug_bounds("operation-panel-header").unwrap();
    assert_eq!(f32::from(panel.size.width), 380.0);
    assert!(f32::from(panel.size.height) <= 440.0);
    assert_eq!(panel.right(), px(788.0));
    assert_eq!(panel.bottom(), px(560.0));
    assert_eq!(f32::from(header.size.height), 44.0);
    assert!(window.debug_bounds("operation-panel-body").is_some());
    assert_eq!(
        window.debug_bounds("file-surface").unwrap(),
        file_surface_before,
        "floating operation history must not consume listing height"
    );

    let minimize = window.debug_bounds("minimize-operations").unwrap().center();
    window.simulate_click(minimize, gpui::Modifiers::default());
    window.run_until_parked();
    let minimized = window.debug_bounds("operation-panel").unwrap();
    assert_eq!(f32::from(minimized.size.width), 280.0);
    assert_eq!(f32::from(minimized.size.height), 40.0);
    assert_eq!(minimized.right(), px(784.0));
    assert_eq!(minimized.bottom(), px(560.0));
    assert!(window.debug_bounds("operation-panel-body").is_none());

    window.simulate_click(minimized.center(), gpui::Modifiers::default());
    window.run_until_parked();
    assert!(window.debug_bounds("operation-panel-body").is_some());
    let clear = window.debug_bounds("clear-operations").unwrap().center();
    window.simulate_click(clear, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(view.operations.operations().len(), 1);
    });

    view.update(window, |view, cx| {
        assert!(view.operations.apply(FileOperationEvent {
            job_id: "running".into(),
            state: explorie_native_services::FileOperationState::Cancelled,
            progress: None,
            result: None,
            retryable_sources: Vec::new(),
            error: None,
        }));
        view.settings.view.show_status_bar = false;
        cx.notify();
    });
    window.run_until_parked();
    let moved_panel = window.debug_bounds("operation-panel").unwrap();
    assert_eq!(moved_panel.bottom(), px(588.0));
    let close = window.debug_bounds("close-operations").unwrap().center();
    window.simulate_click(close, gpui::Modifiers::default());
    window.run_until_parked();
    assert_eq!(
        f32::from(window.debug_bounds("operation-panel").unwrap().size.height),
        0.0
    );
    remove_fixture(&directory);
}

#[gpui::test]
fn go_to_folder_restores_shortcut_autocomplete_validation_recent_and_modal_geometry(
    cx: &mut TestAppContext,
) {
    let directory = fixture_dir();
    let documents = directory.join("Documents");
    let downloads = directory.join("Downloads");
    fs::create_dir(&documents).unwrap();
    fs::create_dir(&downloads).unwrap();
    fs::write(directory.join("doc.txt"), "not a directory").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(directory.clone(), services, cx);
        view.install_shortcut_bindings(cx);
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();

    window.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-shift-g"
    } else {
        "ctrl-g"
    });
    window.run_until_parked();
    let backdrop = window.debug_bounds("go-to-folder-backdrop").unwrap();
    let dialog = window.debug_bounds("go-to-folder-dialog").unwrap();
    assert_eq!(f32::from(backdrop.size.width), 800.0);
    assert_eq!(f32::from(backdrop.size.height), 600.0);
    assert_eq!(f32::from(dialog.size.width), 560.0);
    assert_eq!(dialog.top(), px(90.0));
    assert_eq!(dialog.center().x, backdrop.center().x);

    for key in ["d", "o", "c"] {
        window.simulate_keystrokes(key);
    }
    window.executor().advance_clock(Duration::from_millis(150));
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        let ready = view.update(window, |view, _| {
            view.navigation_ui
                .go_to_folder
                .as_ref()
                .is_some_and(|state| !state.suggestions.is_empty())
        });
        if ready {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(window, |view, _| {
        let state = view.navigation_ui.go_to_folder.as_ref().unwrap();
        assert_eq!(state.input, "doc");
        assert_eq!(state.suggestions.len(), 1);
        assert_eq!(state.suggestions[0].path, documents);
    });
    assert!(window.debug_bounds("go-to-folder-suggestion-0").is_some());

    window.simulate_keystrokes("down enter");
    view.update(window, |view, _| {
        let state = view.navigation_ui.go_to_folder.as_ref().unwrap();
        assert_eq!(state.input, documents.display().to_string());
        assert!(state.suggestions.is_empty());
    });
    window.simulate_keystrokes("enter");
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| view.navigation_ui.go_to_folder.is_none()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    window.run_until_parked();
    view.update(window, |view, _| {
        assert!(view.navigation_ui.go_to_folder.is_none());
        assert_eq!(view.browser.path(), documents);
        assert_eq!(
            view.browser.go_to_folder_recents(),
            std::slice::from_ref(&documents)
        );
    });

    view.update(window, |view, cx| view.open_go_to_folder(cx));
    view.update(window, |view, cx| {
        let state = view.navigation_ui.go_to_folder.as_mut().unwrap();
        state.input = directory.join("missing").display().to_string();
        state.replace_on_type = false;
        view.submit_go_to_folder(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        let ready = view.update(window, |view, _| {
            view.navigation_ui
                .go_to_folder
                .as_ref()
                .and_then(|state| state.error.as_ref())
                .is_some()
        });
        if ready {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(window, |view, _| {
        assert_eq!(
            view.navigation_ui
                .go_to_folder
                .as_ref()
                .unwrap()
                .error
                .as_deref(),
            Some("Path does not exist")
        );
        assert_eq!(view.browser.path(), documents);
    });
    assert!(window.debug_bounds("go-to-folder-error").is_some());

    view.update(window, |view, cx| {
        let state = view.navigation_ui.go_to_folder.as_mut().unwrap();
        state.input.clear();
        state.error = None;
        view.schedule_go_to_folder_suggestions(cx);
    });
    window.run_until_parked();
    let recent = window
        .debug_bounds("go-to-folder-suggestion-0")
        .unwrap()
        .center();
    window.simulate_click(recent, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(
            view.navigation_ui.go_to_folder.as_ref().unwrap().input,
            documents.display().to_string()
        );
    });

    window.simulate_click(gpui::point(px(2.0), px(2.0)), gpui::Modifiers::default());
    window.run_until_parked();
    assert!(window.debug_bounds("go-to-folder-backdrop").is_none());
    remove_fixture(&directory);
}

#[gpui::test]
fn folder_load_failure_restores_retry_picker_and_recovery_geometry(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let alternate = directory.join("alternate");
    fs::create_dir(&alternate).unwrap();
    fs::write(alternate.join("recovered.txt"), "recovered").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    view.update(window, |view, cx| {
        view.listing.state = ListingState::Failed("Access denied".to_string());
        cx.notify();
    });
    window.run_until_parked();

    let surface = window.debug_bounds("listing-drop-surface").unwrap();
    let recovery = window.debug_bounds("listing-error").unwrap();
    assert_eq!(recovery, surface);
    assert!(f32::from(recovery.size.height) >= 240.0);
    assert!(window.debug_bounds("retry-listing").is_some());
    assert!(window.debug_bounds("choose-another-folder").is_some());

    view.update(window, |view, cx| {
        view.complete_folder_picker(Err("Dialog unavailable".to_string()), cx);
    });
    window.run_until_parked();
    assert!(window.debug_bounds("folder-picker-error").is_some());
    view.update(window, |view, _| {
        assert_eq!(
            view.navigation_ui.folder_picker_error.as_deref(),
            Some("Dialog unavailable")
        );
        assert!(!view.navigation_ui.folder_picker_active);
    });

    let retry = window.debug_bounds("retry-listing").unwrap().center();
    window.simulate_click(retry, gpui::Modifiers::default());
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            matches!(view.listing.state, ListingState::Ready)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(window, |view, cx| {
        assert!(matches!(view.listing.state, ListingState::Ready));
        view.listing.state = ListingState::Failed("Folder disappeared".to_string());
        view.navigation_ui.folder_picker_active = true;
        view.navigation_ui.folder_picker_error = None;
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("choose-another-folder").is_some());

    view.update(window, |view, cx| {
        view.complete_folder_picker(Ok(Some(alternate.clone())), cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            matches!(view.listing.state, ListingState::Ready)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), alternate);
        assert!(matches!(view.listing.state, ListingState::Ready));
        assert_eq!(view.browser.visible_entries().len(), 1);
        assert_eq!(
            view.browser.visible_entries()[0]
                .path
                .file_name()
                .and_then(|name| name.to_str()),
            Some("recovered.txt")
        );
        assert!(!view.navigation_ui.folder_picker_active);
        assert!(view.navigation_ui.folder_picker_error.is_none());
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn mutation_prompts_match_legacy_modal_geometry_and_keep_validation_local(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let source = directory.join("alpha.txt");
    fs::write(&source, "alpha").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(directory.clone(), services, cx);
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();

    view.update(window, |view, cx| view.prompt_new_folder(cx));
    window.run_until_parked();
    let backdrop = window.debug_bounds("mutation-prompt-backdrop").unwrap();
    let text_dialog = window.debug_bounds("mutation-prompt-dialog").unwrap();
    assert_eq!(f32::from(backdrop.size.width), 800.0);
    assert_eq!(f32::from(backdrop.size.height), 600.0);
    assert_eq!(f32::from(text_dialog.size.width), 360.0);
    assert_eq!(text_dialog.center().x, backdrop.center().x);
    assert!((f32::from(text_dialog.center().y) - f32::from(backdrop.center().y)).abs() <= 0.5);
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert!(window.debug_bounds("mutation-prompt-error").is_some());
    let cancel = window
        .debug_bounds("mutation-prompt-cancel")
        .unwrap()
        .center();
    window.simulate_click(cancel, gpui::Modifiers::default());
    window.run_until_parked();
    assert!(window.debug_bounds("mutation-prompt-backdrop").is_none());

    view.update(window, |view, cx| {
        view.browser
            .replace_entries(vec![absolute_entry(source.clone())]);
        view.browser.select(source.clone());
        view.prompt_create_archive(cx);
    });
    window.run_until_parked();
    let archive_dialog = window.debug_bounds("mutation-prompt-dialog").unwrap();
    let archive_header = window.debug_bounds("mutation-prompt-header").unwrap();
    let archive_footer = window.debug_bounds("mutation-prompt-footer").unwrap();
    assert_eq!(f32::from(archive_dialog.size.width), 500.0);
    assert_eq!(archive_dialog.center().x, backdrop.center().x);
    assert_eq!(f32::from(archive_header.size.height), 54.0);
    assert_eq!(f32::from(archive_footer.size.height), 50.0);
    assert_eq!(
        f32::from(window.debug_bounds("operation-panel").unwrap().size.height),
        0.0,
        "archive options belong inside the modal rather than the operation footer"
    );
    let format = window.debug_bounds("archive-format").unwrap().center();
    window.simulate_click(format, gpui::Modifiers::default());
    view.update(window, |view, cx| {
        assert!(matches!(
            view.mutation.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(MutationPromptKind::ArchiveName {
                format: ArchiveFormat::SevenZ,
                compression_level: CompressionLevel::None,
                ..
            })
        ));
        view.cancel_mutation_prompt(cx);
    });

    view.update(window, |view, cx| view.prompt_permanent_delete_selected(cx));
    window.run_until_parked();
    let destructive_dialog = window.debug_bounds("mutation-prompt-dialog").unwrap();
    assert_eq!(f32::from(destructive_dialog.size.width), 540.0);
    assert_eq!(destructive_dialog.center().x, backdrop.center().x);
    assert!(window.debug_bounds("mutation-prompt-items").is_some());
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert!(window.debug_bounds("mutation-prompt-error").is_some());
    view.update(window, |view, cx| {
        view.cancel_mutation_prompt(cx);
        view.dismiss_toast(cx);
        view.show_toast("Native operation completed", ToastKind::Success, cx);
    });
    window.run_until_parked();
    let toast = window.debug_bounds("native-toast").unwrap();
    assert!(f32::from(toast.size.width) >= 280.0);
    assert!(f32::from(toast.size.width) <= 400.0);
    assert_eq!(toast.right(), px(784.0));
    assert_eq!(toast.top(), px(48.0));
    remove_fixture(&directory);
}

#[gpui::test]
fn control_surfaces_match_legacy_modal_geometry(cx: &mut TestAppContext) {
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(PathBuf::from("sample"), NativeServices::default(), cx);
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();

    view.update(window, |view, cx| {
        view.open_control_surface(ControlSurface::CommandPalette, cx)
    });
    window.run_until_parked();
    let backdrop = window.debug_bounds("control-surface-backdrop").unwrap();
    let command_palette = window.debug_bounds("command-palette").unwrap();
    assert_eq!(f32::from(backdrop.size.width), 800.0);
    assert_eq!(f32::from(backdrop.size.height), 600.0);
    assert_eq!(f32::from(command_palette.size.width), 560.0);
    assert_eq!(command_palette.center().x, backdrop.center().x);
    assert_eq!(command_palette.top(), px(90.0));

    view.update(window, |view, cx| {
        view.open_control_surface(ControlSurface::Workspaces, cx)
    });
    window.run_until_parked();
    let workspace_manager = window.debug_bounds("workspace-manager").unwrap();
    assert_eq!(f32::from(workspace_manager.size.width), 500.0);
    assert_eq!(workspace_manager.center().x, backdrop.center().x);
    assert!(
        (f32::from(workspace_manager.center().y) - f32::from(backdrop.center().y)).abs() <= 0.5
    );

    view.update(window, |view, cx| {
        let first = PathBuf::from("sample/one.txt");
        let second = PathBuf::from("sample/two.txt");
        view.browser.replace_entries(vec![
            absolute_entry(first.clone()),
            absolute_entry(second.clone()),
        ]);
        view.browser.select(first);
        view.browser.toggle_selection(second);
        view.prompt_rename_selected(cx);
    });
    window.run_until_parked();
    let batch_rename = window.debug_bounds("batch-rename-manager").unwrap();
    assert_eq!(f32::from(batch_rename.size.width), 600.0);
    assert_eq!(batch_rename.center().x, backdrop.center().x);
    assert!((f32::from(batch_rename.center().y) - f32::from(backdrop.center().y)).abs() <= 0.5);
}

#[gpui::test]
fn command_palette_filters_executes_and_persists_recent_commands(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let resources = explorie_native_services::ResourcePaths::test(&directory);
    let services = NativeServices::new(resources.clone());
    let window = cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("ctrl-shift-p", OpenCommandPalette, Some("browser")),
            KeyBinding::new("ctrl-shift-w", ToggleWorkspaceManager, Some("browser")),
            KeyBinding::new("shift-/", ToggleShortcutsOverlay, Some("browser")),
            KeyBinding::new("escape", ClearSelection, Some("browser")),
        ]);
        cx.open_window(Default::default(), |window, cx| {
            let view =
                cx.new(|cx| DirectoryWindow::restore(directory.clone(), false, services, cx));
            window.focus(&view.focus_handle(cx), cx);
            view
        })
        .unwrap()
    });

    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-shift-p").unwrap());
    for character in ["l", "i", "g", "h", "t"] {
        cx.dispatch_keystroke(
            *window,
            Keystroke::parse(character).unwrap().with_simulated_ime(),
        );
    }
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.overlay.surface, ControlSurface::CommandPalette);
            assert_eq!(view.overlay.query, "light");
            assert_eq!(view.visible_commands()[0].id, CommandId::ThemeLight);
            assert!(
                view.visible_commands()
                    .iter()
                    .any(|command| command.id == CommandId::ClearHistory)
            );
        })
        .unwrap();

    cx.dispatch_keystroke(*window, Keystroke::parse("enter").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.overlay.surface, ControlSurface::Closed);
            assert_eq!(view.settings.appearance.theme, ThemeMode::Light);
            assert_eq!(
                view.settings.recent_commands.first().map(String::as_str),
                Some("settings-theme-light")
            );
            view.settings_store.as_ref().unwrap().flush();
        })
        .unwrap();

    let persisted: AppSettings =
        serde_json::from_slice(&fs::read(resources.config_dir.join("settings-v1.json")).unwrap())
            .unwrap();
    assert_eq!(
        persisted.recent_commands.first().map(String::as_str),
        Some("settings-theme-light")
    );
    cx.dispatch_keystroke(*window, Keystroke::parse("shift-/").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.overlay.surface, ControlSurface::Shortcuts)
        })
        .unwrap();
    cx.dispatch_keystroke(*window, Keystroke::parse("escape").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.overlay.surface, ControlSurface::Closed)
        })
        .unwrap();
    remove_fixture(&directory);
}

#[gpui::test]
fn shortcuts_diagnostics_recovery_and_toast_render_as_native_surfaces(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.record_error("Disabled collection", "must not be collected");
        assert_eq!(view.error_reports.len(), 0);
        view.open_control_surface(ControlSurface::Shortcuts, cx);
        view.overlay.query = "copy".to_string();
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );

    view.update(cx, |view, cx| {
        view.open_control_surface(ControlSurface::Diagnostics, cx);
        view.copy_diagnostics(cx);
        assert!(view.toasts.current.is_some());
        view.announce_unclean_recovery(cx);
        assert!(view.recovery.notice);
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    assert_eq!(
        recovery_session_context(1, &directory),
        format!(
            "1 tab • Last: {} • Restored from the last atomic snapshot",
            path_label(&directory)
        )
    );
    assert_eq!(
        recovery_session_context(2, &directory),
        format!(
            "2 tabs • Last: {} • Restored from the last atomic snapshot",
            path_label(&directory)
        )
    );
    let notice = cx.debug_bounds("recovery-notice").unwrap();
    let context = cx.debug_bounds("recovery-session-context").unwrap();
    assert!(context.left() >= notice.left());
    assert!(context.right() <= notice.right());
    assert!(context.top() >= notice.top());
    assert!(context.bottom() <= notice.bottom());
    let diagnostics = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("diagnostics clipboard text");
    assert!(diagnostics.contains("\"runtime\": \"gpui\""));
    assert!(diagnostics.contains("\"pathValues\": \"omitted\""));
    assert!(!diagnostics.contains(&directory.display().to_string()));

    view.update(cx, |view, cx| {
        view.dismiss_recovery(cx);
        view.dismiss_toast(cx);
        view.close_control_surface(cx);
        assert!(!view.recovery.notice);
        assert!(view.toasts.current.is_none());
        assert_eq!(view.overlay.surface, ControlSurface::Closed);
    });
    remove_fixture(&directory);
}

fn open_runtime_window(
    cx: &mut TestAppContext,
    root: &Path,
    services: &NativeServices,
    runtime: &WindowRuntime,
    session_id: &str,
) -> gpui::WindowHandle<DirectoryWindow> {
    let root = root.to_path_buf();
    let services = services.clone();
    let runtime = runtime.clone();
    let session_id = session_id.to_string();
    cx.add_window(move |_, cx| {
        let mut view = DirectoryWindow::restore_window_session(
            root,
            true,
            services,
            Some(runtime),
            session_id,
            cx,
        );
        view.start_service_events(cx);
        view
    })
}

#[gpui::test]
fn windows_share_one_recovery_journal_and_never_recover_live_operations(cx: &mut TestAppContext) {
    let root = fixture_dir();
    let source_dir = root.join("source");
    let destination = root.join("destination");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&destination).unwrap();
    let interrupted_source = source_dir.join("interrupted.txt");
    let live_source = source_dir.join("live.txt");
    fs::write(&interrupted_source, b"interrupted").unwrap();
    fs::write(&live_source, b"live").unwrap();
    let resources = ResourcePaths::test(&root);
    fs::create_dir_all(&resources.config_dir).unwrap();
    let journal = resources.config_dir.join("operation-recovery-v1.json");
    let copy = |source: &Path| FileOperationRequest {
        kind: FileOperationKind::Copy,
        sources: vec![source.to_path_buf()],
        destination: Some(destination.clone()),
        conflict_policy: ConflictPolicy::Error,
    };
    // A crashed run left one copy in the journal.
    let (previous_run, _, _) = OperationRecoveryStore::open_as_other_run(&resources.config_dir);
    previous_run
        .unwrap()
        .record(&copy(&interrupted_source))
        .unwrap();

    let (runtime, _) = WindowRuntime::open(&resources.config_dir);
    let services = NativeServices::new(resources.clone());
    let first = open_runtime_window(cx, &root, &services, &runtime, "primary");
    let second = open_runtime_window(cx, &root, &services, &runtime, "second-window");
    for window in [first, second] {
        window
            .update(cx, |view, _, _| {
                assert!(view.recovery.notice);
                assert_eq!(view.recovery.interrupted.len(), 1);
            })
            .unwrap();
    }

    // An operation this run starts is journaled in the same shared store, and a
    // window opened while it runs does not mistake it for an interrupted one.
    let live_ids = first
        .update(cx, |view, _, _| {
            view.recovery
                .store
                .as_ref()
                .unwrap()
                .record(&copy(&live_source))
                .unwrap()
        })
        .unwrap();
    let third = open_runtime_window(cx, &root, &services, &runtime, "third-window");
    third
        .update(cx, |view, _, _| {
            assert_eq!(view.recovery.interrupted.len(), 1);
            assert_eq!(
                view.recovery.interrupted[0].request().sources,
                vec![interrupted_source.clone()]
            );
        })
        .unwrap();

    // Retrying in one window claims the item for every window, so a stale view
    // elsewhere cannot start a duplicate.
    first
        .update(cx, |view, _, cx| {
            view.retry_safe_interrupted_operations(cx);
            assert!(view.recovery.interrupted.is_empty());
            assert_eq!(view.recovery.jobs.len(), 1);
        })
        .unwrap();
    second
        .update(cx, |view, _, cx| {
            view.retry_safe_interrupted_operations(cx);
            assert!(view.recovery.jobs.is_empty());
            assert!(view.recovery.interrupted.is_empty());
            assert!(!view.recovery.notice);
        })
        .unwrap();
    third
        .update(cx, |view, _, _| {
            assert!(view.sync_interrupted_operations());
            assert!(view.recovery.interrupted.is_empty());
            assert!(!view.recovery.notice);
        })
        .unwrap();

    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if first
            .update(cx, |view, _, _| view.recovery.jobs.is_empty())
            .unwrap()
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        fs::read(destination.join("interrupted.txt")).unwrap(),
        b"interrupted"
    );
    // Only the live entry remains; no window overwrote the shared journal.
    let contents = fs::read_to_string(&journal).unwrap();
    assert!(contents.contains("live.txt"));
    assert!(!contents.contains("interrupted.txt"));
    second
        .update(cx, |view, _, _| {
            view.recovery
                .store
                .as_ref()
                .unwrap()
                .remove(&live_ids)
                .unwrap();
        })
        .unwrap();
    assert!(!journal.exists());

    for window in [first, second, third] {
        window
            .update(cx, |view, _, _| {
                view.session_store.as_ref().unwrap().flush()
            })
            .unwrap();
    }
    remove_fixture(&root);
}

#[gpui::test]
fn interrupted_move_offers_to_restore_its_hidden_source(cx: &mut TestAppContext) {
    let root = fixture_dir();
    let source_dir = root.join("source");
    let destination = root.join("destination");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&destination).unwrap();
    let source = source_dir.join("only-copy.txt");
    let resources = ResourcePaths::test(&root);
    fs::create_dir_all(&resources.config_dir).unwrap();
    let (previous_run, _, _) = OperationRecoveryStore::open_as_other_run(&resources.config_dir);
    previous_run
        .unwrap()
        .record(&FileOperationRequest {
            kind: FileOperationKind::Move,
            sources: vec![source.clone()],
            destination: Some(destination.clone()),
            conflict_policy: ConflictPolicy::Error,
        })
        .unwrap();
    // The run died after setting the source aside for a cross-volume move.
    let set_aside = source_dir.join(format!(".explorie-source-{}", Uuid::new_v4()));
    fs::write(&set_aside, b"precious").unwrap();

    let services = NativeServices::new(resources);
    let (first_view, cx) = cx
        .add_window_view(|_, cx| DirectoryWindow::restore(source_dir.clone(), true, services, cx));
    let view = first_view.clone();
    cx.run_until_parked();
    assert!(cx.debug_bounds("restore-recovery-items").is_some());
    assert!(cx.debug_bounds("recovery-location-0").is_none());
    view.update(cx, |view, cx| {
        assert_eq!(
            view.recovery.interrupted[0].disposition(),
            RecoveryDisposition::NeedsReview
        );
        view.restore_interrupted_items(cx);
        assert_eq!(
            view.recovery.interrupted[0].disposition(),
            RecoveryDisposition::SafeToRetry
        );
        assert!(view.recovery.interrupted[0].artifacts().is_empty());
    });
    assert_eq!(fs::read(&source).unwrap(), b"precious");
    assert!(!set_aside.exists());

    // When the original name is taken the item is only located, with Reveal.
    let replaced = source_dir.join(format!(".explorie-source-{}", Uuid::new_v4()));
    fs::write(&replaced, b"second").unwrap();
    fs::remove_file(&source).unwrap();
    let services = NativeServices::new(ResourcePaths::test(&root));
    let (view, cx) = cx
        .add_window_view(|_, cx| DirectoryWindow::restore(source_dir.clone(), true, services, cx));
    fs::write(&source, b"new file with the same name").unwrap();
    view.update(cx, |view, cx| {
        view.restore_interrupted_items(cx);
        let artifact = &view.recovery.interrupted[0].artifacts()[0];
        assert_eq!(artifact.path(), replaced);
        assert!(!artifact.restorable());
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("restore-recovery-items").is_none());
    assert!(cx.debug_bounds("reveal-recovery-location-0").is_some());
    assert_eq!(fs::read(&replaced).unwrap(), b"second");
    for view in [first_view, view] {
        view.update(cx, |view, _| view.session_store.as_ref().unwrap().flush());
    }
    remove_fixture(&root);
}

#[gpui::test]
fn interrupted_copy_is_restored_retried_and_cleared_from_the_journal(cx: &mut TestAppContext) {
    let root = fixture_dir();
    let source_dir = root.join("source");
    let destination = root.join("destination");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&destination).unwrap();
    let source = source_dir.join("recover-me.txt");
    fs::write(&source, b"recoverable").unwrap();
    let resources = ResourcePaths::test(&root);
    fs::create_dir_all(&resources.config_dir).unwrap();
    let (store, interrupted, warning) =
        OperationRecoveryStore::open_as_other_run(&resources.config_dir);
    assert!(interrupted.is_empty());
    assert!(warning.is_none());
    let store = store.unwrap();
    let request = FileOperationRequest {
        kind: FileOperationKind::Copy,
        sources: vec![source],
        destination: Some(destination.clone()),
        conflict_policy: ConflictPolicy::Error,
    };
    store.record(&request).unwrap();
    drop(store);

    let services = NativeServices::new(resources.clone());
    let (view, cx) = cx.add_window_view(|_, cx| {
        let mut view = DirectoryWindow::restore(root.clone(), false, services, cx);
        view.start_service_events(cx);
        view
    });
    view.update(cx, |view, cx| {
        assert!(view.recovery.notice);
        assert_eq!(view.recovery.interrupted.len(), 1);
        assert_eq!(
            view.recovery.interrupted[0].disposition(),
            RecoveryDisposition::SafeToRetry
        );
        view.retry_safe_interrupted_operations(cx);
        assert!(view.recovery.interrupted.is_empty());
        assert_eq!(view.recovery.jobs.len(), 1);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| view.recovery.jobs.is_empty()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, _| {
        assert!(view.recovery.jobs.is_empty());
        assert!(!view.recovery.notice);
        assert_eq!(
            view.operations.latest().unwrap().status(),
            OperationStatus::Completed
        );
    });
    assert_eq!(
        fs::read(destination.join("recover-me.txt")).unwrap(),
        b"recoverable"
    );
    assert!(
        !resources
            .config_dir
            .join("operation-recovery-v1.json")
            .exists()
    );
    remove_fixture(&root);
}

#[gpui::test]
fn pinned_inspector_stays_visible_summarizes_multi_selection_and_persists_width(
    cx: &mut TestAppContext,
) {
    let directory = fixture_dir();
    let resources = ResourcePaths::test(&directory);
    let first = directory.join("first.txt");
    let second = directory.join("second.txt");
    fs::write(&first, "first").unwrap();
    fs::write(&second, "second").unwrap();
    let services = NativeServices::new(resources.clone());
    let (view, window) = cx
        .add_window_view(|_, cx| DirectoryWindow::restore(directory.clone(), false, services, cx));
    view.update(window, |view, cx| {
        view.browser.replace_entries(vec![
            absolute_entry(first.clone()),
            absolute_entry(second.clone()),
        ]);
        view.settings.appearance.ui_scale = 0.9;
        view.settings.view.show_preview_panel = true;
        view.sync_pinned_preview(cx);
    });
    window.simulate_resize(gpui::size(px(1_200.0), px(720.0)));
    window.run_until_parked();

    assert!(window.debug_bounds("preview-panel").is_some());
    assert!(window.debug_bounds("preview-summary-empty").is_some());
    let initial_width = f32::from(window.debug_bounds("preview-panel").unwrap().size.width);
    assert_eq!(initial_width, 324.0);

    view.update(window, |view, cx| {
        view.browser.select(first.clone());
        view.browser.toggle_selection(second.clone());
        view.sync_pinned_preview(cx);
    });
    window.run_until_parked();
    assert!(window.debug_bounds("preview-summary-multiple").is_some());

    let resizer = window.debug_bounds("preview-panel-resizer").unwrap();
    let start = resizer.center();
    let end = gpui::point(start.x - px(54.0), start.y);
    window.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_move(end, Some(MouseButton::Left), gpui::Modifiers::default());
    window.run_until_parked();
    assert_eq!(
        window
            .debug_bounds("preview-panel-resizer")
            .unwrap()
            .center()
            .x,
        end.x
    );
    window.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::default());
    window.run_until_parked();
    assert_eq!(
        f32::from(window.debug_bounds("preview-panel").unwrap().size.width),
        378.0
    );
    window.simulate_keystrokes("left");
    window.run_until_parked();
    assert_eq!(
        f32::from(window.debug_bounds("preview-panel").unwrap().size.width),
        387.0
    );
    view.update(window, |view, _| {
        view.settings_store.as_ref().unwrap().flush();
    });

    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(resources.config_dir.join("settings-v1.json")).unwrap())
            .unwrap();
    assert_eq!(saved["view"]["previewPanelWidth"], 430.0);

    let restart_services = NativeServices::new(resources);
    let (restarted, restarted_window) = cx.add_window_view(|_, cx| {
        DirectoryWindow::restore(directory.clone(), false, restart_services, cx)
    });
    restarted_window.simulate_resize(gpui::size(px(1_200.0), px(720.0)));
    restarted_window.run_until_parked();
    restarted.update(restarted_window, |view, _| {
        assert_eq!(view.settings.view.preview_panel_width, 430.0);
    });

    remove_fixture(&directory);
}

#[gpui::test]
fn restored_single_selection_starts_the_pinned_preview_after_listing(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let file = directory.join("restored.txt");
    fs::write(&file, "restored preview").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.browser.restore_folder_view_states(HashMap::from([(
            directory.clone(),
            crate::browser::FolderViewState {
                view_mode: ViewMode::List,
                sort_key: SortKey::Name,
                sort_direction: SortDirection::Ascending,
                selected: vec![file.clone()],
                scroll_index: 0,
                grid_min_width: 140,
                show_preview_panel: true,
                column_widths: HashMap::new(),
            },
        )]));
        view.settings.view.show_preview_panel = true;
        view.apply_event(
            DirectoryEvent::Listed {
                generation: view.listing.generation,
                request: ListRequest {
                    path: directory.clone(),
                    calc_dir_size: false,
                },
                entries: vec![absolute_entry(file.clone())],
                warning: None,
            },
            cx,
        );
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(view.preview.state, PreviewState::Ready { .. })
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, _| {
        assert!(matches!(
            &view.preview.state,
            PreviewState::Ready { path, .. } if path == &file
        ));
    });
    cx.simulate_resize(gpui::size(px(1_180.0), px(760.0)));
    cx.run_until_parked();
    assert!(
        f32::from(cx.debug_bounds("text-preview-code").unwrap().size.height) > 40.0,
        "pinned text preview content should receive visible height"
    );

    remove_fixture(&directory);
}

#[gpui::test]
fn pinned_preview_follows_single_selection_and_closes_for_multi_selection(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let first = directory.join("first.txt");
    let second = directory.join("second.txt");
    fs::write(&first, "first").unwrap();
    fs::write(&second, "second").unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.browser.replace_entries(vec![
            fixture_entry(first.to_string_lossy().as_ref()),
            fixture_entry(second.to_string_lossy().as_ref()),
        ]);
        view.browser.select(first.clone());
        view.toggle_preview_panel(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| {
            matches!(view.preview.state, PreviewState::Ready { .. })
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, cx| {
        assert!(matches!(
            &view.preview.state,
            PreviewState::Ready { path, .. } if path == &first
        ));
        view.browser.select_all();
        view.sync_pinned_preview(cx);
        assert!(matches!(view.preview.state, PreviewState::Closed));
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn hidden_preview_panel_stays_hidden_when_preview_content_is_loaded(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let file = directory.join("preview.txt");
    fs::write(&file, "preview content").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(directory.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });
    view.update(window, |view, cx| {
        view.browser
            .replace_entries(vec![absolute_entry(file.clone())]);
        view.browser.select(file.clone());
        view.settings.view.show_preview_panel = false;
        view.start_preview(file.clone(), cx);
    });
    window.simulate_resize(gpui::size(px(1_000.0), px(700.0)));
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if view.update(window, |view, _| {
            matches!(view.preview.state, PreviewState::Ready { .. })
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(window.debug_bounds("preview-panel").is_none());
    view.update(window, |view, cx| view.set_view_mode(ViewMode::Column, cx));
    window.run_until_parked();
    assert!(window.debug_bounds("preview-panel").is_none());

    remove_fixture(&directory);
}

#[gpui::test]
fn invalid_website_url_returns_to_the_native_prompt(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.prompt_new_website_link(cx);
        view.mutation.prompt.as_mut().unwrap().input = "Unsafe".to_string();
        view.submit_mutation_prompt(cx);
        view.mutation.prompt.as_mut().unwrap().input = "javascript:alert(1)".to_string();
        view.submit_mutation_prompt(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| !view.mutation.in_progress) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(!directory.join("Unsafe.url").exists());
    view.update(cx, |view, _| {
        assert!(matches!(
            view.mutation.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(MutationPromptKind::NewWebsiteLinkUrl { .. })
        ));
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|message| message.contains("http or https"))
        );
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn file_operation_progress_redraws_are_throttled_but_completion_is_immediate(
    cx: &mut TestAppContext,
) {
    let directory = fixture_dir();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    cx.run_until_parked();
    let job_id = "throttled-progress-job".to_string();
    view.update(cx, |view, _| {
        view.operations.track(
            job_id.clone(),
            FileOperationRequest {
                kind: FileOperationKind::Copy,
                sources: vec![directory.join("large.bin")],
                destination: Some(directory.join("copy")),
                conflict_policy: ConflictPolicy::Error,
            },
        );
    });
    let notifications = Rc::new(Cell::new(0_usize));
    let counter = Rc::clone(&notifications);
    let _subscription =
        cx.update(|_, cx| cx.observe(&view, move |_, _| counter.set(counter.get() + 1)));
    let event = |state, processed_bytes| FileOperationEvent {
        job_id: job_id.clone(),
        state,
        progress: Some(FileOperationProgress {
            processed_entries: 0,
            total_entries: 1,
            processed_bytes,
            total_bytes: 100,
            current_path: None,
        }),
        result: None,
        retryable_sources: Vec::new(),
        error: None,
    };

    for processed_bytes in 1..=50 {
        let progress = event(
            explorie_native_services::FileOperationState::Running,
            processed_bytes,
        );
        view.update(cx, |view, cx| view.apply_file_operation_event(progress, cx));
    }
    assert!(
        notifications.get() <= 2,
        "50 progress events caused {} redraws",
        notifications.get()
    );
    view.update(cx, |view, _| {
        let progress = view.operations.latest().unwrap().progress().unwrap();
        assert_eq!(progress.processed_bytes, 50);
    });
    // The coalesced updates are drawn by one trailing redraw.
    let before_catch_up = notifications.get();
    cx.executor().advance_clock(Duration::from_millis(150));
    cx.run_until_parked();
    assert_eq!(notifications.get(), before_catch_up + 1);

    let before_completion = notifications.get();
    let mut completed = event(explorie_native_services::FileOperationState::Completed, 100);
    completed.result = Some(FileOperationResult {
        processed_entries: 1,
        processed_bytes: 100,
        targets: Vec::new(),
        target_snapshots: Vec::new(),
    });
    view.update(cx, |view, cx| {
        view.apply_file_operation_event(completed, cx)
    });
    assert!(notifications.get() > before_completion);
    view.update(cx, |view, _| {
        assert_eq!(
            view.operations.latest().unwrap().status(),
            OperationStatus::Completed
        );
        assert_eq!(
            view.status_message.as_deref(),
            Some("File operation completed")
        );
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn shared_state_changes_are_not_reapplied_by_the_window_that_made_them(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let favorite = directory.join("favorite");
    fs::create_dir(&favorite).unwrap();
    let resources = ResourcePaths::test(&directory);
    fs::create_dir_all(&resources.config_dir).unwrap();
    let (runtime, _) = WindowRuntime::open(&resources.config_dir);
    let services = NativeServices::new(resources);
    let window_runtime = runtime.clone();
    let (view, cx) = cx.add_window_view(|_, cx| {
        DirectoryWindow::restore_window_session(
            directory.clone(),
            true,
            services,
            Some(window_runtime),
            "primary".to_string(),
            cx,
        )
    });
    cx.run_until_parked();
    let notifications = Rc::new(Cell::new(0_usize));
    let counter = Rc::clone(&notifications);
    let _subscription =
        cx.update(|_, cx| cx.observe(&view, move |_, _| counter.set(counter.get() + 1)));
    let revision = || runtime.shared_state_revision.load(Ordering::Acquire);

    // A local change is recorded as seen, so the next pull is a no-op.
    view.update(cx, |view, cx| {
        assert!(view.mutate_shared_session(|session| session.toggle_favorite(favorite.clone())));
        assert_eq!(view.shared_state_revision, revision());
        view.pull_shared_state(cx);
    });
    assert_eq!(notifications.get(), 0);

    // A revision that carries nothing new for this window is skipped too.
    runtime.shared_state_revision.fetch_add(1, Ordering::AcqRel);
    view.update(cx, |view, cx| {
        view.pull_shared_state(cx);
        assert_eq!(view.shared_state_revision, revision());
    });
    assert_eq!(notifications.get(), 0);

    // Another window's change is applied once.
    let mut other_window_seen = revision();
    runtime.mutate_session(&mut other_window_seen, |session| {
        session.record_go_to_folder(favorite.clone());
    });
    view.update(cx, |view, cx| {
        view.pull_shared_state(cx);
        assert_eq!(
            view.browser.go_to_folder_recents(),
            std::slice::from_ref(&favorite)
        );
    });
    assert_eq!(notifications.get(), 1);

    // Local settings and workspace changes are recorded as seen too.
    view.update(cx, |view, cx| {
        view.settings.view.show_status_bar = !view.settings.view.show_status_bar;
        view.persist_settings();
        assert_eq!(view.shared_state_revision, revision());
        let snapshot = view.workspace_snapshot();
        view.mutate_workspaces(|workspaces| workspaces.save_current("recorded", snapshot))
            .unwrap();
        assert_eq!(view.shared_state_revision, revision());
        view.pull_shared_state(cx);
    });
    assert_eq!(notifications.get(), 1);

    // A local change made before pulling another window's settings change must
    // not hide that change from the next pull.
    let mut settings = view.read_with(cx, |view, _| view.settings.clone());
    settings.view.show_hidden = !settings.view.show_hidden;
    let show_hidden = settings.view.show_hidden;
    // Published by another window that has not seen any shared changes.
    runtime.publish_settings(&mut 0, settings);
    view.update(cx, |view, cx| {
        view.mutate_shared_session(|session| session.toggle_favorite(favorite.clone()));
        assert_ne!(view.shared_state_revision, revision());
        view.pull_shared_state(cx);
        assert_eq!(view.settings.view.show_hidden, show_hidden);
        assert_eq!(view.browser.show_hidden(), show_hidden);
        assert_eq!(view.shared_state_revision, revision());
        view.session_store.as_ref().unwrap().flush();
        view.settings_store.as_ref().unwrap().flush();
        view.workspace_store.as_ref().unwrap().flush();
    });
    remove_fixture(&directory);
}

#[test]
fn native_move_round_trips_through_undo_and_redo() {
    let directory = fixture_dir();
    let source_dir = directory.join("source");
    let destination = directory.join("destination");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&destination).unwrap();
    let source = source_dir.join("round-trip.txt");
    fs::write(&source, "round trip").unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let request = FileOperationRequest {
        kind: FileOperationKind::Move,
        sources: vec![source.clone()],
        destination: Some(destination.clone()),
        conflict_policy: ConflictPolicy::Error,
    };

    let final_action = pollster::block_on(async {
        let result = await_file_operation(services.clone(), request.clone(), None)
            .await
            .unwrap();
        let record = UndoRecord::from_file_operation(request, result).unwrap();
        let undone = undo_action(record.action, services.clone()).await.unwrap();
        assert!(source.is_file());
        assert!(!destination.join("round-trip.txt").exists());
        redo_action(undone, services, None).await.unwrap()
    });

    assert!(!source.exists());
    assert_eq!(
        fs::read_to_string(destination.join("round-trip.txt")).unwrap(),
        "round trip"
    );
    assert!(matches!(final_action, UndoAction::Move { .. }));
    remove_fixture(&directory);
}

#[test]
fn keep_both_move_undo_restores_the_original_name_and_redo_repeats_it() {
    let directory = fixture_dir();
    let source_dir = directory.join("source");
    let destination = directory.join("destination");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&destination).unwrap();
    let source = source_dir.join("report.txt");
    let existing = destination.join("report.txt");
    let kept = destination.join(kept_name("report.txt"));
    fs::write(&source, "moved").unwrap();
    fs::write(&existing, "already there").unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let request = FileOperationRequest {
        kind: FileOperationKind::Move,
        sources: vec![source.clone()],
        destination: Some(destination.clone()),
        conflict_policy: ConflictPolicy::Rename,
    };

    let undone = pollster::block_on(async {
        let result = await_file_operation(services.clone(), request.clone(), None)
            .await
            .unwrap();
        assert_eq!(result.targets, vec![kept.clone()]);
        let record = UndoRecord::from_file_operation(request, result).unwrap();
        undo_action(record.action, services.clone()).await.unwrap()
    });
    assert_eq!(fs::read_to_string(&source).unwrap(), "moved");
    assert!(!source_dir.join(kept_name("report.txt")).exists());
    assert!(!kept.exists());
    assert_eq!(fs::read_to_string(&existing).unwrap(), "already there");
    let UndoAction::Move { request, pairs } = &undone else {
        panic!("expected a move undo action");
    };
    assert_eq!(request.sources, vec![source.clone()]);
    assert_eq!(pairs, &vec![(source.clone(), kept.clone())]);

    pollster::block_on(redo_action(undone, services, None)).unwrap();
    assert!(!source.exists());
    assert_eq!(fs::read_to_string(&kept).unwrap(), "moved");
    remove_fixture(&directory);
}

#[test]
fn keep_both_move_undo_keeps_the_suffix_when_the_original_name_was_retaken() {
    let directory = fixture_dir();
    let source_dir = directory.join("source");
    let destination = directory.join("destination");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&destination).unwrap();
    let source = source_dir.join("report.txt");
    let kept = destination.join(kept_name("report.txt"));
    let restored = source_dir.join(kept_name("report.txt"));
    fs::write(&source, "moved").unwrap();
    fs::write(destination.join("report.txt"), "already there").unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let request = FileOperationRequest {
        kind: FileOperationKind::Move,
        sources: vec![source.clone()],
        destination: Some(destination.clone()),
        conflict_policy: ConflictPolicy::Rename,
    };

    let undone = pollster::block_on(async {
        let result = await_file_operation(services.clone(), request.clone(), None)
            .await
            .unwrap();
        fs::write(&source, "recreated").unwrap();
        let record = UndoRecord::from_file_operation(request, result).unwrap();
        undo_action(record.action, services.clone()).await.unwrap()
    });
    assert_eq!(fs::read_to_string(&source).unwrap(), "recreated");
    assert_eq!(fs::read_to_string(&restored).unwrap(), "moved");

    // Redo moves the restored item, never the unrelated file now at the
    // original name.
    pollster::block_on(redo_action(undone, services, None)).unwrap();
    assert_eq!(fs::read_to_string(&source).unwrap(), "recreated");
    assert!(!restored.exists());
    assert_eq!(fs::read_to_string(&kept).unwrap(), "moved");
    remove_fixture(&directory);
}

#[test]
fn native_move_undo_refuses_to_overwrite_a_recreated_source() {
    let directory = fixture_dir();
    let source_dir = directory.join("source");
    let destination = directory.join("destination");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&destination).unwrap();
    let source = source_dir.join("conflict.txt");
    let target = destination.join("conflict.txt");
    fs::write(&source, "moved value").unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let request = FileOperationRequest {
        kind: FileOperationKind::Move,
        sources: vec![source.clone()],
        destination: Some(destination),
        conflict_policy: ConflictPolicy::Error,
    };

    let error = pollster::block_on(async {
        let result = await_file_operation(services.clone(), request.clone(), None)
            .await
            .unwrap();
        fs::write(&source, "external replacement").unwrap();
        let record = UndoRecord::from_file_operation(request, result).unwrap();
        undo_action(record.action, services).await.unwrap_err()
    });

    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(fs::read_to_string(&source).unwrap(), "external replacement");
    assert_eq!(fs::read_to_string(&target).unwrap(), "moved value");
    remove_fixture(&directory);
}

#[test]
fn native_rename_round_trips_through_undo_and_redo() {
    let directory = fixture_dir();
    let before = directory.join("before.txt");
    fs::write(&before, "rename proof").unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));

    let final_action = pollster::block_on(async {
        let after = PathBuf::from(
            services
                .mutations
                .rename_path(before.clone(), "after.txt".into())
                .await
                .unwrap(),
        );
        let undone = undo_action(
            UndoAction::Rename {
                before: before.clone(),
                after,
            },
            services.clone(),
        )
        .await
        .unwrap();
        assert!(before.is_file());
        assert!(!directory.join("after.txt").exists());
        redo_action(undone, services, None).await.unwrap()
    });

    assert!(!before.exists());
    assert_eq!(
        fs::read_to_string(directory.join("after.txt")).unwrap(),
        "rename proof"
    );
    assert!(matches!(final_action, UndoAction::Rename { .. }));
    remove_fixture(&directory);
}

#[test]
fn watcher_events_are_scoped_and_fail_recoverably() {
    let path = PathBuf::from("current");
    let paths = vec![path.clone()];
    let old_paths = vec![PathBuf::from("old")];
    let changed = WatcherEvent {
        registration_id: 4,
        state: WatcherState::Changed,
        paths: vec![path.join("changed.txt")],
        error: None,
    };
    assert_eq!(
        watcher_disposition(7, &paths, 6, &paths, &changed),
        WatcherDisposition::Ignore
    );
    assert_eq!(
        watcher_disposition(7, &paths, 7, &old_paths, &changed),
        WatcherDisposition::Ignore
    );
    assert_eq!(
        watcher_disposition(7, &paths, 7, &paths, &changed),
        WatcherDisposition::Refresh
    );

    let failed = WatcherEvent {
        registration_id: 4,
        state: WatcherState::Failed,
        paths: vec![path.clone()],
        error: Some(ServiceError::new(
            explorie_native_services::ErrorCode::Io,
            "watch access denied",
        )),
    };
    assert_eq!(
        watcher_disposition(7, &paths, 7, &paths, &failed),
        WatcherDisposition::Stop("watch access denied".to_string())
    );
}

#[gpui::test]
fn watcher_bursts_allow_in_flight_listings_to_finish(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(window, |view, cx| {
        for mode in [ViewMode::List, ViewMode::Grid, ViewMode::Column] {
            view.browser.set_view_mode(mode);
            view.start_listing(cx);
            let generation = if mode == ViewMode::Column {
                view.column_view.generation
            } else {
                view.listing.generation
            };
            let paths = view.watched_paths();
            for _ in 0..20 {
                assert!(view.apply_watcher_event(
                    view.watcher.generation,
                    &paths,
                    WatcherEvent {
                        registration_id: 1,
                        state: WatcherState::Changed,
                        paths: vec![directory.join("changed.txt")],
                        error: None,
                    },
                    cx,
                ));
            }
            let current = if mode == ViewMode::Column {
                view.column_view.generation
            } else {
                view.listing.generation
            };
            assert_eq!(current, generation, "watcher canceled {mode:?} listing");
            assert!(view.watcher.refresh_pending);

            let stale = DirectoryEvent::Failed {
                generation: generation.wrapping_sub(1),
                request: ListRequest {
                    path: directory.clone(),
                    calc_dir_size: false,
                },
                error: ServiceError::new(ErrorCode::Io, "stale failure"),
            };
            if mode == ViewMode::Column {
                view.apply_column_event(stale, cx);
            } else {
                view.apply_event(stale, cx);
            }
            assert!(view.watcher.refresh_pending);
            assert!(view.listing_in_flight());

            // Deliver a held listing result after the burst, without relying on
            // filesystem speed or test-executor scheduling to produce the race.
            let listing_paths = if mode == ViewMode::Column {
                view.column_view.columns.paths()
            } else {
                vec![directory.clone()]
            };
            for path in &listing_paths {
                if mode == ViewMode::Column && path != &directory {
                    view.apply_column_event(
                        DirectoryEvent::Failed {
                            generation,
                            request: ListRequest {
                                path: path.clone(),
                                calc_dir_size: false,
                            },
                            error: ServiceError::new(ErrorCode::Io, "ancestor unavailable"),
                        },
                        cx,
                    );
                    continue;
                }
                let event = DirectoryEvent::Listed {
                    generation,
                    request: ListRequest {
                        path: path.clone(),
                        calc_dir_size: false,
                    },
                    entries: vec![absolute_entry(path.join("before-refresh.txt"))],
                    warning: None,
                };
                if mode == ViewMode::Column {
                    view.apply_column_event(event, cx);
                } else {
                    view.apply_event(event, cx);
                }
            }
            assert_eq!(
                view.browser.entries()[0].path,
                directory.join("before-refresh.txt")
            );
            assert!(!view.watcher.refresh_pending);
            let refreshed = if mode == ViewMode::Column {
                view.column_view.generation
            } else {
                view.listing.generation
            };
            assert_eq!(refreshed, generation + 1, "burst must schedule one refresh");
            for path in listing_paths {
                let event = DirectoryEvent::Listed {
                    generation: refreshed,
                    request: ListRequest {
                        path: path.clone(),
                        calc_dir_size: false,
                    },
                    entries: vec![absolute_entry(path.join("after-refresh.txt"))],
                    warning: None,
                };
                if mode == ViewMode::Column {
                    view.apply_column_event(event, cx);
                } else {
                    view.apply_event(event, cx);
                }
            }
            assert!(!view.listing_in_flight());
            assert!(matches!(view.listing.state, ListingState::Ready));
            assert_eq!(
                view.browser.entries()[0].path,
                directory.join("after-refresh.txt")
            );
            view.watcher.refresh_pending = true;
            view.start_listing(cx);
            assert!(
                !view.watcher.refresh_pending,
                "new navigation clears old refresh"
            );
            view.listing.task = None;
            view.column_view.tasks.clear();
        }
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn listing_warnings_reach_the_status_line_and_toast_once(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    fs::write(directory.join("notes.txt"), "notes").unwrap();
    fs::write(directory.join(".explorie.json"), "{ not json").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(cx, |view, cx| view.start_listing(cx));
    wait_for_view(&view, cx, "the first listing", |view| {
        matches!(view.listing.state, ListingState::Ready) && view.listing.warning.is_some()
    });
    view.update(cx, |view, cx| {
        assert_eq!(view.browser.entries().len(), 1);
        let warning = view.current_listing_warning().unwrap().to_string();
        assert!(warning.contains(".explorie.json"), "{warning}");
        let toast = view
            .toasts
            .current
            .as_ref()
            .expect("the warning is announced");
        assert_eq!(toast.kind, ToastKind::Warning);
        assert_eq!(toast.message, warning);
        view.dismiss_toast(cx);
        view.refresh(cx);
    });
    wait_for_view(&view, cx, "the refresh", |view| {
        matches!(view.listing.state, ListingState::Ready)
    });
    view.update(cx, |view, _| {
        assert!(
            view.toasts.current.is_none(),
            "a refresh must not repeat the toast"
        );
        assert!(view.toasts.queue.is_empty());
        assert!(
            view.current_listing_warning()
                .is_some_and(|warning| warning.contains(".explorie.json")),
            "the status line keeps the warning"
        );
    });

    fs::remove_file(directory.join(".explorie.json")).unwrap();
    view.update(cx, |view, cx| view.refresh(cx));
    wait_for_view(&view, cx, "the clean listing", |view| {
        matches!(view.listing.state, ListingState::Ready) && view.listing.warning.is_none()
    });
    view.update(cx, |view, _| {
        assert_eq!(view.current_listing_warning(), None);
        assert!(view.listing.announced_warnings.is_empty());
    });
    remove_fixture(&directory);
}

/// Run the test executor until `done` holds, letting native blocking tasks
/// (listings) finish on their own threads.
fn wait_for_view(
    view: &Entity<DirectoryWindow>,
    cx: &mut gpui::VisualTestContext,
    what: &str,
    mut done: impl FnMut(&DirectoryWindow) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        cx.run_until_parked();
        if view.update(cx, |view, _| done(view)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn changed_event(paths: Vec<PathBuf>) -> WatcherEvent {
    WatcherEvent {
        registration_id: 1,
        state: WatcherState::Changed,
        paths,
        error: None,
    }
}

#[gpui::test]
fn watcher_changes_patch_listed_entries_without_relisting(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    for (name, contents) in [
        ("keep.txt", "keep"),
        ("delete.txt", "delete"),
        ("modify.txt", "1"),
        ("rename-me.txt", "rename"),
    ] {
        fs::write(directory.join(name), contents).unwrap();
    }
    let resources = fixture_dir();
    let services = NativeServices::new(ResourcePaths::test(&resources));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(cx, |view, cx| view.start_listing(cx));
    wait_for_view(&view, cx, "the initial listing", |view| {
        matches!(view.listing.state, ListingState::Ready) && view.browser.entries().len() == 4
    });
    let generation = view.update(cx, |view, _| {
        // Deliver events by hand; the native watcher's timing is not under test.
        view.watcher.task = None;
        view.browser.select(directory.join("keep.txt"));
        view.listing.generation
    });

    fs::write(directory.join("created.txt"), "new").unwrap();
    fs::remove_file(directory.join("delete.txt")).unwrap();
    fs::write(directory.join("modify.txt"), "12345").unwrap();
    fs::rename(
        directory.join("rename-me.txt"),
        directory.join("renamed.txt"),
    )
    .unwrap();
    view.update(cx, |view, cx| {
        let changed = ["created.txt", "delete.txt", "modify.txt", "rename-me.txt"]
            .into_iter()
            .map(|name| directory.join(name))
            .collect();
        assert!(view.apply_watcher_event(
            view.watcher.generation,
            &view.watched_paths(),
            changed_event(changed),
            cx,
        ));
        // A burst while the first patch is in flight is applied after it.
        assert!(view.apply_watcher_event(
            view.watcher.generation,
            &view.watched_paths(),
            changed_event(vec![directory.join("renamed.txt")]),
            cx,
        ));
        assert!(view.watcher.patch_task.is_some());
    });
    wait_for_view(&view, cx, "the watcher patch", |view| {
        view.watcher.patch_task.is_none()
    });
    view.update(cx, |view, _| {
        let names: Vec<_> = view
            .browser
            .visible_entries()
            .iter()
            .map(|entry| file_name(entry))
            .collect();
        assert_eq!(
            names,
            ["created.txt", "keep.txt", "modify.txt", "renamed.txt"]
        );
        let modified = view
            .browser
            .visible_entries()
            .iter()
            .find(|entry| entry.path.ends_with("modify.txt"))
            .unwrap();
        assert_eq!(modified.size, 5);
        assert_eq!(view.listing.generation, generation, "patches never re-list");
        assert!(matches!(view.listing.state, ListingState::Ready));
        assert_eq!(
            view.browser.selected_path(),
            Some(directory.join("keep.txt").as_path())
        );
    });

    // A folder that failed to list is re-listed rather than patched.
    view.update(cx, |view, cx| {
        view.listing.state = ListingState::Failed("unreadable".to_string());
        assert!(view.apply_watcher_event(
            view.watcher.generation,
            &view.watched_paths(),
            changed_event(vec![directory.join("keep.txt")]),
            cx,
        ));
        assert_eq!(view.listing.generation, generation + 1);
        assert!(view.watcher.patch_task.is_none());
    });
    wait_for_view(&view, cx, "the re-listing", |view| {
        matches!(view.listing.state, ListingState::Ready)
    });

    // Overflow arrives as the watched root: fall back to a full refresh.
    view.update(cx, |view, cx| {
        assert!(view.apply_watcher_event(
            view.watcher.generation,
            &view.watched_paths(),
            changed_event(vec![directory.clone()]),
            cx,
        ));
        assert_eq!(view.listing.generation, generation + 2);
        assert!(view.watcher.patch_task.is_none());
    });
    wait_for_view(&view, cx, "the fallback listing", |view| {
        matches!(view.listing.state, ListingState::Ready)
    });
    remove_fixture(&directory);
    remove_fixture(&resources);
}

#[gpui::test]
fn column_watcher_changes_patch_only_the_affected_columns(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let leaf = directory.join("leaf");
    fs::create_dir(&leaf).unwrap();
    fs::write(leaf.join("inside.txt"), "inside").unwrap();
    let resources = fixture_dir();
    let services = NativeServices::new(ResourcePaths::test(&resources));
    let (view, cx) = cx.add_window_view(|_, cx| DirectoryWindow::new(leaf.clone(), services, cx));
    view.update(cx, |view, cx| view.set_view_mode(ViewMode::Column, cx));
    wait_for_view(&view, cx, "the column listings", |view| {
        !view.listing_in_flight() && view.browser.entries().len() == 1
    });
    let generation = view.update(cx, |view, _| {
        view.watcher.task = None;
        view.column_view.generation
    });

    fs::write(leaf.join("created.txt"), "new").unwrap();
    fs::write(directory.join("sibling.txt"), "sibling").unwrap();
    view.update(cx, |view, cx| {
        assert!(view.apply_watcher_event(
            view.watcher.generation,
            &view.watched_paths(),
            changed_event(vec![
                leaf.join("created.txt"),
                directory.join("sibling.txt")
            ]),
            cx,
        ));
    });
    wait_for_view(&view, cx, "the column patch", |view| {
        view.watcher.patch_task.is_none()
    });
    view.update(cx, |view, _| {
        assert_eq!(
            view.column_view.generation, generation,
            "patches never re-list"
        );
        let column_names = |path: &Path| -> Vec<String> {
            view.column_view
                .columns
                .columns()
                .iter()
                .find(|column| column.path() == path)
                .unwrap()
                .visible_entries(&view.browser)
                .iter()
                .map(|entry| file_name(entry))
                .collect()
        };
        assert_eq!(column_names(&leaf), ["created.txt", "inside.txt"]);
        assert_eq!(column_names(&directory), ["leaf", "sibling.txt"]);
        assert_eq!(view.browser.entries().len(), 2);
    });
    remove_fixture(&directory);
    remove_fixture(&resources);
}

#[gpui::test]
fn watcher_invalidated_smart_search_restarts_but_user_cancel_stays_stopped(
    cx: &mut TestAppContext,
) {
    let directory = fixture_dir();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(window, |view, cx| {
        let id = view.browser.save_smart_folder(
            "Text".into(),
            SearchCriteria {
                search_paths: vec![directory.clone()],
                ..SearchCriteria::default()
            },
        );
        assert!(view.browser.activate_smart_folder(id));
        view.start_listing(cx);
        let generation = view.search.generation;
        assert!(view.apply_watcher_event(
            view.watcher.generation,
            &view.watched_paths(),
            WatcherEvent {
                registration_id: 1,
                state: WatcherState::Changed,
                paths: vec![directory.join("new.txt")],
                error: None,
            },
            cx
        ));
        assert_eq!(view.search.generation, generation + 1);
        view.apply_search_event(
            SearchEvent::Failed {
                generation,
                error: ServiceError::new(ErrorCode::Cancelled, "invalidated search"),
            },
            cx,
        );
        assert!(view.search.task.is_some());
        view.cancel_smart_search(cx);
        assert!(view.search.task.is_none());
        assert!(!view.watcher.refresh_pending);
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn bound_actions_switch_between_native_views(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("ctrl-1", ShowListView, Some("browser")),
            KeyBinding::new("ctrl-2", ShowGridView, Some("browser")),
            KeyBinding::new("ctrl-3", ShowColumnView, Some("browser")),
            KeyBinding::new("ctrl-f", FocusSearch, Some("browser")),
        ]);
        cx.open_window(Default::default(), |window, cx| {
            let view = cx.new(|cx| {
                DirectoryWindow::new(PathBuf::from("sample"), NativeServices::default(), cx)
            });
            window.focus(&view.focus_handle(cx), cx);
            view
        })
        .unwrap()
    });

    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-2").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.browser.view_mode(), ViewMode::Grid);
        })
        .unwrap();

    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-f").unwrap());
    cx.dispatch_keystroke(*window, Keystroke::parse("a").unwrap().with_simulated_ime());
    window
        .update(cx, |view, _, _| {
            assert!(view.search.active);
            assert_eq!(view.browser.search_query(), "a");
        })
        .unwrap();
    cx.dispatch_keystroke(*window, Keystroke::parse("escape").unwrap());
    window
        .update(cx, |view, _, _| {
            assert!(!view.search.active);
            assert!(view.browser.search_query().is_empty());
        })
        .unwrap();

    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-3").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.browser.view_mode(), ViewMode::Column);
            assert_eq!(
                view.column_view.columns.paths().last(),
                Some(&PathBuf::from("sample"))
            );
            assert_eq!(
                view.column_view.tasks.len(),
                view.column_view.columns.paths().len()
            );
        })
        .unwrap();

    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-1").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.browser.view_mode(), ViewMode::List);
            assert!(view.column_view.tasks.is_empty());
        })
        .unwrap();
}

#[gpui::test]
fn retained_plain_key_shortcuts_go_up_and_resize_grid(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let child = directory.join("child");
    fs::create_dir(&child).unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) = cx.add_window_view(|_, cx| DirectoryWindow::new(child.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.set_view_mode(ViewMode::Grid, cx);
        view.settings.appearance.grid_min_width = 250;

        view.handle_type_select_key(
            &KeyDownEvent {
                keystroke: Keystroke::parse("=").unwrap().with_simulated_ime(),
                is_held: false,
                prefer_character_input: false,
            },
            cx,
        );
        assert_eq!(view.settings.appearance.grid_min_width, 260);

        view.handle_type_select_key(
            &KeyDownEvent {
                keystroke: Keystroke::parse("=").unwrap().with_simulated_ime(),
                is_held: false,
                prefer_character_input: false,
            },
            cx,
        );
        assert_eq!(view.settings.appearance.grid_min_width, 260);

        view.handle_type_select_key(
            &KeyDownEvent {
                keystroke: Keystroke::parse("-").unwrap().with_simulated_ime(),
                is_held: false,
                prefer_character_input: false,
            },
            cx,
        );
        assert_eq!(view.settings.appearance.grid_min_width, 250);

        view.handle_type_select_key(
            &KeyDownEvent {
                keystroke: Keystroke::parse("backspace").unwrap(),
                is_held: false,
                prefer_character_input: false,
            },
            cx,
        );
        // Explorer navigates up on Backspace; Finder ignores a plain Backspace.
        assert_eq!(
            view.browser.path(),
            if cfg!(target_os = "macos") {
                child.as_path()
            } else {
                directory.as_path()
            }
        );
    });

    cx.run_until_parked();
    remove_fixture(&directory);
}

#[gpui::test]
fn bound_actions_drive_tabs_favorites_and_multi_selection(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("ctrl-a", SelectAll, Some("browser")),
            KeyBinding::new("escape", ClearSelection, Some("browser")),
            KeyBinding::new("down", SelectNext, Some("browser")),
            KeyBinding::new("shift-down", SelectNextRange, Some("browser")),
            KeyBinding::new("ctrl-t", NewTab, Some("browser")),
            KeyBinding::new("ctrl-w", CloseTab, Some("browser")),
            KeyBinding::new("ctrl-tab", NextTab, Some("browser")),
            KeyBinding::new("ctrl-d", ToggleFavorite, Some("browser")),
            KeyBinding::new("ctrl-shift-right", MoveTabRight, Some("browser")),
            KeyBinding::new("ctrl-shift-g", SaveSearch, Some("browser")),
        ]);
        cx.open_window(Default::default(), |window, cx| {
            let view = cx.new(|cx| {
                let mut view =
                    DirectoryWindow::new(PathBuf::from("sample"), NativeServices::default(), cx);
                view.browser.replace_entries(vec![
                    fixture_entry("a.txt"),
                    fixture_entry("b.txt"),
                    fixture_entry("c.txt"),
                ]);
                view.listing.state = ListingState::Ready;
                view
            });
            window.focus(&view.focus_handle(cx), cx);
            view
        })
        .unwrap()
    });

    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-a").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.browser.selection_count(), 3);
        })
        .unwrap();
    cx.dispatch_keystroke(*window, Keystroke::parse("escape").unwrap());
    cx.dispatch_keystroke(*window, Keystroke::parse("down").unwrap());
    cx.dispatch_keystroke(*window, Keystroke::parse("shift-down").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.browser.selection_count(), 2);
        })
        .unwrap();
    cx.dispatch_keystroke(*window, Keystroke::parse("escape").unwrap());
    cx.dispatch_keystroke(*window, Keystroke::parse("b").unwrap().with_simulated_ime());
    window
        .update(cx, |view, _, _| {
            assert_eq!(
                view.browser.selected_path(),
                Some(Path::new("sample/b.txt"))
            );
        })
        .unwrap();

    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-d").unwrap());
    window
        .update(cx, |view, _, _| {
            assert!(view.browser.is_favorite(Path::new("sample")));
            view.browser.set_search_query("a".to_string());
        })
        .unwrap();
    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-shift-g").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.overlay.surface, ControlSurface::SmartFolders);
            assert_eq!(view.smart_folder_editor.as_ref().unwrap().name_pattern, "a");
            assert!(view.browser.smart_folders().is_empty());
        })
        .unwrap();
    cx.dispatch_keystroke(*window, Keystroke::parse("escape").unwrap());
    window
        .update(cx, |view, window, cx| {
            assert!(view.settings_ui.confirmation.is_some());
            view.confirm_settings_confirmation(window, cx);
        })
        .unwrap();

    let first = window
        .update(cx, |view, _, _| view.browser.active_tab_id())
        .unwrap();
    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-t").unwrap());
    let second = window
        .update(cx, |view, _, _| {
            assert_eq!(view.browser.tabs().len(), 2);
            view.browser.active_tab_id()
        })
        .unwrap();
    assert_ne!(first, second);

    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-tab").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.browser.active_tab_id(), first);
        })
        .unwrap();
    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-shift-right").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.browser.tabs()[1].id(), first);
        })
        .unwrap();
    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-w").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.browser.tabs().len(), 1);
            assert_eq!(view.browser.active_tab_id(), second);
        })
        .unwrap();
}

#[cfg(target_os = "macos")]
#[gpui::test]
fn close_shortcut_closes_the_last_tab_as_a_window_without_quitting(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    cx.update(|cx| {
        cx.bind_keys([KeyBinding::new("cmd-w", CloseTab, Some("browser"))]);
    });
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            let view = cx.new(|cx| {
                DirectoryWindow::new(
                    directory.clone(),
                    NativeServices::new(ResourcePaths::test(&directory)),
                    cx,
                )
            });
            window.focus(&view.focus_handle(cx), cx);
            view
        })
        .unwrap()
    });

    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
    cx.dispatch_keystroke(*window, Keystroke::parse("cmd-w").unwrap());
    cx.run_until_parked();
    assert!(cx.update(|cx| cx.windows().is_empty()));

    remove_fixture(&directory);
}

#[gpui::test]
fn smart_folder_editor_creates_validates_edits_and_persists_full_criteria(cx: &mut TestAppContext) {
    let config = fixture_dir();
    let directory = config.join("source");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("report.txt"), "needle").unwrap();
    let resources = ResourcePaths::test(&config);
    let services = NativeServices::new(resources.clone());
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::restore(directory.clone(), false, services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });
    view.update(window, |view, cx| {
        view.browser.set_search_query("report".to_string());
        view.browser.set_filter(EntryFilter::All);
        view.open_smart_folder_editor(None, cx);
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.run_until_parked();

    assert!(
        window.debug_bounds("control-surface-backdrop").is_some(),
        "smart-folder state did not produce the shared backdrop"
    );
    assert!(window.debug_bounds("smart-folder-editor").is_some());
    assert!(window.debug_bounds("smart-folder-field-name").is_some());
    assert!(
        window
            .debug_bounds("smart-folder-field-modified-before")
            .is_some()
    );
    assert!(window.debug_bounds("smart-folder-field-tag").is_some());
    view.update(window, |view, _| {
        let editor = view.smart_folder_editor.as_ref().unwrap();
        assert_eq!(editor.name, "Search: report");
        assert_eq!(editor.name_pattern, "report");
        assert_eq!(editor.search_paths, directory.to_string_lossy());
    });

    view.update(window, |view, cx| {
        view.select_smart_folder_field(SmartFolderField::Name, cx);
        view.update_smart_folder_query("Reports".to_string(), cx);
    });
    window.simulate_keystrokes("tab");
    view.update(window, |view, _| {
        assert_eq!(
            view.smart_folder_editor.as_ref().unwrap().field,
            SmartFolderField::SearchPaths
        );
    });
    view.update(window, |view, cx| {
        view.select_smart_folder_field(SmartFolderField::NamePattern, cx);
        view.update_smart_folder_query("report.*".to_string(), cx);
        view.select_smart_folder_field(SmartFolderField::Extensions, cx);
        view.update_smart_folder_query("txt, md".to_string(), cx);
    });
    window.run_until_parked();
    for selector in [
        "smart-folder-type",
        "smart-folder-regex",
        "smart-folder-recursive",
        "smart-folder-combine",
    ] {
        assert!(
            window.debug_bounds(selector).is_some(),
            "{selector} is not initially reachable at the minimum window size"
        );
    }
    view.update(window, |view, cx| {
        view.cycle_smart_folder_type(cx);
        view.toggle_smart_folder_regex(cx);
        view.toggle_smart_folder_recursive(cx);
        view.toggle_smart_folder_combine_mode(cx);
    });
    view.update(window, |view, _| {
        let editor = view.smart_folder_editor.as_ref().unwrap();
        assert_eq!(editor.type_filter, SearchType::Files);
        assert!(editor.name_regex);
        assert!(!editor.recursive);
        assert_eq!(editor.combine_mode, CombineMode::Or);
        editor.validate().expect("complete editor draft is valid");
    });

    let save = window.debug_bounds("save-smart-folder").unwrap().center();
    window.simulate_click(save, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(view.overlay.surface, ControlSurface::Closed);
        assert_eq!(view.browser.smart_folders().len(), 1);
        let folder = &view.browser.smart_folders()[0];
        assert_eq!(folder.name(), "Reports");
        assert_eq!(folder.criteria().name_pattern.as_deref(), Some("report.*"));
        assert_eq!(folder.criteria().extensions, ["txt", "md"]);
        assert_eq!(folder.criteria().type_filter, SearchType::Files);
        assert_eq!(folder.criteria().combine_mode, CombineMode::Or);
        assert!(!folder.criteria().recursive);
    });

    let id = view.update(window, |view, _| view.browser.smart_folders()[0].id());
    view.update(window, |view, cx| {
        view.open_smart_folder_editor(Some(id), cx);
        view.select_smart_folder_field(SmartFolderField::NamePattern, cx);
        view.update_smart_folder_query("[".to_string(), cx);
    });
    window.run_until_parked();
    let save = window.debug_bounds("save-smart-folder").unwrap().center();
    window.simulate_click(save, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(view.overlay.surface, ControlSurface::SmartFolders);
        assert!(view.smart_folder_editor.is_some());
        assert!(
            view.toasts
                .current
                .as_ref()
                .is_some_and(|toast| toast.message.starts_with("Name pattern is not valid regex"))
        );
    });

    view.update(window, |view, cx| {
        view.select_smart_folder_field(SmartFolderField::NamePattern, cx);
        view.update_smart_folder_query("report.*".to_string(), cx);
        view.select_smart_folder_field(SmartFolderField::ContentSearch, cx);
        view.update_smart_folder_query("needle".to_string(), cx);
        view.select_smart_folder_field(SmartFolderField::Tag, cx);
        view.update_smart_folder_query("Work".to_string(), cx);
    });
    window.run_until_parked();
    let save = window.debug_bounds("save-smart-folder").unwrap().center();
    window.simulate_click(save, gpui::Modifiers::default());
    view.update(window, |view, _| {
        assert_eq!(view.overlay.surface, ControlSurface::Closed);
        assert_eq!(
            view.browser.smart_folders()[0]
                .criteria()
                .content_search
                .as_deref(),
            Some("needle")
        );
        view.session_store.as_ref().unwrap().flush();
    });

    let restart_services = NativeServices::new(resources);
    let (restarted, restarted_cx) = cx.add_window_view(|_, cx| {
        DirectoryWindow::restore(directory.clone(), false, restart_services, cx)
    });
    restarted.update(restarted_cx, |view, _| {
        assert_eq!(view.browser.smart_folders().len(), 1);
        let folder = &view.browser.smart_folders()[0];
        assert_eq!(folder.name(), "Reports");
        assert_eq!(folder.criteria().content_search.as_deref(), Some("needle"));
        assert_eq!(folder.criteria().tag.as_deref(), Some("Work"));
        assert_eq!(folder.criteria().extensions, ["txt", "md"]);
    });

    remove_fixture(&config);
}

#[gpui::test]
fn bound_undo_and_redo_actions_round_trip_a_native_rename(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let before = directory.join("before.txt");
    fs::write(&before, "bound recovery").unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let after = PathBuf::from(
        services
            .mutations
            .rename_path(before.clone(), "after.txt".into())
            .wait()
            .unwrap(),
    );
    let window = cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("ctrl-z", Undo, Some("browser")),
            KeyBinding::new("ctrl-shift-z", Redo, Some("browser")),
        ]);
        cx.open_window(Default::default(), |window, cx| {
            let view = cx.new(|cx| {
                let mut view = DirectoryWindow::new(directory.clone(), services, cx);
                view.undo_ledger
                    .push(UndoRecord::renamed(before.clone(), after.clone()));
                view
            });
            window.focus(&view.focus_handle(cx), cx);
            view
        })
        .unwrap()
    });

    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-z").unwrap());
    window
        .update(cx, |view, _, _| {
            assert!(
                view.undo_ledger.is_processing(),
                "undo action did not start: {:?}",
                view.status_message
            );
        })
        .unwrap();
    cx.run_until_parked();
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        let completed = window
            .update(cx, |view, _, _| !view.undo_ledger.is_processing())
            .unwrap();
        if completed {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(before.is_file());
    assert!(!after.exists());
    window
        .update(cx, |view, _, _| {
            assert!(view.undo_ledger.can_redo());
        })
        .unwrap();

    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-shift-z").unwrap());
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        let completed = window
            .update(cx, |view, _, _| !view.undo_ledger.is_processing())
            .unwrap();
        if completed {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!before.exists());
    assert_eq!(fs::read_to_string(&after).unwrap(), "bound recovery");
    window
        .update(cx, |view, _, _| {
            assert!(view.undo_ledger.can_undo(SystemTime::now()));
        })
        .unwrap();
    remove_fixture(&directory);
}

#[test]
fn cancelled_undo_retries_safely_after_a_completed_prefix() {
    const ITEM_COUNT: usize = 3;
    let root = fixture_dir();
    let source_dir = root.join("source");
    let target_dir = root.join("target");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&target_dir).unwrap();
    let mut sources = Vec::with_capacity(ITEM_COUNT);
    let mut targets = Vec::with_capacity(ITEM_COUNT);
    for index in 0..ITEM_COUNT {
        let name = format!("item-{index:04}.txt");
        sources.push(source_dir.join(&name));
        let target = target_dir.join(name);
        fs::write(&target, format!("item {index}")).unwrap();
        targets.push(target);
    }
    let action = UndoAction::Move {
        request: FileOperationRequest {
            kind: FileOperationKind::Move,
            sources: sources.clone(),
            destination: Some(target_dir.clone()),
            conflict_policy: ConflictPolicy::Error,
        },
        pairs: sources
            .iter()
            .cloned()
            .zip(targets.iter().cloned())
            .collect(),
    };
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&root));
    let cancellation = Arc::new(AtomicBool::new(false));
    let result = pollster::block_on(undo_action_with_progress(
        action.clone(),
        services.clone(),
        None,
        Arc::clone(&cancellation),
        |update| {
            if matches!(update, UndoProgressUpdate::ItemCompleted) {
                cancellation.store(true, Ordering::Release);
            }
        },
    ));
    assert_eq!(result.unwrap_err().code, ErrorCode::Cancelled);
    assert_eq!(sources.iter().filter(|path| path.is_file()).count(), 1);
    assert_eq!(targets.iter().filter(|path| path.is_file()).count(), 2);

    pollster::block_on(undo_action_with_progress(
        action,
        services,
        None,
        Arc::new(AtomicBool::new(false)),
        |_| {},
    ))
    .unwrap();
    assert!(sources.iter().all(|path| path.is_file()));
    assert!(targets.iter().all(|path| !path.exists()));
    remove_fixture(&root);
}

#[gpui::test]
fn undo_progress_renders_and_escape_requests_cancellation(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.bind_keys([KeyBinding::new("escape", ClearSelection, Some("browser"))]);
    });
    let root = fixture_dir();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&root));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(root.clone(), services, cx));
    window.simulate_resize(gpui::size(px(900.0), px(650.0)));
    let cancellation = Arc::new(AtomicBool::new(false));
    view.update(window, |view, cx| {
        view.listing.state = ListingState::Ready;
        view.operation_ui.undo_progress = Some(UndoProgressState {
            description: "Move 3 item(s)".to_string(),
            completed_items: 1,
            total_items: 3,
            processed_bytes: 4,
            total_bytes: 10,
            current_job_id: None,
            cancellation: Arc::clone(&cancellation),
            cancelling: false,
        });
        cx.notify();
    });
    window.run_until_parked();

    assert!(window.debug_bounds("cancel-undo").is_some());
    assert!(window.debug_bounds("undo-progress").is_some());
    let focus = view.update(window, |view, _| view.focus_handle.clone());
    window.update(|window, cx| window.focus(&focus, cx));
    window.simulate_keystrokes("escape");
    view.update(window, |view, _| {
        assert!(view.operation_ui.undo_progress.as_ref().unwrap().cancelling);
        assert_eq!(view.status_message.as_deref(), Some("Cancelling undo…"));
    });
    assert!(cancellation.load(Ordering::Acquire));
    remove_fixture(&root);
}

#[gpui::test]
fn smart_folder_results_apply_only_to_the_active_generation(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, cx| {
        DirectoryWindow::new(PathBuf::from("sample"), NativeServices::default(), cx)
    });
    view.update(cx, |view, cx| {
        let id = view.browser.save_smart_folder(
            "Text".into(),
            SearchCriteria {
                name_pattern: Some("text".into()),
                search_paths: vec![PathBuf::from("sample")],
                recursive: true,
                ..SearchCriteria::default()
            },
        );
        assert!(view.browser.activate_smart_folder(id));
        view.search.generation = 4;
        view.apply_search_event(
            SearchEvent::Completed {
                generation: 3,
                result: SearchResult {
                    entries: vec![fixture_entry("stale.txt")],
                    indexed_entries: 1,
                    content_reads: 0,
                    reused_index: true,
                    truncated: false,
                    source: explorie_native_services::SearchSource::Crawler,
                },
            },
            cx,
        );
        assert!(view.browser.visible_entries().is_empty());

        view.apply_search_event(
            SearchEvent::Completed {
                generation: 4,
                result: SearchResult {
                    entries: vec![fixture_entry("text.txt")],
                    indexed_entries: 1,
                    content_reads: 0,
                    reused_index: true,
                    truncated: false,
                    source: explorie_native_services::SearchSource::Crawler,
                },
            },
            cx,
        );
        assert_eq!(view.browser.visible_entries().len(), 1);
        assert!(view.browser.visible_entries()[0].path.ends_with("text.txt"));
        assert!(matches!(view.listing.state, ListingState::Ready));
    });
}

#[test]
fn smart_folder_status_names_spotlight_and_partial_results() {
    let result = |source, reused_index, truncated| SearchResult {
        entries: vec![fixture_entry("a.txt"), fixture_entry("b.txt")],
        indexed_entries: 40,
        content_reads: 0,
        reused_index,
        truncated,
        source,
    };
    assert_eq!(
        crate::window::search::search_result_status(
            &result(SearchSource::Spotlight, true, false),
            None
        ),
        "2 smart-folder results • Spotlight"
    );
    assert_eq!(
        crate::window::search::search_result_status(
            &result(SearchSource::Spotlight, true, true),
            None
        ),
        "2 smart-folder results (partial results) • Spotlight"
    );
    assert_eq!(
        crate::window::search::search_result_status(
            &result(SearchSource::Crawler, true, false),
            None
        ),
        "2 smart-folder results • cached index"
    );
    assert_eq!(
        crate::window::search::search_result_status(
            &result(SearchSource::Crawler, false, true),
            None
        ),
        "2 smart-folder results (partial results) • 40 paths indexed"
    );
    assert_eq!(
        crate::window::search::search_result_status(
            &result(SearchSource::Mixed, false, false),
            None
        ),
        "2 smart-folder results • Spotlight + 40 paths indexed"
    );
}

#[gpui::test]
fn spotlight_searches_report_spotlight_in_the_status_line(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, cx| {
        DirectoryWindow::new(PathBuf::from("sample"), NativeServices::default(), cx)
    });
    view.update(cx, |view, cx| {
        let id = view.browser.save_smart_folder(
            "Text".into(),
            SearchCriteria {
                name_pattern: Some("text".into()),
                search_paths: vec![PathBuf::from("sample")],
                recursive: true,
                ..SearchCriteria::default()
            },
        );
        assert!(view.browser.activate_smart_folder(id));
        view.search.generation = 7;
        view.search.request_id = Some("spotlight-request".to_string());
        view.apply_search_progress(
            SearchProgressEvent {
                request_id: "spotlight-request".to_string(),
                phase: "spotlight".to_string(),
                indexed_entries: 512,
                matched_entries: 3,
                current_path: PathBuf::from("sample/text.txt"),
                entries: Vec::new(),
            },
            cx,
        );
        assert_eq!(
            view.status_message.as_deref(),
            Some("Searching Spotlight • 3 result(s) • Esc to stop")
        );
        view.apply_search_event(
            SearchEvent::Completed {
                generation: 7,
                result: SearchResult {
                    entries: vec![fixture_entry("text.txt")],
                    indexed_entries: 1,
                    content_reads: 0,
                    reused_index: true,
                    truncated: true,
                    source: SearchSource::Spotlight,
                },
            },
            cx,
        );
        let status = view.status_message.clone().unwrap();
        assert_eq!(
            status,
            "1 smart-folder result (partial results) • Spotlight"
        );
        assert!(!status.contains("cached index"));
    });
}

#[gpui::test]
fn native_workspace_manager_persists_save_rename_delete_and_renders(cx: &mut TestAppContext) {
    let root = fixture_dir();
    let first = root.join("first");
    let second = root.join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let resources = explorie_native_services::ResourcePaths::test(&root);
    let services = NativeServices::new(resources.clone());
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::restore(first.clone(), false, services, cx));

    let workspace_id = view.update(cx, |view, cx| {
        view.browser.new_tab();
        assert!(view.browser.navigate(second.clone()));
        view.settings.view.view_mode = ViewMode::Grid;
        view.apply_global_browser_preferences();
        view.open_workspace_manager(cx);
        view.overlay.query = "Editing".to_string();
        view.save_workspace(cx);
        let id = view.workspaces.workspaces()[0].id.clone();
        view.begin_workspace_rename(id.clone(), cx);
        view.overlay.query = "Editing renamed".to_string();
        view.commit_workspace_rename(cx);
        view.workspace_store.as_ref().unwrap().flush();
        id
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );

    let restart_services = NativeServices::new(resources.clone());
    let (restarted, restarted_cx) = cx.add_window_view(|_, cx| {
        DirectoryWindow::restore(first.clone(), false, restart_services, cx)
    });
    restarted.update(restarted_cx, |view, cx| {
        assert_eq!(view.workspaces.workspaces().len(), 1);
        assert_eq!(
            view.workspaces.get(&workspace_id).unwrap().name,
            "Editing renamed"
        );
        view.confirm_workspace_delete(workspace_id.clone(), cx);
        assert!(view.workspaces.workspaces().is_empty());
        view.workspace_store.as_ref().unwrap().flush();
    });
    assert!(resources.config_dir.join("workspaces-v1.json").is_file());
    remove_fixture(&root);
}

#[gpui::test]
fn workspace_load_skips_missing_tabs_and_all_missing_snapshot_is_non_destructive(
    cx: &mut TestAppContext,
) {
    let root = fixture_dir();
    let current = root.join("current");
    let available = root.join("available");
    fs::create_dir(&current).unwrap();
    fs::create_dir(&available).unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&root));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::restore(current.clone(), false, services, cx));

    view.update(cx, |view, cx| {
        let partial = WorkspaceSnapshot {
            tabs: vec![
                WorkspaceTab {
                    id: "available".to_string(),
                    path: available.clone(),
                },
                WorkspaceTab {
                    id: "missing".to_string(),
                    path: root.join("missing"),
                },
            ],
            active_tab_id: "available".to_string(),
            view_mode: ViewMode::Column,
            sort_key: SortKey::Modified,
            sort_direction: SortDirection::Descending,
            show_hidden: true,
            filter_mode: EntryFilter::Folders,
            show_preview_panel: true,
            grid_min_width: 200,
            window: WorkspaceWindowState {
                width: Some(1_100.0),
                height: Some(720.0),
                x: Some(140.0),
                y: Some(90.0),
            },
            sidebar_width: 260.0,
            preview_panel_width: 440.0,
            sidebar_collapsed: true,
        };
        let partial_id = view.workspaces.save_current("Partial", partial).unwrap();
        view.load_workspace(partial_id, cx);
    });
    // Folder availability is checked off the UI thread.
    cx.run_until_parked();
    let path_before = view.update(cx, |view, cx| {
        assert!(view.workspace_ui.load_task.is_none());
        assert_eq!(view.browser.tabs().len(), 1);
        assert_eq!(view.browser.path(), available);
        assert_eq!(view.browser.view_mode(), ViewMode::Column);
        assert!(view.browser.show_hidden());
        assert_eq!(view.browser.filter(), EntryFilter::Folders);
        assert!(view.layout.sidebar_collapsed);
        assert_eq!(view.layout.sidebar_width, 260.0);
        assert_eq!(view.settings.view.sidebar_width, 260.0);
        assert_eq!(view.layout.preview_panel_width, 440.0);
        assert_eq!(view.settings.view.preview_panel_width, 440.0);
        // The next frame applied the saved window geometry.
        assert!(view.layout.pending_workspace_bounds.is_none());
        assert_eq!(view.layout.last_window_bounds.width, Some(1_100.0));
        assert_eq!(view.layout.last_window_bounds.height, Some(720.0));
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|message| message.contains("skipped 1"))
        );

        let path_before = view.browser.path().to_path_buf();
        let missing = WorkspaceSnapshot {
            tabs: vec![WorkspaceTab {
                id: "gone".to_string(),
                path: root.join("gone"),
            }],
            active_tab_id: "gone".to_string(),
            ..view.workspace_snapshot()
        };
        let missing_id = view.workspaces.save_current("Gone", missing).unwrap();
        view.load_workspace(missing_id, cx);
        path_before
    });
    cx.run_until_parked();
    view.update(cx, |view, _| {
        assert_eq!(view.browser.path(), path_before);
        assert!(
            view.toasts
                .current
                .as_ref()
                .is_some_and(|toast| toast.kind == ToastKind::Warning)
        );
        view.session_store.as_ref().unwrap().flush();
        view.settings_store.as_ref().unwrap().flush();
        view.workspace_store.as_ref().unwrap().flush();
    });
    remove_fixture(&root);
}

#[gpui::test]
fn legacy_workspace_map_imports_once_through_native_settings_bridge(cx: &mut TestAppContext) {
    let root = fixture_dir();
    let folder = root.join("legacy-folder");
    fs::create_dir(&folder).unwrap();
    let resources = explorie_native_services::ResourcePaths::test(&root);
    fs::create_dir_all(&resources.config_dir).unwrap();
    let legacy_workspace = serde_json::json!({
        "legacy-one": {
            "id": "legacy-one",
            "name": "Legacy layout",
            "createdAt": 10,
            "updatedAt": 20,
            "tabs": [{"id": "old-tab", "path": folder}],
            "activeTabId": "old-tab",
            "viewMode": "grid",
            "sortKey": "size",
            "sortDir": "desc",
            "showHidden": true,
            "filterMode": "files",
            "showPreviewPanel": true,
            "gridMinWidth": 190
        }
    });
    let legacy = std::collections::BTreeMap::from([
        (
            "explorie:workspaces".to_string(),
            legacy_workspace.to_string(),
        ),
        (
            "explorie:lastWorkspaceId".to_string(),
            "legacy-one".to_string(),
        ),
    ]);
    fs::write(
        resources.config_dir.join("legacy-local-storage.json"),
        serde_json::to_vec_pretty(&legacy).unwrap(),
    )
    .unwrap();
    let services = NativeServices::new(resources.clone());
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::restore(root.clone(), false, services, cx));
    view.update(cx, |view, cx| {
        assert_eq!(view.workspaces.workspaces().len(), 1);
        assert_eq!(view.workspaces.last_workspace_id(), Some("legacy-one"));
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|message| message.contains("legacy workspace"))
        );
        view.load_workspace("legacy-one".to_string(), cx);
    });
    cx.run_until_parked();
    view.update(cx, |view, _| {
        assert_eq!(view.browser.path(), folder);
        assert_eq!(view.browser.view_mode(), ViewMode::Grid);
        assert_eq!(view.browser.sort_key(), SortKey::Size);
        assert!(view.settings.view.show_preview_panel);
        view.workspace_store.as_ref().unwrap().flush();
        // Session and settings writes run on worker threads; finish them
        // before deleting the profile directory underneath them.
        if let Some(store) = view.session_store.as_ref() {
            store.flush();
        }
        if let Some(store) = view.settings_store.as_ref() {
            store.flush();
        }
    });
    assert!(resources.config_dir.join("workspaces-v1.json").is_file());
    remove_fixture(&root);
}

#[test]
fn remote_retry_policy_is_bounded_and_exposes_actionable_details() {
    assert_eq!(remote_retry_delay(1), REMOTE_RETRY_BASE_DELAY);
    assert_eq!(remote_retry_delay(2), REMOTE_RETRY_BASE_DELAY * 2);
    let profile = remote_profile("retry-policy");
    let error =
        ServiceError::new(ErrorCode::RemoteUnavailable, "remote is offline").retryable(true);
    let status = remote_status(&profile.id, RemoteDriveState::Error, None, Some(error));
    let waiting = RemoteRetryState {
        attempt: 1,
        max_attempts: REMOTE_CONNECT_MAX_ATTEMPTS,
        phase: RemoteRetryPhase::Waiting,
        delay: Some(REMOTE_RETRY_BASE_DELAY),
    };
    let waiting_detail = remote_profile_detail(&profile, &status, Some(&waiting));
    assert!(waiting_detail.contains("Retrying in"));
    assert!(waiting_detail.contains("attempt 1 of 3"));

    let exhausted = RemoteRetryState {
        attempt: REMOTE_CONNECT_MAX_ATTEMPTS,
        max_attempts: REMOTE_CONNECT_MAX_ATTEMPTS,
        phase: RemoteRetryPhase::Exhausted,
        delay: None,
    };
    let terminal_detail = remote_profile_detail(&profile, &status, Some(&exhausted));
    assert!(terminal_detail.contains("Gave up after 3 attempts"));
    assert!(terminal_detail.contains("Check the connection and rclone configuration"));
}

#[gpui::test]
fn remote_connect_retries_with_bounded_backoff_and_recovers(cx: &mut TestAppContext) {
    let root = fixture_dir();
    let resources = ResourcePaths::test(&root);
    let backend = Arc::new(FakeGpuiRemoteBackend::default());
    backend.state.lock().unwrap().fail_starts_remaining = 2;
    let services = NativeServices::with_remote_backend(
        resources,
        Arc::clone(&backend) as Arc<dyn RemoteDriveBackend>,
    );
    let id = Uuid::new_v4().to_string();
    let profile = remote_profile(&id);
    let (view, cx) = cx.add_window_view(|_, cx| DirectoryWindow::new(root.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.settings.remote_profiles = vec![profile];
        view.connect_remote_profile(id.clone(), cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| {
            view.remote
                .statuses
                .get(&id)
                .is_some_and(|status| status.state == RemoteDriveState::Connected)
        }) {
            break;
        }
        if let Some(delay) = view.read_with(cx, |view, _| {
            view.remote.retries.get(&id).and_then(|retry| retry.delay)
        }) {
            cx.executor().advance_clock(delay);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, _| {
        assert_eq!(backend.state.lock().unwrap().start_attempts, 3);
        assert_eq!(
            view.remote.statuses.get(&id).unwrap().state,
            RemoteDriveState::Connected
        );
        assert!(!view.remote.retries.contains_key(&id));
    });
    remove_fixture(&root);
}

#[gpui::test]
fn remote_connect_gives_up_and_scheduled_retry_can_be_cancelled(cx: &mut TestAppContext) {
    let root = fixture_dir();
    let resources = ResourcePaths::test(&root);
    let backend = Arc::new(FakeGpuiRemoteBackend::default());
    backend.state.lock().unwrap().fail_start = true;
    let services = NativeServices::with_remote_backend(
        resources,
        Arc::clone(&backend) as Arc<dyn RemoteDriveBackend>,
    );
    let terminal_id = Uuid::new_v4().to_string();
    let cancelled_id = Uuid::new_v4().to_string();
    let terminal_profile = remote_profile(&terminal_id);
    let cancelled_profile = remote_profile(&cancelled_id);
    let (view, cx) = cx.add_window_view(|_, cx| DirectoryWindow::new(root.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.settings.remote_profiles = vec![terminal_profile, cancelled_profile];
        view.connect_remote_profile(terminal_id.clone(), cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| {
            view.remote
                .retries
                .get(&terminal_id)
                .is_some_and(|retry| retry.phase == RemoteRetryPhase::Exhausted)
        }) {
            break;
        }
        if let Some(delay) = view.read_with(cx, |view, _| {
            view.remote
                .retries
                .get(&terminal_id)
                .and_then(|retry| retry.delay)
        }) {
            cx.executor().advance_clock(delay);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, cx| {
        assert_eq!(backend.state.lock().unwrap().start_attempts, 3);
        let retry = view.remote.retries.get(&terminal_id).unwrap();
        assert_eq!(retry.attempt, REMOTE_CONNECT_MAX_ATTEMPTS);
        assert_eq!(retry.phase, RemoteRetryPhase::Exhausted);
        let detail = remote_profile_detail(
            &view.settings.remote_profiles[0],
            view.remote.statuses.get(&terminal_id).unwrap(),
            Some(retry),
        );
        assert!(detail.contains("Gave up after 3 attempts"));

        view.connect_remote_profile(cancelled_id.clone(), cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| {
            view.remote
                .retries
                .get(&cancelled_id)
                .is_some_and(|retry| retry.phase == RemoteRetryPhase::Waiting)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    view.update(cx, |view, _| {
        assert_eq!(backend.state.lock().unwrap().start_attempts, 4);
        view.cancel_remote_connect_attempt(&cancelled_id);
        assert!(!view.remote.retries.contains_key(&cancelled_id));
        assert_eq!(
            view.remote.statuses.get(&cancelled_id).unwrap().state,
            RemoteDriveState::Disconnected
        );
    });
    cx.executor().advance_clock(REMOTE_RETRY_BASE_DELAY * 2);
    cx.run_until_parked();
    assert_eq!(backend.state.lock().unwrap().start_attempts, 4);
    remove_fixture(&root);
}

#[gpui::test]
fn remote_status_polling_detects_degradation_reconnects_and_stops_when_disabled(
    cx: &mut TestAppContext,
) {
    let root = fixture_dir();
    let resources = ResourcePaths::test(&root);
    let backend = Arc::new(FakeGpuiRemoteBackend::default());
    let services = NativeServices::with_remote_backend(
        resources,
        Arc::clone(&backend) as Arc<dyn RemoteDriveBackend>,
    );
    let id = Uuid::new_v4().to_string();
    let profile = remote_profile(&id);
    let (view, cx) = cx.add_window_view(|_, cx| DirectoryWindow::new(root.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.settings.behavior.remote_drives_enabled = true;
        view.settings.remote_profiles = vec![profile];
        view.connect_remote_profile(id.clone(), cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| {
            view.remote
                .statuses
                .get(&id)
                .is_some_and(|status| status.state == RemoteDriveState::Connected)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, cx| {
        assert_eq!(backend.state.lock().unwrap().start_attempts, 1);
        view.start_remote_status_polling(cx);
        let mut state = backend.state.lock().unwrap();
        state.quit_requested = true;
        state.fail_starts_remaining = 1;
    });
    cx.executor().advance_clock(REMOTE_STATUS_POLL_INTERVAL);
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| {
            view.remote
                .retries
                .get(&id)
                .is_some_and(|retry| retry.phase == RemoteRetryPhase::Waiting)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, _| {
        assert_eq!(backend.state.lock().unwrap().start_attempts, 2);
        assert!(
            view.toasts
                .current
                .as_ref()
                .is_some_and(|toast| toast.message.contains("disconnected; reconnecting"))
        );
        assert_eq!(
            view.remote.retries.get(&id).unwrap().phase,
            RemoteRetryPhase::Waiting
        );
    });
    cx.executor().advance_clock(REMOTE_RETRY_BASE_DELAY);
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| {
            view.remote
                .statuses
                .get(&id)
                .is_some_and(|status| status.state == RemoteDriveState::Connected)
                && !view.remote.retries.contains_key(&id)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, cx| {
        assert_eq!(backend.state.lock().unwrap().start_attempts, 3);
        assert_eq!(
            view.remote.statuses.get(&id).unwrap().state,
            RemoteDriveState::Connected
        );
        view.toggle_remote_drives(cx);
        assert!(view.remote.disable_pending);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| !view.remote.disable_pending) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.update(cx, |view, _| {
        assert!(!view.settings.behavior.remote_drives_enabled);
        assert!(view.remote.poll_task.is_none());
        assert!(view.remote.statuses.is_empty());
        backend.state.lock().unwrap().quit_requested = true;
    });
    cx.executor().advance_clock(REMOTE_STATUS_POLL_INTERVAL * 2);
    cx.run_until_parked();
    view.update(cx, |view, _| {
        assert_eq!(backend.state.lock().unwrap().start_attempts, 3);
        assert!(view.remote.statuses.is_empty());
    });
    remove_fixture(&root);
}

#[gpui::test]
fn native_remote_manager_connects_blocks_pending_disconnect_and_removes_cleanly(
    cx: &mut TestAppContext,
) {
    let root = fixture_dir();
    let resources = ResourcePaths::test(&root);
    let backend = Arc::new(FakeGpuiRemoteBackend::default());
    let services = NativeServices::with_remote_backend(
        resources.clone(),
        Arc::clone(&backend) as Arc<dyn RemoteDriveBackend>,
    );
    let id = Uuid::new_v4().to_string();
    let profile = remote_profile(&id);
    let (view, cx) = cx.add_window_view(|_, cx| {
        let mut view = DirectoryWindow::restore(root.clone(), false, services, cx);
        view.settings.behavior.remote_drives_enabled = true;
        view.settings.remote_profiles = vec![profile];
        view.persist_settings();
        view.start_service_events(cx);
        view.start_remote_drives(cx);
        view
    });

    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| {
            view.remote
                .statuses
                .get(&id)
                .is_some_and(|status| status.state == RemoteDriveState::Connected)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, cx| {
        assert_eq!(backend.state.lock().unwrap().started, 1);
        assert_eq!(
            view.remote.statuses.get(&id).unwrap().state,
            RemoteDriveState::Connected
        );
        view.open_remote_drive_manager(cx);
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(900.0), px(650.0)),
        |_, _| view.clone().into_element(),
    );

    backend.state.lock().unwrap().pending_uploads = 2;
    view.update(cx, |view, cx| {
        view.disconnect_remote_profile(id.clone(), false, cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| view.remote.blocked_disconnect.is_some()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, cx| {
        let blocked = view.remote.blocked_disconnect.as_ref().unwrap();
        assert_eq!(blocked.pending_uploads, 2);
        view.disconnect_remote_profile(id.clone(), true, cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| {
            view.remote
                .statuses
                .get(&id)
                .is_some_and(|status| status.state == RemoteDriveState::Disconnected)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, cx| {
        assert_eq!(backend.state.lock().unwrap().stopped, 1);
        view.request_remote_profile_delete(id.clone(), cx);
        view.confirm_remote_profile_delete(id.clone(), cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| view.settings.remote_profiles.is_empty()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, _| {
        assert!(view.settings.remote_profiles.is_empty());
        view.settings_store.as_ref().unwrap().flush();
    });
    remove_fixture(&root);
}

#[gpui::test]
fn native_remote_editor_persists_profile_and_failed_connect_can_retry(cx: &mut TestAppContext) {
    let root = fixture_dir();
    let resources = ResourcePaths::test(&root);
    let backend = Arc::new(FakeGpuiRemoteBackend::default());
    backend.state.lock().unwrap().fail_start = true;
    let services = NativeServices::with_remote_backend(
        resources.clone(),
        Arc::clone(&backend) as Arc<dyn RemoteDriveBackend>,
    );
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::restore(root.clone(), false, services, cx));

    let id = view.update(cx, |view, cx| {
        view.remote.environment = Some(RemoteDriveEnvironment {
            platform: std::env::consts::OS.to_string(),
            rclone_available: true,
            rclone_version: Some("fake-rclone 1.0".to_string()),
            winfsp_available: cfg!(windows).then_some(true),
            helper_status: cfg!(target_os = "macos").then(|| "enabled".to_string()),
            occupied_mount_targets: Vec::new(),
            error: None,
        });
        view.remote.available = vec!["cloud".to_string(), "backup".to_string()];
        view.open_remote_drive_manager(cx);
        view.open_remote_profile_editor(None, cx);
        let id = view.remote.editor.as_ref().unwrap().draft.id.clone();
        view.overlay.query = "Projects".to_string();
        view.commit_remote_editor_field(cx);
        assert_eq!(view.overlay.query, "cloud");
        view.commit_remote_editor_field(cx);
        view.overlay.query = "work".to_string();
        view.commit_remote_editor_field(cx);
        view.overlay.query = unused_remote_mount_target();
        view.commit_remote_editor_field(cx);
        assert_eq!(view.settings.remote_profiles.len(), 1);
        id
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| {
            view.remote
                .statuses
                .get(&id)
                .is_some_and(|status| status.state == RemoteDriveState::Error)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, cx| {
        let status = view.remote.statuses.get(&id).unwrap();
        assert_eq!(status.state, RemoteDriveState::Error);
        assert!(
            status.error.as_ref().unwrap().retryable,
            "unexpected remote error: {:?}",
            status.error
        );
        assert!(status.error.as_ref().unwrap().message.contains("offline"));
        backend.state.lock().unwrap().fail_start = false;
        view.connect_remote_profile(id.clone(), cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| {
            view.remote
                .statuses
                .get(&id)
                .is_some_and(|status| status.state == RemoteDriveState::Connected)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, cx| {
        assert_eq!(
            view.remote.statuses.get(&id).unwrap().state,
            RemoteDriveState::Connected
        );
        view.apply_service_event(
            ServiceEvent::RemoteDriveExitBlocked(RemoteDriveExitBlocker {
                pending_uploads: 1,
                errored_files: 0,
                error: None,
            }),
            cx,
        );
        assert_eq!(view.overlay.surface, ControlSurface::RemoteDrives);
        assert!(view.remote.exit_blocker.is_some());
        view.settings_store.as_ref().unwrap().flush();
    });
    let saved = fs::read_to_string(resources.config_dir.join("settings-v1.json")).unwrap();
    assert!(saved.contains("remoteProfiles"));
    assert!(!saved.to_ascii_lowercase().contains("password"));
    view.update(cx, |view, cx| view.force_disconnect_all_remotes(cx));
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if backend.state.lock().unwrap().stopped == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    remove_fixture(&root);
}

fn remote_test_environment(helper_status: &str) -> RemoteDriveEnvironment {
    RemoteDriveEnvironment {
        platform: std::env::consts::OS.to_string(),
        rclone_available: true,
        rclone_version: Some("fake-rclone 1.0".to_string()),
        winfsp_available: cfg!(windows).then_some(true),
        helper_status: cfg!(target_os = "macos").then(|| helper_status.to_string()),
        occupied_mount_targets: Vec::new(),
        error: None,
    }
}

/// Found on a real machine: once a click elsewhere took the focus away from
/// the profile editor's field, clicking the field again did not focus it,
/// so typing went nowhere.
#[gpui::test]
fn the_remote_editor_field_takes_focus_back_when_clicked(cx: &mut TestAppContext) {
    let root = fixture_dir();
    let backend = Arc::new(FakeGpuiRemoteBackend::default());
    let services = NativeServices::with_remote_backend(
        ResourcePaths::test(&root),
        Arc::clone(&backend) as Arc<dyn RemoteDriveBackend>,
    );
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::restore(root.clone(), false, services, cx));
    window.simulate_resize(gpui::size(px(1024.0), px(768.0)));
    view.update(window, |view, cx| {
        view.install_shortcut_bindings(cx);
        view.remote.environment = Some(remote_test_environment("enabled"));
        view.remote.available = vec!["cloud".to_string()];
        view.open_remote_drive_manager(cx);
        view.remote.environment = Some(remote_test_environment("enabled"));
        view.open_remote_profile_editor(None, cx);
    });
    window.run_until_parked();
    let input_focus = view.read_with(window, |view, _| view.text_input.focus.clone().unwrap());
    let field_focused = |window: &mut gpui::VisualTestContext| {
        window.update(|window, _| input_focus.is_focused(window))
    };
    assert!(field_focused(window), "the new editor field starts focused");

    // A click outside the field, on the status bar, takes the focus away.
    window.simulate_click(
        gpui::point(px(500.0), px(760.0)),
        gpui::Modifiers::default(),
    );
    window.run_until_parked();
    assert!(!field_focused(window));

    let field = window
        .debug_bounds("remote-profile-editor-field")
        .expect("the editor field renders");
    window.simulate_click(field.center(), gpui::Modifiers::default());
    window.run_until_parked();
    assert!(field_focused(window), "clicking the field focuses it again");
    for character in ["O", "f", "f", "i", "c", "e"] {
        window.update(|window, cx| {
            window.dispatch_keystroke(
                Keystroke::parse(character).unwrap().with_simulated_ime(),
                cx,
            );
        });
    }
    window.run_until_parked();
    view.read_with(window, |view, _| assert_eq!(view.overlay.query, "Office"));
    remove_fixture(&root);
}

#[gpui::test]
fn the_remote_manager_shows_how_to_ready_the_helper_and_hints_each_field(cx: &mut TestAppContext) {
    let root = fixture_dir();
    let backend = Arc::new(FakeGpuiRemoteBackend::default());
    let services = NativeServices::with_remote_backend(
        ResourcePaths::test(&root),
        Arc::clone(&backend) as Arc<dyn RemoteDriveBackend>,
    );
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::restore(root.clone(), false, services, cx));
    window.simulate_resize(gpui::size(px(1024.0), px(768.0)));
    view.update(window, |view, cx| {
        view.remote.available = vec!["cloud".to_string()];
        view.open_remote_drive_manager(cx);
    });
    window.run_until_parked();

    // The helper row shows whenever the macOS helper is not ready, with no
    // error needed to reveal it.
    for (helper, shown) in [
        ("not-registered", cfg!(target_os = "macos")),
        ("approval-required", cfg!(target_os = "macos")),
        ("enabled", false),
    ] {
        view.update(window, |view, cx| {
            view.remote.environment = Some(remote_test_environment(helper));
            view.remote.setup_error = None;
            cx.notify();
        });
        window.run_until_parked();
        assert_eq!(
            window.debug_bounds("remote-helper-setup").is_some(),
            shown,
            "helper {helper}"
        );
        assert_eq!(
            window.debug_bounds("install-remote-helper").is_some(),
            shown,
            "helper {helper}"
        );
    }

    // Each step of the profile editor hints at what it wants.
    view.update(window, |view, cx| view.open_remote_profile_editor(None, cx));
    window.run_until_parked();
    let placeholder = |view: &Entity<DirectoryWindow>, window: &mut gpui::VisualTestContext| {
        view.read_with(window, |view, cx| {
            view.text_input
                .entity
                .as_ref()
                .unwrap()
                .read(cx)
                .placeholder()
                .to_string()
        })
    };
    let mut seen = Vec::new();
    for value in ["Office", "cloud", "", ""] {
        seen.push(placeholder(&view, window));
        view.update(window, |view, cx| {
            if view.remote.editor.as_ref().unwrap().field.next().is_some() {
                view.overlay.query = value.to_string();
                view.commit_remote_editor_field(cx);
            }
        });
        window.run_until_parked();
    }
    assert_eq!(seen[0], "My NAS");
    assert_eq!(seen[1], "Remote name from rclone config");
    assert_eq!(seen[2], "Optional folder inside the remote");
    assert!(
        !seen.iter().any(|hint| hint == "Type to filter…"),
        "{seen:?}"
    );
    remove_fixture(&root);
}

#[gpui::test]
fn remote_manager_keybinding_opens_the_native_surface(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        cx.bind_keys([KeyBinding::new(
            "ctrl-shift-r",
            ToggleRemoteDriveManager,
            Some("browser"),
        )]);
        cx.open_window(Default::default(), |window, cx| {
            let view = cx.new(|cx| {
                DirectoryWindow::new(PathBuf::from("sample"), NativeServices::default(), cx)
            });
            window.focus(&view.focus_handle(cx), cx);
            view
        })
        .unwrap()
    });
    cx.dispatch_keystroke(*window, Keystroke::parse("ctrl-shift-r").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.overlay.surface, ControlSurface::RemoteDrives)
        })
        .unwrap();
}

#[gpui::test]
fn native_batch_rename_renders_commits_and_round_trips_undo_redo(cx: &mut TestAppContext) {
    let root = fixture_dir();
    let first = root.join("alpha.txt");
    let second = root.join("beta.txt");
    fs::write(&first, "alpha").unwrap();
    fs::write(&second, "beta").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&root));
    let (view, cx) = cx.add_window_view(|_, cx| DirectoryWindow::new(root.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.browser.replace_entries(vec![
            absolute_entry(first.clone()),
            absolute_entry(second.clone()),
        ]);
        view.browser.select(first.clone());
        view.browser.toggle_selection(second.clone());
        view.prompt_rename_selected(cx);
        assert_eq!(view.overlay.surface, ControlSurface::BatchRename);
        view.set_batch_rename_mode(BatchRenameMode::Number, cx);
        if let Some(editor) = &mut view.batch_rename {
            editor.number_digits = 1;
            editor.position = batch_rename::InsertPosition::Prefix;
        }
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(900.0), px(650.0)),
        |_, _| view.clone().into_element(),
    );
    view.update(cx, |view, cx| view.apply_batch_rename(cx));
    wait_for_view(&view, cx, "the batch rename", |view| {
        !view.mutation.in_progress
    });
    assert_eq!(
        fs::read_to_string(root.join("1_alpha.txt")).unwrap(),
        "alpha"
    );
    assert_eq!(fs::read_to_string(root.join("2_beta.txt")).unwrap(), "beta");
    view.update(cx, |view, cx| {
        assert_eq!(view.overlay.surface, ControlSurface::Closed);
        assert!(view.undo_ledger.can_undo(SystemTime::now()));
        view.undo(cx);
    });
    wait_for_view(&view, cx, "the undo", |view| {
        !view.undo_ledger.is_processing()
    });
    assert_eq!(fs::read_to_string(&first).unwrap(), "alpha");
    assert_eq!(fs::read_to_string(&second).unwrap(), "beta");
    view.update(cx, |view, cx| view.redo(cx));
    wait_for_view(&view, cx, "the redo", |view| {
        !view.undo_ledger.is_processing()
    });
    assert_eq!(
        fs::read_to_string(root.join("1_alpha.txt")).unwrap(),
        "alpha"
    );
    assert_eq!(fs::read_to_string(root.join("2_beta.txt")).unwrap(), "beta");
    remove_fixture(&root);
}

#[gpui::test]
fn f2_opens_batch_rename_for_multiple_selected_items(cx: &mut TestAppContext) {
    let first = PathBuf::from("sample/one.txt");
    let second = PathBuf::from("sample/two.txt");
    let window = cx.update(|cx| {
        cx.bind_keys([KeyBinding::new("f2", RenameSelected, Some("browser"))]);
        cx.open_window(Default::default(), |window, cx| {
            let view = cx.new(|cx| {
                let mut view =
                    DirectoryWindow::new(PathBuf::from("sample"), NativeServices::default(), cx);
                view.browser.replace_entries(vec![
                    absolute_entry(first.clone()),
                    absolute_entry(second.clone()),
                ]);
                view.browser.select(first.clone());
                view.browser.toggle_selection(second.clone());
                view
            });
            window.focus(&view.focus_handle(cx), cx);
            view
        })
        .unwrap()
    });

    cx.dispatch_keystroke(*window, Keystroke::parse("f2").unwrap());
    window
        .update(cx, |view, _, _| {
            assert_eq!(view.overlay.surface, ControlSurface::BatchRename);
            assert_eq!(view.batch_rename.as_ref().unwrap().sources.len(), 2);
        })
        .unwrap();
}

#[gpui::test]
fn active_native_mutations_block_close_and_render_wait_controls(cx: &mut TestAppContext) {
    let root = fixture_dir();
    let destination = root.join("destination");
    fs::create_dir(&destination).unwrap();
    let mut sources = Vec::new();
    for index in 0..500 {
        let path = root.join(format!("source-{index:03}.txt"));
        fs::write(&path, vec![b'x'; 4096]).unwrap();
        sources.push(path);
    }
    let services = NativeServices::new(ResourcePaths::test(&root));
    let operation_services = services.clone();
    let (view, cx) = cx.add_window_view(|_, cx| DirectoryWindow::new(root.clone(), services, cx));

    let job_id = operation_services
        .mutations
        .start_file_operation(FileOperationRequest {
            kind: FileOperationKind::Copy,
            sources,
            destination: Some(destination),
            conflict_policy: ConflictPolicy::Error,
        })
        .unwrap();
    view.update(cx, |view, cx| {
        assert!(!view.request_window_close(cx));
        assert!(view.mutation.exit_waiting);
    });
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(800.0), px(600.0)),
        |_, _| view.clone().into_element(),
    );
    view.update(cx, |view, cx| {
        view.cancel_operations_and_close(cx);
        assert!(view.mutation.exit_waiting);
        view.keep_app_open(cx);
        assert!(!view.mutation.exit_waiting);
    });
    operation_services.mutations.cancel_file_operation(&job_id);
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        if operation_services.mutations.active_count() == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(operation_services.mutations.active_count(), 0);
    remove_fixture(&root);
}

fn fixture_entry(name: &str) -> FileEntry {
    FileEntry {
        id: Uuid::new_v4(),
        path: PathBuf::from("sample").join(name),
        size: 0,
        modified: SystemTime::UNIX_EPOCH,
        hidden: false,
        is_dir: false,
        custom: std::collections::HashMap::new(),
        is_symlink: false,
        is_junction: false,
        link_target: None,
        has_xattrs: false,
        is_package: false,
        link_target_is_dir: false,
        is_cloud_placeholder: false,
        tags: Vec::new(),
    }
}

#[gpui::test]
fn trashing_a_selection_starts_one_operation(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    // Missing items fail validation before anything reaches the real Trash.
    let sources = vec![
        directory.join("first-missing"),
        directory.join("second-missing"),
    ];
    view.update(cx, |view, cx| {
        view.start_service_events(cx);
        view.start_trash_operations(sources.clone(), cx);
        let operations = view.operations.operations();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].request().kind, FileOperationKind::Trash);
        assert_eq!(operations[0].request().sources, sources);
    });
    wait_for_view(&view, cx, "the trash validation to settle", |view| {
        view.operations.active_count() == 0
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn extraction_prompt_suggests_a_free_folder_and_refuses_existing_ones(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let source = directory.join("source.txt");
    fs::write(&source, "archive proof").unwrap();
    let archive = directory.join("bundle.zip");
    explorie_core::archive::create_zip_archive(&[source], &archive, CompressionLevel::Normal)
        .unwrap();
    fs::create_dir(directory.join("bundle")).unwrap();
    fs::write(directory.join("bundle/source.txt"), "keep").unwrap();
    fs::create_dir(directory.join("bundle 2")).unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    let taken = |name: &str| format!("A folder named \u{201c}{name}\u{201d} already exists");

    view.update(cx, |view, cx| {
        view.prompt_extract_archive_path(archive.clone(), cx);
        assert_eq!(view.mutation.prompt.as_ref().unwrap().input, "bundle 3");
        view.mutation.prompt.as_mut().unwrap().input = "bundle".to_string();
        view.submit_mutation_prompt(cx);
        let prompt = view.mutation.prompt.as_ref().expect("prompt stays open");
        assert!(matches!(
            prompt.kind,
            MutationPromptKind::ExtractDirectory { .. }
        ));
        assert_eq!(prompt.error, Some(taken("bundle")));

        // A folder that appears after the name was accepted is refused too.
        view.mutation.prompt.as_mut().unwrap().input = "bundle 3".to_string();
        view.submit_mutation_prompt(cx);
        assert!(matches!(
            view.mutation.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(MutationPromptKind::ExtractPassword { .. })
        ));
        fs::create_dir(directory.join("bundle 3")).unwrap();
        view.submit_mutation_prompt(cx);
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| !view.mutation.in_progress) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    view.update(cx, |view, _| {
        let prompt = view.mutation.prompt.as_ref().expect("prompt reopened");
        assert!(matches!(
            prompt.kind,
            MutationPromptKind::ExtractDirectory { .. }
        ));
        assert_eq!(prompt.error, Some(taken("bundle 3")));
    });
    assert_eq!(fs::read_dir(directory.join("bundle 3")).unwrap().count(), 0);
    assert_eq!(
        fs::read_to_string(directory.join("bundle/source.txt")).unwrap(),
        "keep"
    );
    remove_fixture(&directory);
}

#[gpui::test]
fn submitting_an_unchanged_name_closes_the_rename_prompt(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let source = directory.join("notes.txt");
    fs::write(&source, "keep").unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));

    view.update(cx, |view, cx| {
        view.prompt_rename_path(source.clone(), cx);
        view.submit_mutation_prompt(cx);
        assert!(view.mutation.prompt.is_none());
        assert!(!view.mutation.in_progress);
        assert!(!view.undo_ledger.can_undo(SystemTime::now()));
    });
    assert_eq!(fs::read_to_string(&source).unwrap(), "keep");
    assert!(
        !directory
            .join(explorie_core::numbered_name(
                std::ffi::OsStr::new("notes.txt"),
                2,
                true
            ))
            .exists()
    );
    remove_fixture(&directory);
}

fn keymap_fixture(
    cx: &mut TestAppContext,
) -> (
    PathBuf,
    PathBuf,
    Entity<DirectoryWindow>,
    &mut gpui::VisualTestContext,
) {
    let directory = fixture_dir();
    let folder = directory.join("folder");
    fs::create_dir(&folder).unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(directory.clone(), services, cx);
        view.install_shortcut_bindings(cx);
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    let mut entry = absolute_entry(folder.clone());
    entry.is_dir = true;
    view.update(window, |view, cx| {
        view.browser.replace_entries(vec![entry]);
        view.browser.select(folder.clone());
        cx.notify();
    });
    window.run_until_parked();
    (directory, folder, view, window)
}

#[gpui::test]
fn return_submits_inline_prompts_instead_of_acting_on_the_selection(cx: &mut TestAppContext) {
    let (directory, _folder, view, window) = keymap_fixture(cx);
    view.update(window, |view, cx| view.prompt_rename_selected(cx));
    window.run_until_parked();
    window.update(|window, _| {
        assert!(
            window
                .context_stack()
                .iter()
                .any(|context| context.contains("NativeTextInput"))
        );
    });
    window.simulate_keystrokes("r e n a m e d enter");
    window.run_until_parked();
    view.update(window, |view, _| {
        assert!(view.mutation.prompt.is_none(), "Return submits the rename");
        assert_eq!(view.browser.path(), directory.as_path());
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn command_palette_owns_arrow_keys_and_return(cx: &mut TestAppContext) {
    let (directory, folder, view, window) = keymap_fixture(cx);
    view.update(window, |view, cx| {
        view.open_control_surface(ControlSurface::CommandPalette, cx)
    });
    window.run_until_parked();
    window.simulate_keystrokes("v i e w");
    window.run_until_parked();
    let expected = view.update(window, |view, _| view.visible_commands()[1].id);
    window.simulate_keystrokes("down");
    view.update(window, |view, _| {
        assert_eq!(view.overlay.selected, 1);
        assert_eq!(view.browser.selected_paths(), vec![folder.clone()]);
    });
    window.simulate_keystrokes("up down enter");
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.overlay.surface, ControlSurface::Closed);
        assert_eq!(
            view.settings.recent_commands.first().map(String::as_str),
            Some(expected.as_str())
        );
        assert_eq!(view.browser.path(), directory.as_path());
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn return_follows_the_platform_file_manager(cx: &mut TestAppContext) {
    let (directory, folder, view, window) = keymap_fixture(cx);
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    view.update(window, |view, _| {
        if cfg!(target_os = "macos") {
            // Finder renames on Return and opens with Cmd+O / Cmd+Down.
            assert!(matches!(
                view.mutation.prompt.as_ref().map(|prompt| &prompt.kind),
                Some(MutationPromptKind::Rename { .. })
            ));
            assert_eq!(view.browser.path(), directory.as_path());
        } else {
            assert_eq!(view.browser.path(), folder.as_path());
        }
    });
    remove_fixture(&directory);
}

#[cfg(target_os = "macos")]
#[gpui::test]
fn finder_open_and_navigation_chords_drive_the_browser(cx: &mut TestAppContext) {
    let (directory, folder, view, window) = keymap_fixture(cx);
    window.simulate_keystrokes("cmd-down");
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), folder.as_path())
    });
    window.simulate_keystrokes("cmd-[");
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), directory.as_path())
    });
    window.simulate_keystrokes("cmd-]");
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), folder.as_path())
    });
    window.simulate_keystrokes("cmd-up");
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), directory.as_path())
    });
    window.simulate_keystrokes("cmd-shift-g");
    window.run_until_parked();
    view.update(window, |view, cx| {
        assert!(view.navigation_ui.go_to_folder.is_some());
        view.close_go_to_folder(cx);
    });
    let hidden = view.update(window, |view, _| view.browser.show_hidden());
    window.simulate_keystrokes("cmd->");
    view.update(window, |view, _| {
        assert_ne!(view.browser.show_hidden(), hidden)
    });
    remove_fixture(&directory);
}

#[cfg(target_os = "macos")]
#[gpui::test]
fn cmd_backspace_edits_a_focused_text_field_instead_of_trashing(cx: &mut TestAppContext) {
    let (directory, folder, view, window) = keymap_fixture(cx);
    view.update(window, |view, cx| view.prompt_rename_selected(cx));
    window.run_until_parked();
    window.simulate_keystrokes("n e w cmd-backspace x");
    window.run_until_parked();
    view.update(window, |view, _| {
        let prompt = view.mutation.prompt.as_ref().unwrap();
        assert!(matches!(prompt.kind, MutationPromptKind::Rename { .. }));
        assert_eq!(prompt.input, "x");
    });
    assert!(folder.exists());
    remove_fixture(&directory);
}

#[gpui::test]
fn shortcut_recorder_captures_chords_that_are_already_bound(cx: &mut TestAppContext) {
    let (directory, _folder, view, window) = keymap_fixture(cx);
    view.update(window, |view, cx| {
        view.open_shortcut_editor("settings-open", cx)
    });
    window.run_until_parked();
    let go_to_folder = if cfg!(target_os = "macos") {
        "cmd-shift-g"
    } else {
        "ctrl-g"
    };
    window.simulate_keystrokes(go_to_folder);
    window.run_until_parked();
    view.update(window, |view, _| {
        assert!(
            view.navigation_ui.go_to_folder.is_none(),
            "recording must not run the bound command"
        );
        let editor = view.settings_ui.shortcut_editor.as_ref().unwrap();
        assert_eq!(
            editor.binding,
            if cfg!(target_os = "macos") {
                "secondary-shift-g"
            } else {
                "secondary-g"
            }
        );
        assert!(
            editor
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Go to folder"))
        );
    });
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    view.update(window, |view, _| {
        assert!(view.settings_ui.shortcut_editor.is_none())
    });
    window.simulate_keystrokes(go_to_folder);
    window.run_until_parked();
    view.update(window, |view, _| {
        assert!(view.navigation_ui.go_to_folder.is_some())
    });
    remove_fixture(&directory);
}

fn menu_fixture(
    cx: &mut TestAppContext,
    open_window: impl Fn(&mut App) -> Option<gpui::WindowHandle<DirectoryWindow>> + 'static,
) -> (
    PathBuf,
    PathBuf,
    NativeServices,
    Entity<DirectoryWindow>,
    &mut gpui::VisualTestContext,
) {
    let directory = fixture_dir();
    let folder = directory.join("folder");
    fs::create_dir(&folder).unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let menu_services = services.clone();
    cx.update(|cx| install_app_menus(cx, menu_services, open_window));
    let window_services = services.clone();
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(directory.clone(), window_services, cx);
        view.install_shortcut_bindings(cx);
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.update(|window, _| window.activate_window());
    let mut entry = absolute_entry(folder.clone());
    entry.is_dir = true;
    view.update(window, |view, cx| {
        view.browser.replace_entries(vec![entry]);
        view.browser.select(folder.clone());
        cx.notify();
    });
    window.run_until_parked();
    (directory, folder, services, view, window)
}

#[gpui::test]
fn menu_commands_run_in_the_active_window(cx: &mut TestAppContext) {
    let (directory, _folder, _services, view, window) = menu_fixture(cx, |_| None);
    window.dispatch_action(crate::app_menu::RunCommand(CommandId::GoHome));
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(Some(view.browser.path().to_path_buf()), dirs::home_dir());
    });
    #[cfg(target_os = "macos")]
    {
        // Finder's Go-menu chords reach the same command path.
        window.simulate_keystrokes("cmd-shift-d");
        window.run_until_parked();
        view.update(window, |view, _| {
            assert_eq!(Some(view.browser.path().to_path_buf()), dirs::desktop_dir());
        });
    }
    window.dispatch_action(crate::app_menu::RunCommand(CommandId::ToggleSidebar));
    window.run_until_parked();
    view.update(window, |view, _| assert!(view.layout.sidebar_collapsed));
    window.dispatch_action(crate::app_menu::RunCommand(CommandId::About));
    window.run_until_parked();
    view.update(window, |view, _| {
        assert!(view.settings_ui.panel_open);
        assert_eq!(view.settings_ui.tab, SettingsTab::About);
        // Menu commands are not command-palette history.
        assert!(view.settings.recent_commands.is_empty());
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn edit_menu_acts_on_a_focused_text_field_before_the_selection(cx: &mut TestAppContext) {
    let (directory, folder, _services, view, window) = menu_fixture(cx, |_| None);
    window.dispatch_action(crate::app_menu::MenuCopy);
    window.run_until_parked();
    view.update(window, |view, _| {
        let clipboard = view.clipboard.state.as_ref().expect("files copied");
        assert_eq!(clipboard.paths, vec![folder.clone()]);
    });

    view.update(window, |view, cx| {
        view.clipboard.state = None;
        view.prompt_rename_selected(cx);
    });
    window.run_until_parked();
    window.dispatch_action(crate::app_menu::MenuSelectAll);
    window.dispatch_action(crate::app_menu::MenuCopy);
    window.run_until_parked();
    view.update(window, |view, _| assert!(view.clipboard.state.is_none()));
    assert_eq!(
        window
            .update(|_, cx| cx.read_from_clipboard())
            .and_then(|item| item.text())
            .as_deref(),
        Some("folder")
    );
    remove_fixture(&directory);
}

#[gpui::test]
fn menu_bar_tracks_the_active_window_and_shortcut_changes(cx: &mut TestAppContext) {
    let (directory, _folder, _services, view, window) = menu_fixture(cx, |_| None);
    let state = window.update(|_, cx| crate::app_menu::current_menu_state(cx).unwrap());
    assert!(!state.show_hidden);
    assert_eq!(state.view_mode, Some(ViewMode::List));

    view.update(window, |view, cx| {
        view.toggle_hidden(cx);
        view.set_view_mode(ViewMode::Grid, cx);
    });
    window.run_until_parked();
    let state = window.update(|_, cx| crate::app_menu::current_menu_state(cx).unwrap());
    assert!(state.show_hidden);
    assert_eq!(state.view_mode, Some(ViewMode::Grid));

    view.update(window, |view, cx| {
        view.settings
            .shortcut_bindings
            .insert("nav-back".to_string(), "secondary-alt-j".to_string());
        view.install_shortcut_bindings(cx);
        cx.notify();
    });
    window.run_until_parked();
    let state = window.update(|_, cx| crate::app_menu::current_menu_state(cx).unwrap());
    assert_eq!(
        state.shortcut_overrides.get("nav-back").map(String::as_str),
        Some("secondary-alt-j")
    );
    remove_fixture(&directory);
}

#[gpui::test]
fn quit_waits_for_active_file_operations_like_closing_the_last_window(cx: &mut TestAppContext) {
    let (directory, _folder, services, view, window) = menu_fixture(cx, |_| None);
    let destination = directory.join("destination");
    fs::create_dir(&destination).unwrap();
    let sources = (0..500)
        .map(|index| {
            let path = directory.join(format!("source-{index:03}.txt"));
            fs::write(&path, vec![b'x'; 4096]).unwrap();
            path
        })
        .collect();
    let job_id = services
        .mutations
        .start_file_operation(FileOperationRequest {
            kind: FileOperationKind::Copy,
            sources,
            destination: Some(destination),
            conflict_policy: ConflictPolicy::Error,
        })
        .unwrap();
    window.dispatch_action(crate::app_menu::Quit);
    window.run_until_parked();
    view.update(window, |view, cx| {
        assert!(view.mutation.exit_waiting);
        view.keep_app_open(cx);
    });
    services.mutations.cancel_file_operation(&job_id);
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        if services.mutations.active_count() == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    remove_fixture(&directory);
}

#[gpui::test]
fn quit_without_windows_reopens_one_only_when_work_is_pending(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let reopened = Rc::new(Cell::new(0));
    let counter = reopened.clone();
    let menu_services = services.clone();
    cx.update(|cx| {
        install_app_menus(cx, menu_services, move |_| {
            counter.set(counter.get() + 1);
            None
        })
    });
    // The clean-quit check runs on a native worker thread.
    cx.executor().allow_parking();
    cx.update(|cx| cx.dispatch_action(&crate::app_menu::Quit));
    for _ in 0..20 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(reopened.get(), 0, "a clean quit needs no window");

    let destination = directory.join("destination");
    fs::create_dir(&destination).unwrap();
    let sources = (0..500)
        .map(|index| {
            let path = directory.join(format!("source-{index:03}.txt"));
            fs::write(&path, vec![b'x'; 4096]).unwrap();
            path
        })
        .collect();
    let job_id = services
        .mutations
        .start_file_operation(FileOperationRequest {
            kind: FileOperationKind::Copy,
            sources,
            destination: Some(destination),
            conflict_policy: ConflictPolicy::Error,
        })
        .unwrap();
    cx.update(|cx| cx.dispatch_action(&crate::app_menu::Quit));
    cx.run_until_parked();
    assert_eq!(
        reopened.get(),
        1,
        "pending work reopens a window to explain"
    );
    services.mutations.cancel_file_operation(&job_id);
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        if services.mutations.active_count() == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    remove_fixture(&directory);
}

#[test]
fn volume_labels_follow_the_platform_file_manager() {
    use explorie_native_services::VolumeLocation;
    let unnamed = VolumeLocation {
        path: "/Volumes/Untitled".to_string(),
        name: "  ".to_string(),
    };
    assert_eq!(volume_label(&unnamed), "/Volumes/Untitled");
    #[cfg(not(windows))]
    assert_eq!(
        volume_label(&VolumeLocation {
            path: "/".to_string(),
            name: "Macintosh HD".to_string(),
        }),
        "Macintosh HD"
    );
    #[cfg(windows)]
    assert_eq!(
        volume_label(&VolumeLocation {
            path: "C:\\".to_string(),
            name: "Local Disk".to_string(),
        }),
        "Local Disk (C:)"
    );
}
