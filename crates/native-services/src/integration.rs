#[cfg(target_os = "macos")]
use crate::process::{ProcessError, run_with_timeout};
use crate::{ActiveOperation, BlockingTask, ErrorCode, ServiceContext, ServiceError, SharedState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(any(windows, target_os = "macos"))]
use std::process::Command;
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
const MAX_METADATA_OUTPUT: usize = 1024 * 1024;
#[cfg(target_os = "macos")]
const INSTALL_CLEANUP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemIntegrationStatus {
    pub supported: bool,
    pub enabled: bool,
    /// Why folder opening cannot be routed to Explorie on this system, when
    /// the platform normally supports it.
    #[serde(default)]
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AppInfo {
    pub name: String,
    pub path: PathBuf,
    pub bundle_id: Option<String>,
    /// The application the system opens this file with by default.
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallCleanupOffer {
    pub image_path: PathBuf,
    pub mount_point: PathBuf,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Deserialize)]
struct DiskImageInfo {
    #[serde(default)]
    images: Vec<DiskImage>,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Deserialize)]
struct DiskImage {
    #[serde(rename = "image-path")]
    image_path: Option<PathBuf>,
    #[serde(rename = "system-entities", default)]
    system_entities: Vec<DiskImageEntity>,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Deserialize)]
struct DiskImageEntity {
    #[serde(rename = "mount-point")]
    mount_point: Option<PathBuf>,
}

pub trait FinderTagsBackend: Send + Sync {
    fn supported(&self) -> bool;
    fn get(&self, path: &Path) -> io::Result<Vec<String>>;
    fn set(&self, path: &Path, tags: &[String]) -> io::Result<()>;
}

pub trait PlatformActionsBackend: Send + Sync {
    fn open(&self, path: &Path) -> io::Result<()>;
    fn reveal(&self, path: &Path) -> io::Result<()>;
    fn open_with(&self, path: &Path, app_name: &str) -> io::Result<()>;
    fn apps_for_file(&self, path: &Path) -> io::Result<Vec<AppInfo>>;

    /// Ask the user to pick an application (Open With ▸ Other…). `None`
    /// means the chooser was cancelled.
    fn choose_application(&self) -> io::Result<Option<PathBuf>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Choosing an application is unavailable on this platform.",
        ))
    }
}

struct SystemFinderTagsBackend;

struct SystemPlatformActionsBackend;

impl FinderTagsBackend for SystemFinderTagsBackend {
    fn supported(&self) -> bool {
        cfg!(target_os = "macos")
    }

    fn get(&self, path: &Path) -> io::Result<Vec<String>> {
        get_finder_tags(path)
    }

    fn set(&self, path: &Path, tags: &[String]) -> io::Result<()> {
        set_finder_tags(path, tags)
    }
}

impl PlatformActionsBackend for SystemPlatformActionsBackend {
    fn open(&self, path: &Path) -> io::Result<()> {
        open::that_detached(path).map_err(io::Error::other)
    }

    fn reveal(&self, path: &Path) -> io::Result<()> {
        reveal(path)
    }

    fn open_with(&self, path: &Path, app_name: &str) -> io::Result<()> {
        open_with_app(path, app_name)
    }

    fn apps_for_file(&self, path: &Path) -> io::Result<Vec<AppInfo>> {
        apps_for_file(path)
    }

    #[cfg(target_os = "macos")]
    fn choose_application(&self) -> io::Result<Option<PathBuf>> {
        choose_application()
    }
}

#[derive(Clone)]
pub struct IntegrationService {
    context: ServiceContext,
    shared: Arc<SharedState>,
    finder_tags: Arc<dyn FinderTagsBackend>,
    platform_actions: Arc<dyn PlatformActionsBackend>,
}

impl IntegrationService {
    pub(crate) fn new(context: ServiceContext, shared: Arc<SharedState>) -> Self {
        Self::with_backends(
            context,
            shared,
            Arc::new(SystemFinderTagsBackend),
            Arc::new(SystemPlatformActionsBackend),
        )
    }

    pub(crate) fn with_finder_tags_backend(
        context: ServiceContext,
        shared: Arc<SharedState>,
        finder_tags: Arc<dyn FinderTagsBackend>,
    ) -> Self {
        Self::with_backends(
            context,
            shared,
            finder_tags,
            Arc::new(SystemPlatformActionsBackend),
        )
    }

    pub(crate) fn with_platform_actions_backend(
        context: ServiceContext,
        shared: Arc<SharedState>,
        platform_actions: Arc<dyn PlatformActionsBackend>,
    ) -> Self {
        Self::with_backends(
            context,
            shared,
            Arc::new(SystemFinderTagsBackend),
            platform_actions,
        )
    }

    pub(crate) fn with_backends(
        context: ServiceContext,
        shared: Arc<SharedState>,
        finder_tags: Arc<dyn FinderTagsBackend>,
        platform_actions: Arc<dyn PlatformActionsBackend>,
    ) -> Self {
        Self {
            context,
            shared,
            finder_tags,
            platform_actions,
        }
    }

    pub fn status(&self) -> BlockingTask<SystemIntegrationStatus> {
        self.context
            .spawn_blocking(|| system_integration_status().map_err(ServiceError::from))
    }

    pub fn set_status(&self, enabled: bool) -> BlockingTask<SystemIntegrationStatus> {
        let guard = ActiveOperation::new(Arc::clone(&self.shared));
        self.context.spawn_blocking(move || {
            let _guard = guard;
            set_system_integration(enabled).map_err(ServiceError::from)
        })
    }

    pub fn open(&self, path: PathBuf) -> BlockingTask<()> {
        let backend = Arc::clone(&self.platform_actions);
        self.context.spawn_blocking(move || {
            backend
                .open(&path)
                .map_err(|error| ServiceError::from(error).operation("open"))
        })
    }

    pub fn reveal(&self, path: PathBuf) -> BlockingTask<()> {
        let backend = Arc::clone(&self.platform_actions);
        self.context.spawn_blocking(move || {
            backend
                .reveal(&path)
                .map_err(|error| ServiceError::from(error).operation("reveal"))
        })
    }

    pub fn finder_tags(&self, path: PathBuf) -> BlockingTask<Vec<String>> {
        let backend = Arc::clone(&self.finder_tags);
        self.context
            .spawn_blocking(move || backend.get(&path).map_err(ServiceError::from))
    }

    pub fn set_finder_tags(&self, path: PathBuf, tags: Vec<String>) -> BlockingTask<()> {
        let shared = Arc::clone(&self.shared);
        let backend = Arc::clone(&self.finder_tags);
        let guard = ActiveOperation::new(Arc::clone(&self.shared));
        self.context.spawn_blocking(move || {
            if remote_root(&shared, &path) {
                return Err(ServiceError::new(
                    ErrorCode::RemoteUnavailable,
                    "Refusing to mutate a managed remote-drive root",
                ));
            }
            let _guard = guard;
            backend.set(&path, &tags).map_err(ServiceError::from)
        })
    }

    pub fn finder_tags_supported(&self) -> bool {
        self.finder_tags.supported()
    }

    pub fn finder_tag_colors(&self) -> BlockingTask<HashMap<String, u8>> {
        self.context.spawn_blocking(|| Ok(finder_tag_colors()))
    }

    pub fn open_with(&self, path: PathBuf, app_name: String) -> BlockingTask<()> {
        let backend = Arc::clone(&self.platform_actions);
        self.context.spawn_blocking(move || {
            backend
                .open_with(&path, &app_name)
                .map_err(|error| ServiceError::from(error).operation("open_with"))
        })
    }

    /// Let the user pick an application in a system file chooser that
    /// starts in /Applications and only accepts applications. Resolves to
    /// `None` when the chooser is cancelled.
    pub fn choose_application(&self) -> BlockingTask<Option<PathBuf>> {
        let backend = Arc::clone(&self.platform_actions);
        self.context.spawn_blocking(move || {
            backend
                .choose_application()
                .map_err(|error| ServiceError::from(error).operation("choose_application"))
        })
    }

    pub fn apps_for_file(&self, path: PathBuf) -> BlockingTask<Vec<AppInfo>> {
        let backend = Arc::clone(&self.platform_actions);
        self.context.spawn_blocking(move || {
            backend
                .apps_for_file(&path)
                .map_err(|error| ServiceError::from(error).operation("apps_for_file"))
        })
    }

    pub fn install_cleanup_offer(&self) -> BlockingTask<Option<InstallCleanupOffer>> {
        let resources = self.context.resources().clone();
        self.context.spawn_blocking(move || {
            find_install_cleanup_offer(&resources).map_err(ServiceError::from)
        })
    }

    pub fn cleanup_install_media(&self, offer: InstallCleanupOffer) -> BlockingTask<()> {
        let resources = self.context.resources().clone();
        let guard = ActiveOperation::new(Arc::clone(&self.shared));
        self.context.spawn_blocking(move || {
            let _guard = guard;
            cleanup_install_media(&resources, &offer).map_err(ServiceError::from)
        })
    }

    pub fn home_dir(&self) -> BlockingTask<PathBuf> {
        self.context.spawn_blocking(|| {
            dirs::home_dir().ok_or_else(|| {
                ServiceError::new(ErrorCode::NotFound, "Could not determine home directory")
            })
        })
    }

    pub fn platform(&self) -> &'static str {
        std::env::consts::OS
    }

    pub fn app_version(&self) -> &str {
        &self.context.resources().app_version
    }
}

fn remote_root(shared: &SharedState, path: &Path) -> bool {
    shared
        .remote_roots
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&crate::listing::normalize_path(path))
}

#[cfg(any(target_os = "macos", test))]
fn parse_disk_image_info(json: &[u8]) -> io::Result<Vec<InstallCleanupOffer>> {
    let info: DiskImageInfo = serde_json::from_slice(json).map_err(io::Error::other)?;
    Ok(info
        .images
        .into_iter()
        .filter_map(|image| {
            let image_path = image.image_path?;
            image.system_entities.into_iter().find_map(|entity| {
                entity.mount_point.map(|mount_point| InstallCleanupOffer {
                    image_path: image_path.clone(),
                    mount_point,
                })
            })
        })
        .collect())
}

#[cfg(target_os = "macos")]
fn find_install_cleanup_offer(
    resources: &crate::ResourcePaths,
) -> io::Result<Option<InstallCleanupOffer>> {
    let Some(current_exe) = resources.current_exe.as_deref() else {
        return Ok(None);
    };
    let installed_roots = [
        PathBuf::from("/Applications"),
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join("Applications"),
    ];
    let Some(app_bundle) = current_exe
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
    else {
        return Ok(None);
    };
    if !installed_roots
        .iter()
        .any(|root| app_bundle.starts_with(root))
    {
        return Ok(None);
    }

    let output = run_with_timeout(
        Command::new("/usr/bin/hdiutil").args(["info", "-plist"]),
        INSTALL_CLEANUP_TIMEOUT,
        MAX_METADATA_OUTPUT,
        64 * 1024,
    )
    .map_err(install_cleanup_process_error)?;
    if !output.status.success() || output.stdout_truncated {
        return Ok(None);
    }
    let temp = std::env::temp_dir().join(format!(
        "explorie-install-media-{}-{}.plist",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::write(&temp, output.stdout)?;
    let converted = run_with_timeout(
        Command::new("/usr/bin/plutil")
            .args(["-convert", "json", "-o", "-"])
            .arg(&temp),
        INSTALL_CLEANUP_TIMEOUT,
        MAX_METADATA_OUTPUT,
        64 * 1024,
    );
    let _ = fs::remove_file(&temp);
    let converted = converted.map_err(install_cleanup_process_error)?;
    if !converted.status.success() || converted.stdout_truncated {
        return Ok(None);
    }

    for offer in parse_disk_image_info(&converted.stdout)? {
        if !offer.mount_point.starts_with("/Volumes")
            || offer
                .image_path
                .extension()
                .is_none_or(|extension| extension != "dmg")
            || !offer.image_path.is_file()
            || current_exe.starts_with(&offer.mount_point)
        {
            continue;
        }
        let Some(name) = offer.image_path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.to_ascii_lowercase().starts_with("explorie") {
            continue;
        }
        let info_plist = offer.mount_point.join("explorie.app/Contents/Info.plist");
        let Ok(info_plist) = fs::read_to_string(info_plist) else {
            continue;
        };
        let version_entry = format!(
            "<key>CFBundleShortVersionString</key><string>{}</string>",
            resources.app_version
        );
        if info_plist.contains("<string>com.omershatz.explorie</string>")
            && info_plist.contains(&version_entry)
        {
            return Ok(Some(offer));
        }
    }
    Ok(None)
}

#[cfg(not(target_os = "macos"))]
fn find_install_cleanup_offer(
    _resources: &crate::ResourcePaths,
) -> io::Result<Option<InstallCleanupOffer>> {
    Ok(None)
}

#[cfg(target_os = "macos")]
fn cleanup_install_media(
    resources: &crate::ResourcePaths,
    offer: &InstallCleanupOffer,
) -> io::Result<()> {
    let Some(current) = find_install_cleanup_offer(resources)? else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "The Explorie installer image is no longer mounted",
        ));
    };
    if &current != offer {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The mounted installer changed before cleanup",
        ));
    }
    let detached = run_with_timeout(
        Command::new("/usr/bin/hdiutil")
            .arg("detach")
            .arg(&offer.mount_point),
        INSTALL_CLEANUP_TIMEOUT,
        64 * 1024,
        64 * 1024,
    )
    .map_err(install_cleanup_process_error)?;
    if !detached.status.success() {
        let detail = String::from_utf8_lossy(&detached.stderr);
        return Err(io::Error::other(format!(
            "Unable to eject the installer image: {}",
            detail.trim()
        )));
    }
    move_install_image_to_trash(&offer.image_path)
}

#[cfg(not(target_os = "macos"))]
fn cleanup_install_media(
    _resources: &crate::ResourcePaths,
    _offer: &InstallCleanupOffer,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Installer image cleanup is available only on macOS",
    ))
}

#[cfg(target_os = "macos")]
fn install_cleanup_process_error(error: ProcessError) -> io::Error {
    match error {
        ProcessError::Io(error) => error,
        ProcessError::TimedOut => io::Error::new(
            io::ErrorKind::TimedOut,
            "Installer cleanup helper timed out",
        ),
    }
}

#[cfg(target_os = "macos")]
fn move_install_image_to_trash(path: &Path) -> io::Result<()> {
    use std::ffi::{CStr, CString};
    use std::os::raw::c_char;
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" {
        fn explorie_move_install_image_to_trash(path: *const c_char) -> *mut c_char;
        fn explorie_install_cleanup_free(value: *mut c_char);
    }

    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid installer path"))?;
    // SAFETY: The bridge copies the NUL-terminated path synchronously and returns
    // either null or a heap string released by the paired free function.
    let error = unsafe { explorie_move_install_image_to_trash(path.as_ptr()) };
    if error.is_null() {
        return Ok(());
    }
    // SAFETY: A non-null bridge result is a valid NUL-terminated allocation.
    let message = unsafe { CStr::from_ptr(error) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: The pointer came from the bridge and has not been released yet.
    unsafe { explorie_install_cleanup_free(error) };
    Err(io::Error::other(message))
}

pub fn system_integration_status() -> io::Result<SystemIntegrationStatus> {
    #[cfg(windows)]
    {
        windows_integration::enabled().map(|enabled| SystemIntegrationStatus {
            supported: true,
            enabled,
            unavailable_reason: None,
        })
    }
    #[cfg(target_os = "macos")]
    {
        let enabled = macos_folder_integration::enabled();
        // Still report the setting when it is on, so it can be turned off.
        if !enabled && !macos_folder_integration::available() {
            return Ok(SystemIntegrationStatus {
                supported: false,
                enabled: false,
                unavailable_reason: Some(macos_folder_integration::UNAVAILABLE.to_string()),
            });
        }
        Ok(SystemIntegrationStatus {
            supported: true,
            enabled,
            unavailable_reason: None,
        })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    Ok(SystemIntegrationStatus {
        supported: false,
        enabled: false,
        unavailable_reason: None,
    })
}

pub fn set_system_integration(enabled: bool) -> io::Result<SystemIntegrationStatus> {
    #[cfg(windows)]
    {
        windows_integration::set_enabled(enabled)?;
        system_integration_status()
    }
    #[cfg(target_os = "macos")]
    {
        macos_folder_integration::set_enabled(enabled)?;
        system_integration_status()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = enabled;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "System integration is currently available only on Windows and macOS.",
        ))
    }
}

#[cfg(target_os = "macos")]
fn reveal(path: &Path) -> io::Result<()> {
    Command::new("open")
        .args(["-R"])
        .arg(path)
        .spawn()
        .map(|_| ())
}

#[cfg(windows)]
fn reveal(path: &Path) -> io::Result<()> {
    Command::new("explorer")
        .args(["/select,"])
        .arg(path)
        .spawn()
        .map(|_| ())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn reveal(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Reveal in file manager is not supported on this platform.",
    ))
}

/// Finder tags as raw `"Name\n<colorIndex>"` strings; the encoding lives in
/// `explorie_core`, which listings share.
fn get_finder_tags(path: &Path) -> io::Result<Vec<String>> {
    explorie_core::read_finder_tags(path)
}

fn set_finder_tags(path: &Path, tags: &[String]) -> io::Result<()> {
    explorie_core::write_finder_tags(path, tags)
}

fn finder_tag_colors() -> HashMap<String, u8> {
    [
        ("None", 0),
        ("Gray", 1),
        ("Green", 2),
        ("Purple", 3),
        ("Blue", 4),
        ("Yellow", 5),
        ("Red", 6),
        ("Orange", 7),
    ]
    .into_iter()
    .map(|(name, color)| (name.to_string(), color))
    .collect()
}

#[cfg(target_os = "macos")]
fn open_with_app(path: &Path, app_name: &str) -> io::Result<()> {
    Command::new("open")
        .args(["-a", app_name])
        .arg(path)
        .spawn()
        .map(|_| ())
}

#[cfg(windows)]
fn open_with_app(path: &Path, _app_name: &str) -> io::Result<()> {
    Command::new("rundll32.exe")
        .args(["shell32.dll,OpenAs_RunDLL"])
        .arg(path)
        .spawn()
        .map(|_| ())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn open_with_app(_path: &Path, _app_name: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Open With is unavailable on this platform.",
    ))
}

/// One application as reported by LaunchServices.
#[cfg(any(target_os = "macos", test))]
#[derive(Deserialize)]
struct LaunchServicesApp {
    name: String,
    path: PathBuf,
    bundle_id: Option<String>,
    #[serde(default)]
    is_default: bool,
}

/// Finder's Open With order: the default handler first (marked), then the
/// other applications alphabetically. Each name is offered once (the first,
/// preferred copy wins) so the menu has no look-alike entries; Open With
/// launches the listed copy by its path.
#[cfg(any(target_os = "macos", test))]
fn open_with_menu(apps: Vec<LaunchServicesApp>) -> Vec<AppInfo> {
    use std::collections::HashSet;

    let (defaults, mut others): (Vec<_>, Vec<_>) = apps.into_iter().partition(|app| app.is_default);
    others.sort_by_cached_key(|app| app.name.to_lowercase());
    let mut seen = HashSet::new();
    defaults
        .into_iter()
        .chain(others)
        .filter(|app| !app.name.is_empty() && seen.insert(app.name.to_lowercase()))
        .map(|app| AppInfo {
            name: app.name,
            path: app.path,
            bundle_id: app.bundle_id,
            is_default: app.is_default,
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn apps_for_file(path: &Path) -> io::Result<Vec<AppInfo>> {
    use std::ffi::{CStr, CString};
    use std::os::raw::c_char;
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" {
        fn explorie_apps_for_file(path: *const c_char) -> *mut c_char;
        fn explorie_open_with_free(value: *mut c_char);
    }

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid path"))?;
    // SAFETY: The bridge reads the NUL-terminated path synchronously and
    // returns null or a heap string released by the paired free function.
    let json = unsafe { explorie_apps_for_file(c_path.as_ptr()) };
    if json.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: A non-null bridge result is a valid NUL-terminated allocation.
    let bytes = unsafe { CStr::from_ptr(json) }.to_bytes().to_vec();
    // SAFETY: The pointer came from the bridge and has not been released yet.
    unsafe { explorie_open_with_free(json) };
    let apps: Vec<LaunchServicesApp> = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(open_with_menu(apps))
}

/// Show the system application chooser and wait for the user's answer.
/// The panel runs on the main thread (the bridge dispatches it there), so
/// this must be called from a worker thread while the app's run loop runs.
#[cfg(target_os = "macos")]
fn choose_application() -> io::Result<Option<PathBuf>> {
    use std::ffi::{CStr, OsStr, c_void};
    use std::os::raw::c_char;
    use std::os::unix::ffi::OsStrExt;
    use std::sync::mpsc;

    type Chosen = mpsc::Sender<Option<PathBuf>>;

    unsafe extern "C" {
        fn explorie_choose_application(
            callback: extern "C" fn(*mut c_void, *const c_char),
            context: *mut c_void,
        );
    }

    extern "C" fn chosen(context: *mut c_void, path: *const c_char) {
        // SAFETY: `context` is the boxed sender leaked below; the bridge
        // calls back exactly once.
        let sender = unsafe { Box::from_raw(context.cast::<Chosen>()) };
        let path = (!path.is_null()).then(|| {
            // SAFETY: A non-null path is a NUL-terminated string that the
            // bridge keeps alive for the duration of the callback.
            let bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
            PathBuf::from(OsStr::from_bytes(bytes))
        });
        let _ = sender.send(path);
    }

    let (sender, receiver) = mpsc::channel();
    let context = Box::into_raw(Box::new(sender)).cast::<c_void>();
    // SAFETY: The callback matches the bridge's signature and takes
    // ownership of `context` when it runs.
    unsafe { explorie_choose_application(chosen, context) };
    receiver
        .recv()
        .map_err(|_| io::Error::other("The application chooser closed unexpectedly"))
}

#[cfg(windows)]
fn apps_for_file(_path: &Path) -> io::Result<Vec<AppInfo>> {
    Ok(vec![AppInfo {
        name: "Choose another app…".to_string(),
        path: PathBuf::new(),
        bundle_id: None,
        is_default: false,
    }])
}

#[cfg(not(any(windows, target_os = "macos")))]
fn apps_for_file(_path: &Path) -> io::Result<Vec<AppInfo>> {
    Ok(Vec::new())
}

#[cfg(target_os = "macos")]
mod macos_folder_integration {
    use super::*;

    unsafe extern "C" {
        fn explorie_folder_integration_available() -> i32;
        fn explorie_folder_integration_enabled() -> i32;
        fn explorie_folder_integration_set(enabled: i32) -> i32;
    }

    pub const UNAVAILABLE: &str = "This version of macOS keeps Finder as the app that opens folders, \
         and no other app can take that over. To open a folder in Explorie, choose \
         Open With \u{25b8} explorie in Finder.";

    /// LaunchServices' paramErr: macOS refused to change the folder handler.
    const PARAM_ERR: i32 = -50;

    /// Whether this macOS version lets an app take over folder opens.
    pub fn available() -> bool {
        // SAFETY: The Objective-C bridge has no arguments and returns a plain
        // integer from the operating-system version.
        unsafe { explorie_folder_integration_available() != 0 }
    }

    pub fn enabled() -> bool {
        // SAFETY: The Objective-C bridge has no arguments and returns a plain
        // integer after querying LaunchServices.
        unsafe { explorie_folder_integration_enabled() != 0 }
    }

    pub fn set_enabled(enabled: bool) -> io::Result<()> {
        // SAFETY: The Objective-C bridge accepts a 0/1 integer and returns an
        // OSStatus without retaining Rust-owned memory.
        let status = unsafe { explorie_folder_integration_set(i32::from(enabled)) };
        if status == 0 {
            Ok(())
        } else if status == PARAM_ERR {
            Err(io::Error::new(io::ErrorKind::Unsupported, UNAVAILABLE))
        } else {
            Err(io::Error::other(format!(
                "LaunchServices rejected the folder-handler change (OSStatus {status})"
            )))
        }
    }
}

#[cfg(windows)]
mod windows_integration {
    use super::*;
    use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    const VERB: &str = "Explorie";
    const BACKUP_KEY: &str = r"Software\Explorie\FolderOpenHandler";
    const SHELL_KEYS: [(&str, &str, &str); 2] = [
        (r"Software\Classes\Directory\shell", "Directory", "%1"),
        (r"Software\Classes\Drive\shell", "Drive", "%1"),
    ];
    const BACKGROUND_SHELL_KEY: &str = r"Software\Classes\Directory\Background\shell";

    fn read_default(key: &RegKey) -> io::Result<Option<String>> {
        match key.get_value::<String, _>("") {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn delete_tree_if_present(root: &RegKey, path: &str) -> io::Result<()> {
        match root.delete_subkey_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn command_for(executable: &Path, path_argument: &str) -> String {
        format!(r#""{}" "{}""#, executable.display(), path_argument)
    }

    fn register_verb(root: &RegKey, shell_key: &str, path_argument: &str) -> io::Result<()> {
        let executable = std::env::current_exe()?;
        let (verb, _) = root.create_subkey(format!(r"{shell_key}\{VERB}"))?;
        verb.set_value("", &"Open in Explorie")?;
        verb.set_value("Icon", &executable.to_string_lossy().as_ref())?;
        let (command, _) = verb.create_subkey("command")?;
        command.set_value("", &command_for(&executable, path_argument))
    }

    fn backup_defaults(root: &RegKey) -> io::Result<()> {
        let (backup, _) = root.create_subkey(BACKUP_KEY)?;
        for (shell_key, backup_name, _) in SHELL_KEYS {
            let (shell, _) = root.create_subkey(shell_key)?;
            let current = read_default(&shell)?;
            let has_backup = backup
                .get_value::<u32, _>(format!("{backup_name}Present"))
                .is_ok();
            if current.as_deref() == Some(VERB) {
                if has_backup {
                    continue;
                }
                backup.set_value(format!("{backup_name}Present"), &0_u32)?;
                match backup.delete_value(format!("{backup_name}Value")) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                continue;
            }
            match current {
                Some(value) => {
                    backup.set_value(format!("{backup_name}Present"), &1_u32)?;
                    backup.set_value(format!("{backup_name}Value"), &value)?;
                }
                None => {
                    backup.set_value(format!("{backup_name}Present"), &0_u32)?;
                    match backup.delete_value(format!("{backup_name}Value")) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        backup.set_value("Version", &1_u32)
    }

    fn restore_defaults(root: &RegKey) -> io::Result<()> {
        let backup = match root.open_subkey(BACKUP_KEY) {
            Ok(backup) => Some(backup),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let mut first_error = None;
        for (shell_key, backup_name, _) in SHELL_KEYS {
            let result = (|| {
                let (shell, _) = root.create_subkey(shell_key)?;
                if read_default(&shell)?.as_deref() != Some(VERB) {
                    return Ok(());
                }
                let present = backup.as_ref().and_then(|key| {
                    key.get_value::<u32, _>(format!("{backup_name}Present"))
                        .ok()
                }) == Some(1);
                if present
                    && let Some(value) = backup.as_ref().and_then(|key| {
                        key.get_value::<String, _>(format!("{backup_name}Value"))
                            .ok()
                    })
                {
                    return shell.set_value("", &value);
                }
                match shell.delete_value("") {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error),
                }
            })();
            if first_error.is_none()
                && let Err(error) = result
            {
                first_error = Some(error);
            }
        }
        if let Some(error) = first_error {
            Err(error)
        } else {
            delete_tree_if_present(root, BACKUP_KEY)
        }
    }

    fn notify_shell() {
        // SAFETY: SHChangeNotify accepts null item pointers for the global
        // association-change event when SHCNF_IDLIST is used.
        unsafe {
            SHChangeNotify(
                SHCNE_ASSOCCHANGED as i32,
                SHCNF_IDLIST,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
    }

    fn remove(root: &RegKey) -> io::Result<()> {
        let mut first_error = restore_defaults(root).err();
        for (shell_key, _, _) in SHELL_KEYS {
            if let Err(error) = delete_tree_if_present(root, &format!(r"{shell_key}\{VERB}"))
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        if let Err(error) = delete_tree_if_present(root, &format!(r"{BACKGROUND_SHELL_KEY}\{VERB}"))
            && first_error.is_none()
        {
            first_error = Some(error);
        }
        notify_shell();
        first_error.map_or(Ok(()), Err)
    }

    pub fn enabled() -> io::Result<bool> {
        let root = RegKey::predef(HKEY_CURRENT_USER);
        let executable = std::env::current_exe()?;
        for (shell_key, _, path_argument) in SHELL_KEYS {
            let shell = match root.open_subkey(shell_key) {
                Ok(shell) => shell,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error),
            };
            if read_default(&shell)?.as_deref() != Some(VERB) {
                return Ok(false);
            }
            let command = match root.open_subkey(format!(r"{shell_key}\{VERB}\command")) {
                Ok(command) => command,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error),
            };
            if read_default(&command)?.as_deref()
                != Some(command_for(&executable, path_argument).as_str())
            {
                return Ok(false);
            }
        }
        let background_command =
            match root.open_subkey(format!(r"{BACKGROUND_SHELL_KEY}\{VERB}\command")) {
                Ok(command) => command,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error),
            };
        if read_default(&background_command)?.as_deref()
            != Some(command_for(&executable, "%V").as_str())
        {
            return Ok(false);
        }
        Ok(true)
    }

    pub fn set_enabled(enabled: bool) -> io::Result<()> {
        let root = RegKey::predef(HKEY_CURRENT_USER);
        if !enabled {
            return remove(&root);
        }
        if self::enabled()? {
            return Ok(());
        }
        // Refresh each shell key that no longer points at Explorie while
        // retaining the previous snapshot for keys from a partial install.
        backup_defaults(&root)?;
        let result = (|| {
            for (shell_key, _, path_argument) in SHELL_KEYS {
                register_verb(&root, shell_key, path_argument)?;
                let (shell, _) = root.create_subkey(shell_key)?;
                shell.set_value("", &VERB)?;
            }
            register_verb(&root, BACKGROUND_SHELL_KEY, "%V")
        })();
        if let Err(error) = result {
            return match remove(&root) {
                Ok(()) => Err(error),
                Err(rollback) => Err(io::Error::new(
                    error.kind(),
                    format!(
                        "folder integration setup failed ({error}); rollback also failed ({rollback})"
                    ),
                )),
            };
        }
        notify_shell();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NativeServices, ResourcePaths};
    #[cfg(any(windows, target_os = "macos"))]
    use std::fs;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeFinderTagsBackend(Mutex<Vec<String>>);

    #[derive(Default)]
    struct FakePlatformActionsBackend {
        calls: Mutex<Vec<String>>,
        fail_next: Mutex<Option<&'static str>>,
    }

    impl FakePlatformActionsBackend {
        fn record(&self, action: &'static str, path: &Path) -> io::Result<()> {
            if self.fail_next.lock().unwrap().take() == Some(action) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("fake {action} denied"),
                ));
            }
            self.calls
                .lock()
                .unwrap()
                .push(format!("{action}:{}", path.display()));
            Ok(())
        }
    }

    impl FinderTagsBackend for FakeFinderTagsBackend {
        fn supported(&self) -> bool {
            true
        }

        fn get(&self, _path: &Path) -> io::Result<Vec<String>> {
            Ok(self.0.lock().unwrap().clone())
        }

        fn set(&self, _path: &Path, tags: &[String]) -> io::Result<()> {
            *self.0.lock().unwrap() = tags.to_vec();
            Ok(())
        }
    }

    impl PlatformActionsBackend for FakePlatformActionsBackend {
        fn open(&self, path: &Path) -> io::Result<()> {
            self.record("open", path)
        }

        fn reveal(&self, path: &Path) -> io::Result<()> {
            self.record("reveal", path)
        }

        fn open_with(&self, path: &Path, app_name: &str) -> io::Result<()> {
            self.record("open_with", path)?;
            self.calls.lock().unwrap().push(format!("app:{app_name}"));
            Ok(())
        }

        fn apps_for_file(&self, path: &Path) -> io::Result<Vec<AppInfo>> {
            self.record("apps_for_file", path)?;
            Ok(vec![AppInfo {
                name: "Fixture App".to_string(),
                path: PathBuf::from("/fixture/app"),
                bundle_id: Some("test.fixture.app".to_string()),
                is_default: true,
            }])
        }
    }

    #[test]
    fn application_choices_cross_the_async_service_boundary() {
        struct Chooser(Option<PathBuf>);

        impl PlatformActionsBackend for Chooser {
            fn open(&self, _path: &Path) -> io::Result<()> {
                Ok(())
            }

            fn reveal(&self, _path: &Path) -> io::Result<()> {
                Ok(())
            }

            fn open_with(&self, _path: &Path, _app_name: &str) -> io::Result<()> {
                Ok(())
            }

            fn apps_for_file(&self, _path: &Path) -> io::Result<Vec<AppInfo>> {
                Ok(Vec::new())
            }

            fn choose_application(&self) -> io::Result<Option<PathBuf>> {
                Ok(self.0.clone())
            }
        }

        let root = tempfile::tempdir().unwrap();
        let chosen = PathBuf::from("/Applications/Chosen.app");
        let native = NativeServices::with_platform_actions_backend(
            ResourcePaths::test(root.path()),
            Arc::new(Chooser(Some(chosen.clone()))),
        );
        assert_eq!(
            native.integration.choose_application().wait().unwrap(),
            Some(chosen)
        );
        let cancelled = NativeServices::with_platform_actions_backend(
            ResourcePaths::test(root.path()),
            Arc::new(Chooser(None)),
        );
        assert_eq!(
            cancelled.integration.choose_application().wait().unwrap(),
            None
        );

        // Backends that cannot choose report it instead of hanging.
        let native = NativeServices::with_platform_actions_backend(
            ResourcePaths::test(root.path()),
            Arc::new(FakePlatformActionsBackend::default()),
        );
        assert_eq!(
            native
                .integration
                .choose_application()
                .wait()
                .unwrap_err()
                .code,
            ErrorCode::Unsupported
        );
    }

    #[test]
    fn finder_tag_colors_are_stable_on_all_platforms() {
        let colors = finder_tag_colors();
        assert_eq!(colors.get("None"), Some(&0));
        assert_eq!(colors.get("Orange"), Some(&7));
    }

    #[test]
    fn disk_image_info_keeps_each_backing_image_with_its_mount_point() {
        let offers = parse_disk_image_info(
            br#"{
                "images": [
                    {
                        "image-path": "/Users/fixture/Downloads/explorie-0.2.13-macos-arm64.dmg",
                        "system-entities": [
                            {"dev-entry": "/dev/disk4"},
                            {"mount-point": "/Volumes/explorie"}
                        ]
                    }
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(
            offers,
            vec![InstallCleanupOffer {
                image_path: PathBuf::from(
                    "/Users/fixture/Downloads/explorie-0.2.13-macos-arm64.dmg"
                ),
                mount_point: PathBuf::from("/Volumes/explorie"),
            }]
        );
    }

    #[test]
    fn platform_and_home_are_exposed_by_native_adapter() {
        let root = tempfile::tempdir().unwrap();
        let native = NativeServices::new(ResourcePaths::test(root.path()));
        let service = native.integration.clone();
        assert!(!service.platform().is_empty());
        assert!(service.app_version().is_empty());
        let _ = NativeServices::new(ResourcePaths::test(root.path()));
    }

    #[test]
    fn injected_finder_tags_backend_crosses_the_async_service_boundary() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(FakeFinderTagsBackend::default());
        backend.0.lock().unwrap().push("Important\n6".to_string());
        let native = NativeServices::with_finder_tags_backend(
            ResourcePaths::test(root.path()),
            backend.clone(),
        );
        assert!(native.integration.finder_tags_supported());
        assert_eq!(
            native
                .integration
                .finder_tags(root.path().join("fixture.txt"))
                .wait()
                .unwrap(),
            vec!["Important\n6".to_string()]
        );
        native
            .integration
            .set_finder_tags(
                root.path().join("fixture.txt"),
                vec!["Review\n4".to_string()],
            )
            .wait()
            .unwrap();
        assert_eq!(*backend.0.lock().unwrap(), vec!["Review\n4".to_string()]);
    }

    #[test]
    fn injected_platform_actions_cross_the_async_service_boundary_and_preserve_errors() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.txt");
        let backend = Arc::new(FakePlatformActionsBackend::default());
        let native = NativeServices::with_platform_actions_backend(
            ResourcePaths::test(root.path()),
            backend.clone(),
        );

        native.integration.open(path.clone()).wait().unwrap();
        native.integration.reveal(path.clone()).wait().unwrap();
        native
            .integration
            .open_with(path.clone(), "Fixture App".to_string())
            .wait()
            .unwrap();
        let apps = native
            .integration
            .apps_for_file(path.clone())
            .wait()
            .unwrap();
        assert_eq!(apps[0].bundle_id.as_deref(), Some("test.fixture.app"));
        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls.len(), 5);
        assert!(calls[0].starts_with("open:"));
        assert!(calls[1].starts_with("reveal:"));
        assert!(calls[2].starts_with("open_with:"));
        assert_eq!(calls[3], "app:Fixture App");
        assert!(calls[4].starts_with("apps_for_file:"));
        drop(calls);

        *backend.fail_next.lock().unwrap() = Some("reveal");
        let error = native.integration.reveal(path).wait().unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied);
        assert_eq!(error.operation.as_deref(), Some("reveal"));
        assert!(error.message.contains("fake reveal denied"));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires an interactive Windows shell; run explicitly during real-machine release QA"]
    fn windows_system_open_produces_a_real_shell_side_effect() {
        use std::thread;
        use std::time::{Duration, Instant};

        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("opened.txt");
        let script = root.path().join("open-side-effect.cmd");
        fs::write(
            &script,
            format!(
                "@echo off\r\n>\"{}\" echo opened-by-shell\r\n",
                marker.display()
            ),
        )
        .unwrap();
        let native = NativeServices::new(ResourcePaths::test(root.path()));
        native.integration.open(script).wait().unwrap();

        let deadline = Instant::now() + Duration::from_secs(20);
        let contents = loop {
            match fs::read_to_string(&marker) {
                Ok(contents) => break contents,
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => panic!(
                    "system open returned but the shell marker was not readable within 20 seconds: {error}"
                ),
            }
        };
        assert_eq!(contents.trim(), "opened-by-shell");
    }

    #[test]
    fn open_with_menu_puts_the_default_first_then_sorts_and_dedupes_by_name() {
        let app = |name: &str, path: &str, is_default: bool| LaunchServicesApp {
            name: name.to_string(),
            path: PathBuf::from(path),
            bundle_id: Some(format!("test.{name}")),
            is_default,
        };
        let menu = open_with_menu(vec![
            app("Zed", "/Applications/Zed.app", false),
            app("BBEdit", "/Applications/BBEdit.app", false),
            app("TextEdit", "/System/Applications/TextEdit.app", true),
            app("bbedit", "/Volumes/Old/BBEdit.app", false),
            app("", "/Applications/Nameless.app", false),
        ]);
        let names: Vec<_> = menu.iter().map(|app| app.name.as_str()).collect();
        assert_eq!(names, ["TextEdit", "BBEdit", "Zed"]);
        assert_eq!(menu[1].path, PathBuf::from("/Applications/BBEdit.app"));
        assert_eq!(menu[0].bundle_id.as_deref(), Some("test.TextEdit"));
        let defaults: Vec<_> = menu.iter().map(|app| app.is_default).collect();
        assert_eq!(defaults, [true, false, false]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_open_with_lists_launch_services_handlers() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("notes.txt");
        fs::write(&path, "plain text").unwrap();

        let apps = apps_for_file(&path).unwrap();
        let text_edit = apps
            .iter()
            .find(|app| app.bundle_id.as_deref() == Some("com.apple.TextEdit"))
            .unwrap_or_else(|| panic!("TextEdit should open .txt files: {apps:?}"));
        assert_eq!(text_edit.name, "TextEdit");
        assert!(text_edit.path.ends_with("TextEdit.app"));
        assert!(apps.iter().all(|app| app.path.exists()));
        let mut names: Vec<_> = apps.iter().map(|app| app.name.to_lowercase()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), apps.len(), "names are unique: {apps:?}");
    }

    #[cfg(target_os = "macos")]
    fn write_raw_tags(path: &Path, values: Vec<plist::Value>) {
        let mut encoded = Vec::new();
        plist::Value::Array(values)
            .to_writer_binary(&mut encoded)
            .unwrap();
        write_raw_tag_bytes(path, &encoded);
    }

    #[cfg(target_os = "macos")]
    fn write_raw_tag_bytes(path: &Path, bytes: &[u8]) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: NUL-terminated strings and a pointer/length pair for `bytes`.
        let result = unsafe {
            libc::setxattr(
                c_path.as_ptr(),
                c"com.apple.metadata:_kMDItemUserTags".as_ptr(),
                bytes.as_ptr().cast(),
                bytes.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        assert_eq!(result, 0, "{}", io::Error::last_os_error());
    }

    #[cfg(target_os = "macos")]
    fn read_raw_tags(path: &Path) -> Vec<plist::Value> {
        let output = Command::new("xattr")
            .args(["-px", "com.apple.metadata:_kMDItemUserTags"])
            .arg(path)
            .output()
            .unwrap();
        assert!(output.status.success());
        let hex: String = String::from_utf8(output.stdout)
            .unwrap()
            .split_whitespace()
            .collect();
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
            .collect();
        plist::Value::from_reader(io::Cursor::new(bytes))
            .unwrap()
            .into_array()
            .unwrap()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_finder_tags_read_colors_and_names_straight_from_the_xattr() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("tagged.txt");
        fs::write(&path, "content").unwrap();
        assert!(get_finder_tags(&path).unwrap().is_empty());

        write_raw_tags(
            &path,
            vec![
                plist::Value::String("Important\n6".into()),
                plist::Value::String("Client, Inc.".into()),
                plist::Value::String("say \"hi\"".into()),
            ],
        );
        assert_eq!(
            get_finder_tags(&path).unwrap(),
            vec![
                "Important\n6".to_string(),
                "Client, Inc.".to_string(),
                "say \"hi\"".to_string()
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_finder_tag_writes_keep_colors_and_unknown_entries() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("tagged.txt");
        fs::write(&path, "content").unwrap();
        write_raw_tags(
            &path,
            vec![
                plist::Value::String("Important\n6".into()),
                plist::Value::String("Plain".into()),
                plist::Value::Integer(42.into()),
            ],
        );
        assert_eq!(
            get_finder_tags(&path).unwrap(),
            vec!["Important\n6".to_string(), "Plain".to_string()]
        );

        set_finder_tags(&path, &["Important".to_string(), "Review\n4".to_string()]).unwrap();
        assert_eq!(
            read_raw_tags(&path),
            vec![
                plist::Value::String("Important\n6".into()),
                plist::Value::String("Review\n4".into()),
                plist::Value::Integer(42.into()),
            ]
        );

        // Removing every tag keeps the entry this code does not understand.
        set_finder_tags(&path, &[]).unwrap();
        assert_eq!(read_raw_tags(&path), vec![plist::Value::Integer(42.into())]);
        assert!(get_finder_tags(&path).unwrap().is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_unreadable_finder_tags_are_reported_and_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("tagged.txt");
        fs::write(&path, "content").unwrap();
        write_raw_tag_bytes(&path, b"not a plist");

        assert_eq!(
            get_finder_tags(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            set_finder_tags(&path, &["Work".to_string()])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        let output = Command::new("xattr")
            .args(["-p", "com.apple.metadata:_kMDItemUserTags"])
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "not a plist"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_finder_tag_write_and_clear_preserve_source_content() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("tagged.txt");
        fs::write(&path, "source stays unchanged").unwrap();
        set_finder_tags(&path, &["Review\n4".to_string()]).unwrap();

        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let attr_name = CString::new("com.apple.metadata:_kMDItemUserTags").unwrap();
        let size = unsafe {
            libc::getxattr(
                c_path.as_ptr(),
                attr_name.as_ptr(),
                std::ptr::null_mut(),
                0,
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        assert!(size > 0, "Finder tag xattr should contain a binary plist");

        set_finder_tags(&path, &[]).unwrap();
        let cleared = unsafe {
            libc::getxattr(
                c_path.as_ptr(),
                attr_name.as_ptr(),
                std::ptr::null_mut(),
                0,
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        assert_eq!(cleared, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ENOATTR)
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "source stays unchanged");
    }
}
