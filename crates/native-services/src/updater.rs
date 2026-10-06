#[cfg(target_os = "macos")]
use crate::process::{ProcessError, run_with_timeout};
use crate::{BlockingTask, ErrorCode, ServiceContext, ServiceError, ServiceResult};
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
#[cfg(target_os = "macos")]
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(any(windows, target_os = "macos"))]
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::thread;
use std::time::Duration;

const RELEASE_API_URL: &str = "https://api.github.com/repos/bildhaus/explorie/releases/latest";
const RELEASE_DOWNLOAD_PREFIX: &str = "https://github.com/bildhaus/explorie/releases/download";
const MAX_RELEASE_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_UPDATE_BYTES: u64 = 512 * 1024 * 1024;
const MIN_UPDATE_BYTES: u64 = 1024 * 1024;
/// The minisign public key that update payloads must be signed with. The
/// checked-in file carries no key until a maintainer configures signing (see
/// "Update signing" in README.md); `EXPLORIE_UPDATE_PUBLIC_KEY` can override it
/// at build time. Once a key is compiled in, every update needs a valid
/// signature on every platform; without one, updates rely on GitHub's
/// SHA-256 digest alone.
const UPDATE_PUBLIC_KEY_FILE: &str = include_str!("../update-signing-key.pub");
const SIGNATURE_LINE_PREFIX: &str = "explorie-signature";
const SIGNATURE_BLOCK_START: &str = "<!-- explorie-update-signatures";
const SIGNATURE_BLOCK_END: &str = "-->";
#[cfg(any(windows, test))]
const WINDOWS_INSTALLER_ARGUMENTS: [&str; 6] = [
    "/SP-",
    "/VERYSILENT",
    "/SUPPRESSMSGBOXES",
    "/NORESTART",
    "/CLOSEAPPLICATIONS",
    "/RELAUNCHEXPLORIE",
];
#[cfg(target_os = "macos")]
const MACOS_UPDATE_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(target_os = "macos")]
const MAX_MACOS_UPDATE_COMMAND_OUTPUT: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UpdatePlatform {
    Windows,
    Macos,
}

impl UpdatePlatform {
    fn current() -> Option<Self> {
        if cfg!(windows) {
            Some(Self::Windows)
        } else if cfg!(target_os = "macos") {
            Some(Self::Macos)
        } else {
            None
        }
    }

    fn asset_name(self, version: &str) -> String {
        match self {
            Self::Windows => windows_installer_name(version),
            Self::Macos => macos_dmg_name(version),
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Windows => "Windows installer",
            Self::Macos => "macOS disk image",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: Option<String>,
    pub asset_name: String,
    pub download_url: String,
    pub sha256: String,
    pub size: u64,
    /// The minisign signature published for this asset, when present.
    pub signature: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadedUpdate {
    pub info: UpdateInfo,
    pub installer_path: PathBuf,
    pub sha256: String,
}

#[derive(Clone)]
pub struct UpdateService {
    context: ServiceContext,
}

impl UpdateService {
    pub(crate) fn new(context: ServiceContext) -> Self {
        Self { context }
    }

    pub fn check(&self) -> BlockingTask<Option<UpdateInfo>> {
        let current_version = self.context.resources().app_version.clone();
        self.context.spawn_blocking(move || {
            let Some(platform) = UpdatePlatform::current() else {
                return Ok(None);
            };
            let key = configured_update_key()?;
            let release = get_bytes(RELEASE_API_URL, MAX_RELEASE_METADATA_BYTES)?;
            discover_update(&current_version, &release, platform, key.as_ref())
        })
    }

    pub fn download(&self, update: UpdateInfo) -> BlockingTask<DownloadedUpdate> {
        let cache_dir = self.context.resources().cache_dir.join("updates");
        self.context.spawn_blocking(move || {
            let key = configured_update_key()?;
            validate_update_info(&update)?;
            require_signature(&update, key.as_ref())?;
            fs::create_dir_all(&cache_dir).map_err(ServiceError::from)?;

            let expected_sha256 = update.sha256.clone();
            let installer_path = cache_dir.join(&update.asset_name);
            if installer_path.is_file()
                && hash_file(&installer_path)? == expected_sha256
                && installer_path
                    .metadata()
                    .map(|value| value.len())
                    .unwrap_or(0)
                    == update.size
                && verify_update_signature(&installer_path, &update, key.as_ref()).is_ok()
            {
                return Ok(DownloadedUpdate {
                    info: update,
                    installer_path,
                    sha256: expected_sha256,
                });
            }

            let staged_path = cache_dir.join(format!("{}.part", update.asset_name));
            let _ = fs::remove_file(&staged_path);
            let result = download_installer(
                &update.download_url,
                &staged_path,
                update.size,
                &expected_sha256,
            )
            .and_then(|()| verify_update_signature(&staged_path, &update, key.as_ref()));
            if let Err(error) = result {
                let _ = fs::remove_file(&staged_path);
                return Err(error);
            }
            if installer_path.exists() {
                fs::remove_file(&installer_path).map_err(ServiceError::from)?;
            }
            fs::rename(&staged_path, &installer_path).map_err(ServiceError::from)?;

            Ok(DownloadedUpdate {
                info: update,
                installer_path,
                sha256: expected_sha256,
            })
        })
    }

    pub fn launch(&self, update: DownloadedUpdate) -> BlockingTask<()> {
        let cache_dir = self.context.resources().cache_dir.join("updates");
        #[cfg(target_os = "macos")]
        let resources = self.context.resources().clone();
        self.context.spawn_blocking(move || {
            validate_signed_downloaded_update(&cache_dir, &update)?;
            #[cfg(windows)]
            return launch_installer(&update.installer_path);
            #[cfg(target_os = "macos")]
            return launch_macos_update_helper(&resources, &update);
            #[cfg(not(any(windows, target_os = "macos")))]
            Err(ServiceError::new(
                ErrorCode::Unsupported,
                "Automatic updates are unavailable on this platform",
            ))
        })
    }
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    body: Option<String>,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

fn discover_update(
    current_version: &str,
    release_json: &[u8],
    platform: UpdatePlatform,
    key: Option<&PublicKey>,
) -> ServiceResult<Option<UpdateInfo>> {
    let current = Version::parse(current_version).map_err(|_| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            "The installed Explorie version is not valid semantic version data",
        )
    })?;
    let release: GitHubRelease = serde_json::from_slice(release_json).map_err(|_| {
        ServiceError::new(
            ErrorCode::RemoteUnavailable,
            "GitHub returned malformed release metadata",
        )
        .retryable(true)
    })?;
    let version_text = release.tag_name.strip_prefix('v').ok_or_else(|| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            "The latest Explorie release tag is malformed",
        )
    })?;
    let version = Version::parse(version_text).map_err(|_| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            "The latest Explorie release version is malformed",
        )
    })?;
    if version <= current {
        return Ok(None);
    }

    let asset_name = platform.asset_name(version_text);
    let installer = release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)
        .ok_or_else(|| {
            ServiceError::new(
                ErrorCode::NotFound,
                format!(
                    "Release v{version_text} has no compatible {}",
                    platform.display_name()
                ),
            )
        })?;
    if !(MIN_UPDATE_BYTES..=MAX_UPDATE_BYTES).contains(&installer.size) {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "The update payload has an invalid size",
        ));
    }
    let body = release.body.unwrap_or_default();
    let signature = match key {
        Some(_) => release_signature(&body, &asset_name)?,
        None => None,
    };
    let notes = release_notes(&body);
    let update = UpdateInfo {
        version: version_text.to_string(),
        notes: (!notes.is_empty()).then_some(notes),
        asset_name,
        download_url: installer.browser_download_url.clone(),
        sha256: sha256_from_digest(installer.digest.as_deref())?,
        size: installer.size,
        signature,
    };
    validate_update_info_for_platform(&update, platform)?;
    require_signature(&update, key)?;
    Ok(Some(update))
}

fn configured_update_key() -> ServiceResult<Option<PublicKey>> {
    parse_update_key(
        option_env!("EXPLORIE_UPDATE_PUBLIC_KEY")
            .filter(|key| !key.trim().is_empty())
            .unwrap_or(UPDATE_PUBLIC_KEY_FILE),
    )
}

/// Parse a minisign public key file (or a bare base64 key). Comment-only
/// input means signing is not configured.
fn parse_update_key(text: &str) -> ServiceResult<Option<PublicKey>> {
    let mut keys = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("untrusted comment:"));
    let Some(key) = keys.next() else {
        return Ok(None);
    };
    let invalid = || {
        ServiceError::new(
            ErrorCode::Internal,
            "The embedded update signing key is invalid; updates are disabled",
        )
    };
    if keys.next().is_some() {
        return Err(invalid());
    }
    PublicKey::from_base64(key).map(Some).map_err(|_| invalid())
}

fn signature_trusted_comment(asset_name: &str) -> String {
    format!("explorie-update {asset_name}")
}

/// Find the `explorie-signature <asset> <signature> <global-signature>` line for
/// `asset_name` and rebuild the minisign signature it stands for. The trusted
/// comment is derived from the expected asset name, so a signature published
/// for any other asset (including an older release) cannot verify.
fn release_signature(body: &str, asset_name: &str) -> ServiceResult<Option<String>> {
    let malformed = || {
        ServiceError::new(
            ErrorCode::InvalidInput,
            "The release notes contain a malformed update signature",
        )
    };
    let mut found = None;
    for line in body.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some(SIGNATURE_LINE_PREFIX) {
            continue;
        }
        let (Some(name), Some(signature), Some(global), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(malformed());
        };
        if name != asset_name {
            continue;
        }
        if found.is_some() {
            return Err(ServiceError::new(
                ErrorCode::InvalidInput,
                "The release notes contain more than one signature for this update",
            ));
        }
        let text = format!(
            "untrusted comment: explorie update signature\n{signature}\ntrusted comment: {}\n{global}\n",
            signature_trusted_comment(asset_name)
        );
        Signature::decode(&text).map_err(|_| malformed())?;
        found = Some(text);
    }
    Ok(found)
}

/// Release notes without the machine-readable signature block.
fn release_notes(body: &str) -> String {
    let mut kept = Vec::new();
    let mut inside = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if !inside && trimmed == SIGNATURE_BLOCK_START {
            inside = true;
        } else if inside {
            inside = trimmed != SIGNATURE_BLOCK_END;
        } else if !trimmed.starts_with(SIGNATURE_LINE_PREFIX) {
            kept.push(line);
        }
    }
    kept.join("\n").trim().to_string()
}

fn require_signature(update: &UpdateInfo, key: Option<&PublicKey>) -> ServiceResult<()> {
    if key.is_some() && update.signature.is_none() {
        return Err(ServiceError::new(
            ErrorCode::PermissionDenied,
            "The update is not signed with the Explorie update key",
        ));
    }
    Ok(())
}

/// Check the payload's detached ed25519 signature when a key is configured.
fn verify_update_signature(
    path: &Path,
    update: &UpdateInfo,
    key: Option<&PublicKey>,
) -> ServiceResult<()> {
    let Some(key) = key else {
        return Ok(());
    };
    require_signature(update, Some(key))?;
    let rejected = || {
        ServiceError::new(
            ErrorCode::PermissionDenied,
            "The update payload failed its Explorie signature check",
        )
    };
    let signature = update
        .signature
        .as_deref()
        .and_then(|text| Signature::decode(text).ok())
        .filter(|signature| {
            signature.trusted_comment() == signature_trusted_comment(&update.asset_name)
        })
        .ok_or_else(rejected)?;
    let mut verifier = key.verify_stream(&signature).map_err(|_| rejected())?;
    let mut file = File::open(path).map_err(ServiceError::from)?;
    let mut buffer = vec![0_u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(ServiceError::from)?;
        if count == 0 {
            break;
        }
        verifier.update(&buffer[..count]);
    }
    verifier.finalize().map_err(|_| rejected())
}

fn validate_update_info(update: &UpdateInfo) -> ServiceResult<()> {
    let platform = UpdatePlatform::current().ok_or_else(|| {
        ServiceError::new(
            ErrorCode::Unsupported,
            "Automatic updates are unavailable on this platform",
        )
    })?;
    validate_update_info_for_platform(update, platform)
}

fn validate_update_info_for_platform(
    update: &UpdateInfo,
    platform: UpdatePlatform,
) -> ServiceResult<()> {
    if Version::parse(&update.version).is_err() {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "The update version is malformed",
        ));
    }
    let expected_asset = platform.asset_name(&update.version);
    if update.asset_name != expected_asset
        || update.download_url != release_asset_url(&update.version, &expected_asset)
        || !is_sha256(&update.sha256)
        || !(MIN_UPDATE_BYTES..=MAX_UPDATE_BYTES).contains(&update.size)
    {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "The update metadata does not match the Explorie release contract",
        ));
    }
    Ok(())
}

fn windows_installer_name(version: &str) -> String {
    format!("explorie-{version}-windows-x64-setup-unsigned.exe")
}

fn macos_dmg_name(version: &str) -> String {
    format!("explorie-{version}-macos-arm64.dmg")
}

fn release_asset_url(version: &str, asset_name: &str) -> String {
    format!("{RELEASE_DOWNLOAD_PREFIX}/v{version}/{asset_name}")
}

fn sha256_from_digest(digest: Option<&str>) -> ServiceResult<String> {
    let hash = digest
        .and_then(|value| value.strip_prefix("sha256:"))
        .filter(|hash| is_sha256(hash))
        .ok_or_else(|| {
            ServiceError::new(
                ErrorCode::InvalidInput,
                "The update asset is missing a valid GitHub SHA-256 digest",
            )
        })?;
    Ok(hash.to_ascii_lowercase())
}

fn is_sha256(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|value| value.is_ascii_hexdigit())
}

fn get_bytes(url: &str, limit: u64) -> ServiceResult<Vec<u8>> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .timeout_write(Duration::from_secs(30))
        .build();
    let request = agent
        .get(url)
        .set("User-Agent", "explorie-updater")
        .set("Accept", "application/vnd.github+json");
    let response = request.call().map_err(network_error)?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(ServiceError::from)?;
    if bytes.len() as u64 > limit {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "The update response exceeded its safety limit",
        ));
    }
    Ok(bytes)
}

fn download_installer(
    url: &str,
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> ServiceResult<()> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .timeout_write(Duration::from_secs(30))
        .build();
    let response = agent
        .get(url)
        .set("User-Agent", "explorie-updater")
        .call()
        .map_err(network_error)?;
    let mut reader = response.into_reader();
    let mut file = File::create(path).map_err(ServiceError::from)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(ServiceError::from)?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > MAX_UPDATE_BYTES || total > expected_size {
            return Err(ServiceError::new(
                ErrorCode::InvalidInput,
                "The update payload exceeded its declared size",
            ));
        }
        hasher.update(&buffer[..count]);
        file.write_all(&buffer[..count])
            .map_err(ServiceError::from)?;
    }
    file.sync_all().map_err(ServiceError::from)?;
    if total != expected_size {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "The update payload size does not match its release metadata",
        ));
    }
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if actual_sha256 != expected_sha256 {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "The update payload failed its SHA-256 integrity check",
        ));
    }
    Ok(())
}

fn hash_file(path: &Path) -> ServiceResult<String> {
    let mut file = File::open(path).map_err(ServiceError::from)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher).map_err(ServiceError::from)?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_downloaded_update(cache_dir: &Path, update: &DownloadedUpdate) -> ServiceResult<()> {
    let platform = UpdatePlatform::current().ok_or_else(|| {
        ServiceError::new(
            ErrorCode::Unsupported,
            "Automatic updates are unavailable on this platform",
        )
    })?;
    validate_downloaded_update_for_platform(cache_dir, update, platform)
}

fn validate_signed_downloaded_update(
    cache_dir: &Path,
    update: &DownloadedUpdate,
) -> ServiceResult<()> {
    validate_downloaded_update(cache_dir, update)?;
    verify_update_signature(
        &update.installer_path,
        &update.info,
        configured_update_key()?.as_ref(),
    )
}

fn validate_downloaded_update_for_platform(
    cache_dir: &Path,
    update: &DownloadedUpdate,
    platform: UpdatePlatform,
) -> ServiceResult<()> {
    validate_update_info_for_platform(&update.info, platform)?;
    let expected_path = cache_dir.join(&update.info.asset_name);
    if update.installer_path != expected_path || !expected_path.is_file() {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "The prepared update is outside the Explorie update cache",
        ));
    }
    let actual_sha256 = hash_file(&expected_path)?;
    if expected_path
        .metadata()
        .map(|value| value.len())
        .unwrap_or(0)
        != update.info.size
        || actual_sha256 != update.info.sha256
        || actual_sha256 != update.sha256
    {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "The prepared update changed after verification",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn launch_installer(installer: &Path) -> ServiceResult<()> {
    let mut command = Command::new(installer);
    command
        .args(WINDOWS_INSTALLER_ARGUMENTS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.spawn().map(|_| ()).map_err(|error| {
        ServiceError::from(error)
            .operation("launch_update")
            .retryable(true)
    })
}

#[cfg(target_os = "macos")]
fn launch_macos_update_helper(
    resources: &crate::ResourcePaths,
    update: &DownloadedUpdate,
) -> ServiceResult<()> {
    let current_exe = resources.current_exe.as_deref().ok_or_else(|| {
        ServiceError::new(
            ErrorCode::NotFound,
            "The installed Explorie executable could not be located",
        )
    })?;
    macos_installed_app_bundle(current_exe)?;
    let mut command = Command::new(current_exe);
    command
        .arg("--apply-macos-update")
        .arg(&update.installer_path)
        .arg(&update.info.version)
        .arg(&update.sha256)
        .arg(update.info.size.to_string())
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn().map(|_| ()).map_err(|error| {
        ServiceError::from(error)
            .operation("launch_update")
            .retryable(true)
    })
}

#[cfg(target_os = "macos")]
pub fn apply_macos_update_command(
    args: impl IntoIterator<Item = OsString>,
) -> Option<ServiceResult<()>> {
    let mut args = args.into_iter().skip(1);
    while let Some(argument) = args.next() {
        if argument != "--apply-macos-update" {
            continue;
        }
        let reopen_app = std::env::current_exe()
            .ok()
            .and_then(|current_exe| macos_installed_app_bundle(&current_exe).ok());
        let parsed = (|| {
            let payload = PathBuf::from(args.next().ok_or_else(|| {
                ServiceError::new(
                    ErrorCode::InvalidInput,
                    "The update payload path is missing",
                )
            })?);
            let version = os_string_argument(args.next(), "update version")?;
            let sha256 = os_string_argument(args.next(), "update checksum")?;
            let size = os_string_argument(args.next(), "update size")?
                .parse::<u64>()
                .map_err(|_| {
                    ServiceError::new(ErrorCode::InvalidInput, "The update size is malformed")
                })?;
            let parent_pid = os_string_argument(args.next(), "parent process")?
                .parse::<u32>()
                .map_err(|_| {
                    ServiceError::new(
                        ErrorCode::InvalidInput,
                        "The update parent process is malformed",
                    )
                })?;
            apply_macos_update(payload, version, sha256, size, parent_pid)
        })();
        if parsed.is_err()
            && let Some(app) = reopen_app
            && app.is_dir()
        {
            let _ = open_macos_app(&app);
        }
        return Some(parsed);
    }
    None
}

#[cfg(target_os = "macos")]
fn os_string_argument(value: Option<OsString>, label: &str) -> ServiceResult<String> {
    value
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| {
            ServiceError::new(ErrorCode::InvalidInput, format!("The {label} is missing"))
        })
}

#[cfg(target_os = "macos")]
fn apply_macos_update(
    payload: PathBuf,
    version: String,
    sha256: String,
    size: u64,
    parent_pid: u32,
) -> ServiceResult<()> {
    let current_exe = std::env::current_exe().map_err(ServiceError::from)?;
    let target_app = macos_installed_app_bundle(&current_exe)?;
    wait_for_process_exit(parent_pid)?;

    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("explorie/updates");
    let info = UpdateInfo {
        version: version.clone(),
        notes: None,
        asset_name: macos_dmg_name(&version),
        download_url: release_asset_url(&version, &macos_dmg_name(&version)),
        sha256: sha256.clone(),
        size,
        signature: None,
    };
    let update = DownloadedUpdate {
        info,
        installer_path: payload.clone(),
        sha256,
    };
    validate_downloaded_update(&cache_dir, &update)?;

    let old_team = verify_macos_app(&target_app, None, None)?;
    run_macos_command(
        Command::new("/usr/bin/hdiutil").arg("verify").arg(&payload),
        "The downloaded update disk image is invalid",
    )?;
    run_macos_command(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--verbose=2"])
            .arg(&payload),
        "The downloaded update disk image has an invalid signature",
    )?;
    run_macos_command(
        Command::new("/usr/sbin/spctl")
            .args([
                "--assess",
                "--type",
                "open",
                "--context",
                "context:primary-signature",
                "--verbose=2",
            ])
            .arg(&payload),
        "Gatekeeper rejected the downloaded update disk image",
    )?;

    let mut mounted = MountedUpdate::attach(&payload)?;
    let source_app = find_mounted_update_app(&mounted.mount_point)?;
    verify_macos_app(&source_app, Some(&version), Some(&old_team))?;

    let parent = target_app.parent().ok_or_else(|| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            "The installed Explorie application has no parent directory",
        )
    })?;
    let nonce = uuid::Uuid::new_v4();
    let staged_app = parent.join(format!(".explorie-update-{nonce}.app"));
    let backup_app = parent.join(format!(".explorie-backup-{nonce}.app"));
    if let Err(error) = run_macos_command(
        Command::new("/usr/bin/ditto")
            .arg(&source_app)
            .arg(&staged_app),
        "Unable to stage the Explorie update",
    ) {
        let _ = fs::remove_dir_all(&staged_app);
        return Err(error);
    }
    if let Err(error) = verify_macos_app(&staged_app, Some(&version), Some(&old_team)) {
        let _ = fs::remove_dir_all(&staged_app);
        return Err(error);
    }
    if let Err(error) = mounted.detach() {
        let _ = fs::remove_dir_all(&staged_app);
        return Err(error);
    }

    if let Err(error) = fs::rename(&target_app, &backup_app) {
        let _ = fs::remove_dir_all(&staged_app);
        return Err(ServiceError::from(error)
            .operation("backup_installed_update")
            .retryable(true));
    }
    if let Err(error) = fs::rename(&staged_app, &target_app) {
        if let Err(restore_error) = fs::rename(&backup_app, &target_app) {
            return Err(ServiceError::new(
                ErrorCode::Io,
                format!(
                    "The update replacement failed and rollback failed: {error}; {restore_error}"
                ),
            ));
        }
        let _ = fs::remove_dir_all(&staged_app);
        return Err(ServiceError::from(error)
            .operation("replace_installed_update")
            .retryable(true));
    }

    if let Err(error) = open_macos_app(&target_app) {
        fs::remove_dir_all(&target_app).map_err(|remove_error| {
            ServiceError::new(
                ErrorCode::Io,
                format!(
                    "The update could not reopen and its staged app could not be removed: {error}; {remove_error}"
                ),
            )
        })?;
        fs::rename(&backup_app, &target_app).map_err(|restore_error| {
            ServiceError::new(
                ErrorCode::Io,
                format!(
                    "The update could not reopen and rollback failed: {error}; {restore_error}"
                ),
            )
        })?;
        let _ = open_macos_app(&target_app);
        return Err(error);
    }

    remove_directory_with_retries(&backup_app);
    remove_file_with_retries(&payload);
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_installed_app_bundle(current_exe: &Path) -> ServiceResult<PathBuf> {
    let app = current_exe
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .ok_or_else(|| {
            ServiceError::new(
                ErrorCode::InvalidInput,
                "Automatic updates require an installed Explorie application bundle",
            )
        })?;
    let installed_roots = [
        PathBuf::from("/Applications"),
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join("Applications"),
    ];
    if !installed_roots.iter().any(|root| app.starts_with(root))
        || !app.is_dir()
        || fs::symlink_metadata(app)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(true)
    {
        return Err(ServiceError::new(
            ErrorCode::PermissionDenied,
            "Move Explorie to Applications before installing updates",
        ));
    }
    Ok(app.to_path_buf())
}

#[cfg(target_os = "macos")]
fn wait_for_process_exit(parent_pid: u32) -> ServiceResult<()> {
    if parent_pid == 0 {
        return Ok(());
    }
    for _ in 0..240 {
        // SAFETY: Signal zero checks whether the specified process still exists
        // without delivering a signal or changing process state.
        let status = unsafe { libc::kill(parent_pid as libc::pid_t, 0) };
        if status != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(ServiceError::new(
        ErrorCode::Busy,
        "Explorie did not finish closing before the update timeout",
    )
    .retryable(true))
}

#[cfg(target_os = "macos")]
fn verify_macos_app(
    app: &Path,
    expected_version: Option<&str>,
    expected_team: Option<&str>,
) -> ServiceResult<String> {
    let plist = fs::read_to_string(app.join("Contents/Info.plist")).map_err(ServiceError::from)?;
    if !plist.contains("<string>com.omershatz.explorie</string>") {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "The update application has an unexpected bundle identifier",
        ));
    }
    if let Some(version) = expected_version {
        let expected = format!("<key>CFBundleShortVersionString</key><string>{version}</string>");
        if !plist.contains(&expected) {
            return Err(ServiceError::new(
                ErrorCode::InvalidInput,
                "The update application version does not match the release",
            ));
        }
    }
    run_macos_command(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict", "--verbose=2"])
            .arg(app),
        "The update application has an invalid code signature",
    )?;
    run_macos_command(
        Command::new("/usr/sbin/spctl")
            .args(["--assess", "--type", "execute", "--verbose=2"])
            .arg(app),
        "Gatekeeper rejected the update application",
    )?;
    let details = run_macos_command(
        Command::new("/usr/bin/codesign")
            .args(["-d", "--verbose=4"])
            .arg(app),
        "Unable to inspect the update application signature",
    )?;
    let details = String::from_utf8_lossy(&details);
    let team = details
        .lines()
        .find_map(|line| line.trim().strip_prefix("TeamIdentifier="))
        .filter(|team| !team.is_empty() && *team != "not set")
        .ok_or_else(|| {
            ServiceError::new(
                ErrorCode::InvalidInput,
                "The update application has no Developer ID team identifier",
            )
        })?;
    if expected_team.is_some_and(|expected| expected != team) {
        return Err(ServiceError::new(
            ErrorCode::PermissionDenied,
            "The update application was signed by a different developer team",
        ));
    }
    Ok(team.to_string())
}

#[cfg(target_os = "macos")]
fn find_mounted_update_app(mount_point: &Path) -> ServiceResult<PathBuf> {
    let mut apps = fs::read_dir(mount_point)
        .map_err(ServiceError::from)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "app"));
    let app = apps.next().ok_or_else(|| {
        ServiceError::new(
            ErrorCode::NotFound,
            "The update disk image contains no application bundle",
        )
    })?;
    if apps.next().is_some() {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "The update disk image contains multiple application bundles",
        ));
    }
    Ok(app)
}

#[cfg(target_os = "macos")]
struct MountedUpdate {
    mount_point: PathBuf,
    attached: bool,
}

#[cfg(target_os = "macos")]
impl MountedUpdate {
    fn attach(payload: &Path) -> ServiceResult<Self> {
        let mount_point = std::env::temp_dir().join(format!(
            "explorie-update-mount-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&mount_point).map_err(ServiceError::from)?;
        let attached = run_macos_command(
            Command::new("/usr/bin/hdiutil")
                .args(["attach", "-nobrowse", "-readonly", "-mountpoint"])
                .arg(&mount_point)
                .arg(payload),
            "Unable to mount the update disk image",
        );
        if let Err(error) = attached {
            let _ = fs::remove_dir(&mount_point);
            return Err(error);
        }
        Ok(Self {
            mount_point,
            attached: true,
        })
    }

    fn detach(&mut self) -> ServiceResult<()> {
        if self.attached {
            run_macos_command(
                Command::new("/usr/bin/hdiutil")
                    .arg("detach")
                    .arg(&self.mount_point),
                "Unable to unmount the update disk image",
            )?;
            self.attached = false;
        }
        let _ = fs::remove_dir(&self.mount_point);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl Drop for MountedUpdate {
    fn drop(&mut self) {
        if self.attached {
            let _ = Command::new("/usr/bin/hdiutil")
                .arg("detach")
                .arg(&self.mount_point)
                .status();
        }
        let _ = fs::remove_dir(&self.mount_point);
    }
}

#[cfg(target_os = "macos")]
fn run_macos_command(command: &mut Command, message: &str) -> ServiceResult<Vec<u8>> {
    let output = run_with_timeout(
        command,
        MACOS_UPDATE_COMMAND_TIMEOUT,
        MAX_MACOS_UPDATE_COMMAND_OUTPUT,
        MAX_MACOS_UPDATE_COMMAND_OUTPUT,
    )
    .map_err(|error| match error {
        ProcessError::Io(error) => ServiceError::from(error),
        ProcessError::TimedOut => ServiceError::new(
            ErrorCode::Busy,
            format!("{message}: the system command timed out"),
        )
        .retryable(true),
    })?;
    if output.status.success() {
        let mut details = output.stdout;
        details.extend_from_slice(&output.stderr);
        return Ok(details);
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    Err(ServiceError::new(
        ErrorCode::PermissionDenied,
        if detail.trim().is_empty() {
            message.to_string()
        } else {
            format!("{message}: {}", detail.trim())
        },
    ))
}

#[cfg(target_os = "macos")]
fn open_macos_app(app: &Path) -> ServiceResult<()> {
    run_macos_command(
        Command::new("/usr/bin/open").args(["-n"]).arg(app),
        "Unable to reopen Explorie after updating",
    )
    .map(|_| ())
}

#[cfg(target_os = "macos")]
fn remove_directory_with_retries(path: &Path) {
    for _ in 0..120 {
        match fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(_) => thread::sleep(Duration::from_millis(250)),
        }
    }
}

#[cfg(target_os = "macos")]
fn remove_file_with_retries(path: &Path) {
    for _ in 0..120 {
        match fs::remove_file(path) {
            Ok(()) => return,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(_) => thread::sleep(Duration::from_millis(250)),
        }
    }
}

fn network_error(error: ureq::Error) -> ServiceError {
    ServiceError::new(
        ErrorCode::RemoteUnavailable,
        format!("Unable to reach the Explorie release service: {error}"),
    )
    .retryable(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test-only ed25519 keys generated for these fixtures by
    // scripts/update-signatures.mjs; their secret halves were discarded. Both
    // fixture signatures cover the 1 MiB zero-filled payload.
    const TEST_KEY: &str = "RWTZ6T/8eOBV4I3UjVXPu0uZMs2rrD1h2YJl5nxr/rMj90e6Axd/pfaQ";
    const OTHER_TEST_KEY: &str = "RWSWD5kmw0r/1aKrqjoe/CciS3d3f9fiMVSC5MRbS+vmMSN472uabYGa";
    const WINDOWS_SIGNATURE: &str = "explorie-signature explorie-0.2.9-windows-x64-setup-unsigned.exe RUTZ6T/8eOBV4Ivm4P0yttgnHE8ExHfPtBreSiD5z57uWPnLe6xFbZoLoydb6RVnyejriXKYmL7irFjHh7RuytRVJaJCJGo0mAI= YN/t82kCrv4Oi9h74EqSsYKd3tBgQa51spyEmrt0mS8IYtUfFCPAJzXzgOENT791Hsd4bLsp06fK9Z00xpk6Cw==";
    const MACOS_SIGNATURE: &str = "explorie-signature explorie-0.2.9-macos-arm64.dmg RUTZ6T/8eOBV4Ivm4P0yttgnHE8ExHfPtBreSiD5z57uWPnLe6xFbZoLoydb6RVnyejriXKYmL7irFjHh7RuytRVJaJCJGo0mAI= GOi6x7crwLKbFX9DcBkSvLeRr1IdjAZP2IzHEqztuMm+rFX7VdyJmo5ElRjolZkjrbzQJAotnwrfr5ZSm+OwDg==";
    const OTHER_KEY_MACOS_SIGNATURE: &str = "explorie-signature explorie-0.2.9-macos-arm64.dmg RUSWD5kmw0r/1YSeBE+bgVAhJasY+21J7gLUKjY+V9zFrFvpdqvykDedyf3eTEekFzmBXLY9Xo5J+ngBx0QXKtflgOPYsZNlZwA= xQw3JOolO+e4AyW9qOpuRPNUYwNhBxNNrdVEG4PYSMCfcUx3rt4RjBzbNQjfPkzYf1I+dT5Ry86rSyMmJgAXBg==";

    // Made by an independent minisign implementation (rsign2 0.6.7) over the
    // same payload with `-t "explorie-update explorie-0.2.9-macos-arm64.dmg"`.
    const RSIGN_TEST_KEY: &str = "RWQqqwbfKtw2IClva6awfwNN/7AzqRTTVAulQk++WRgrJZ0RD+XsBR0q";
    const RSIGN_MACOS_SIGNATURE: &str = "explorie-signature explorie-0.2.9-macos-arm64.dmg RUQqqwbfKtw2IJPoJiMeXyujyiSp3ul9o6REHxLg/9E5ZaMBebq6mzVf8m5d3dw/4C+KPzVzghOpZ+wjkqdGH7wd3Ywj/jeqkgo= +d7rrucTgXOacdm0Bvru90Qr4Qyz9J/fu80g07W9Hr5aHp/rOKN01tcn/Yt4hhnL7rQw9MDjMt8ZWAmOGWopAg==";

    /// Discovery as it behaves in builds without an update signing key.
    fn discover_update(
        current_version: &str,
        release_json: &[u8],
        platform: UpdatePlatform,
    ) -> ServiceResult<Option<UpdateInfo>> {
        super::discover_update(current_version, release_json, platform, None)
    }

    fn test_key(key: &str) -> PublicKey {
        parse_update_key(key).unwrap().unwrap()
    }

    fn signature_line(platform: UpdatePlatform) -> &'static str {
        match platform {
            UpdatePlatform::Windows => WINDOWS_SIGNATURE,
            UpdatePlatform::Macos => MACOS_SIGNATURE,
        }
    }

    fn signed_release_json(platform: UpdatePlatform, body: &str) -> Vec<u8> {
        let name = platform.asset_name("0.2.9");
        let mut release: serde_json::Value =
            serde_json::from_slice(&release_json("0.2.9", MIN_UPDATE_BYTES, &name)).unwrap();
        release["body"] = serde_json::json!(body);
        serde_json::to_vec(&release).unwrap()
    }

    fn release_json(version: &str, size: u64, asset_name: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "tag_name": format!("v{version}"),
            "body": "Fixes",
            "assets": [{
                "name": asset_name,
                "browser_download_url": release_asset_url(version, asset_name),
                "size": size,
                "digest": format!("sha256:{}", "a".repeat(64)),
            }],
        }))
        .unwrap()
    }

    #[test]
    fn discovers_only_a_newer_exact_platform_asset_with_its_digest() {
        for platform in [UpdatePlatform::Windows, UpdatePlatform::Macos] {
            let name = platform.asset_name("0.2.9");
            let release = release_json("0.2.9", MIN_UPDATE_BYTES, &name);
            let update = discover_update("0.2.8", &release, platform)
                .unwrap()
                .unwrap();
            assert_eq!(update.version, "0.2.9");
            assert_eq!(update.asset_name, name);
            assert_eq!(update.sha256, "a".repeat(64));
            assert!(
                discover_update("0.2.9", &release, platform)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn rejects_portable_fallbacks_wrong_platform_sizes_and_foreign_urls() {
        for name in [
            "explorie-0.2.9-windows-x64-portable-unsigned.exe",
            &macos_dmg_name("0.2.9"),
        ] {
            assert!(
                discover_update(
                    "0.2.8",
                    &release_json("0.2.9", MIN_UPDATE_BYTES, name),
                    UpdatePlatform::Windows,
                )
                .is_err()
            );
        }
        for platform in [UpdatePlatform::Windows, UpdatePlatform::Macos] {
            let name = platform.asset_name("0.2.9");
            for size in [MIN_UPDATE_BYTES - 1, MAX_UPDATE_BYTES + 1] {
                assert!(
                    discover_update("0.2.8", &release_json("0.2.9", size, &name), platform)
                        .is_err()
                );
            }
            let mut update = discover_update(
                "0.2.8",
                &release_json("0.2.9", MIN_UPDATE_BYTES, &name),
                platform,
            )
            .unwrap()
            .unwrap();
            update.download_url = "https://example.com/update.exe".to_string();
            assert!(validate_update_info_for_platform(&update, platform).is_err());
        }
    }

    #[test]
    fn release_digest_is_required_and_must_be_sha256() {
        for platform in [UpdatePlatform::Windows, UpdatePlatform::Macos] {
            let name = platform.asset_name("0.2.9");
            let mut release: serde_json::Value =
                serde_json::from_slice(&release_json("0.2.9", MIN_UPDATE_BYTES, &name)).unwrap();
            for digest in [
                serde_json::Value::Null,
                serde_json::json!(""),
                serde_json::json!(format!("sha512:{}", "a".repeat(64))),
                serde_json::json!(format!("sha256:{}", "a".repeat(63))),
                serde_json::json!(format!("sha256:{}", "a".repeat(65))),
                serde_json::json!(format!("sha256:{}", "g".repeat(64))),
                serde_json::json!(format!("sha256:{} ", "a".repeat(64))),
            ] {
                release["assets"][0]["digest"] = digest;
                assert!(
                    discover_update("0.2.8", &serde_json::to_vec(&release).unwrap(), platform)
                        .is_err()
                );
            }
            release["assets"][0]
                .as_object_mut()
                .unwrap()
                .remove("digest");
            assert!(
                discover_update("0.2.8", &serde_json::to_vec(&release).unwrap(), platform).is_err()
            );
            release["assets"][0]["digest"] =
                serde_json::json!(format!("sha256:{}", "A".repeat(64)));
            assert_eq!(
                discover_update("0.2.8", &serde_json::to_vec(&release).unwrap(), platform)
                    .unwrap()
                    .unwrap()
                    .sha256,
                "a".repeat(64)
            );
        }
    }

    #[test]
    fn downloaded_payload_must_match_metadata_digest_and_size() {
        let temp = tempfile::tempdir().unwrap();
        let payload = b"fixture update payload";
        let correct_hash = format!("{:x}", Sha256::digest(payload));
        for (size, digest, accepted) in [
            (payload.len() as u64, correct_hash.as_str(), true),
            (payload.len() as u64, "a".repeat(64).as_str(), false),
            (payload.len() as u64 - 1, correct_hash.as_str(), false),
            (payload.len() as u64 + 1, correct_hash.as_str(), false),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/update", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0_u8; 4096];
                assert!(stream.read(&mut request).unwrap() > 0);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                )
                .unwrap();
                stream.write_all(payload).unwrap();
            });
            let path = temp.path().join("update.part");
            let result = download_installer(&url, &path, size, digest);
            server.join().unwrap();
            assert_eq!(result.is_ok(), accepted, "{result:?}");
            if accepted {
                assert_eq!(fs::read(&path).unwrap(), payload);
            }
        }
    }

    #[test]
    fn prepared_update_is_rehashed_immediately_before_launch() {
        let temp = tempfile::tempdir().unwrap();
        for platform in [UpdatePlatform::Windows, UpdatePlatform::Macos] {
            let cache = temp.path().join(platform.display_name());
            fs::create_dir_all(&cache).unwrap();
            let name = platform.asset_name("0.2.9");
            let path = cache.join(&name);
            fs::write(&path, vec![0_u8; MIN_UPDATE_BYTES as usize]).unwrap();
            let sha256 = hash_file(&path).unwrap();
            let mut update = DownloadedUpdate {
                info: UpdateInfo {
                    version: "0.2.9".to_string(),
                    notes: None,
                    asset_name: name.clone(),
                    download_url: release_asset_url("0.2.9", &name),
                    sha256: sha256.clone(),
                    size: MIN_UPDATE_BYTES,
                    signature: None,
                },
                installer_path: path.clone(),
                sha256,
            };
            assert!(validate_downloaded_update_for_platform(&cache, &update, platform).is_ok());
            fs::write(&path, vec![1_u8; MIN_UPDATE_BYTES as usize]).unwrap();
            assert!(validate_downloaded_update_for_platform(&cache, &update, platform).is_err());
            update.sha256 = hash_file(&path).unwrap();
            assert!(validate_downloaded_update_for_platform(&cache, &update, platform).is_err());
        }
    }

    #[test]
    fn checked_in_update_key_is_absent_or_valid() {
        parse_update_key(UPDATE_PUBLIC_KEY_FILE).unwrap();
        assert!(
            parse_update_key("untrusted comment: none\n\n")
                .unwrap()
                .is_none()
        );
        assert!(parse_update_key("untrusted comment: key\nnot-a-key\n").is_err());
        assert!(parse_update_key(&format!("{TEST_KEY}\n{OTHER_TEST_KEY}")).is_err());
        assert!(
            parse_update_key(&format!(
                "untrusted comment: minisign public key\n{TEST_KEY}\n"
            ))
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn unsigned_releases_follow_the_digest_only_path_without_a_key() {
        for platform in [UpdatePlatform::Windows, UpdatePlatform::Macos] {
            let update = discover_update(
                "0.2.8",
                &signed_release_json(platform, "Fixes\nexplorie-signature malformed"),
                platform,
            )
            .unwrap()
            .unwrap();
            assert_eq!(update.signature, None);
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join(&update.asset_name);
            fs::write(&path, b"any payload").unwrap();
            assert!(verify_update_signature(&path, &update, None).is_ok());
        }
    }

    #[test]
    fn signed_releases_require_a_valid_signature_for_the_exact_asset() {
        let key = test_key(TEST_KEY);
        let temp = tempfile::tempdir().unwrap();
        for platform in [UpdatePlatform::Windows, UpdatePlatform::Macos] {
            let name = platform.asset_name("0.2.9");
            let path = temp.path().join(&name);
            fs::write(&path, vec![0_u8; MIN_UPDATE_BYTES as usize]).unwrap();
            let body = format!(
                "## Changes\n\nFixes\n\n<!-- explorie-update-signatures\n{WINDOWS_SIGNATURE}\n{MACOS_SIGNATURE}\n-->\n"
            );
            let update = super::discover_update(
                "0.2.8",
                &signed_release_json(platform, &body),
                platform,
                Some(&key),
            )
            .unwrap()
            .unwrap();
            assert_eq!(update.notes.as_deref(), Some("## Changes\n\nFixes"));
            assert!(update.signature.is_some());
            verify_update_signature(&path, &update, Some(&key)).unwrap();

            // Tampered payload.
            let tampered = temp.path().join(format!("tampered-{name}"));
            let mut bytes = vec![0_u8; MIN_UPDATE_BYTES as usize];
            bytes[4096] = 1;
            fs::write(&tampered, bytes).unwrap();
            assert_eq!(
                verify_update_signature(&tampered, &update, Some(&key))
                    .unwrap_err()
                    .code,
                ErrorCode::PermissionDenied
            );

            // A key the release was not signed with.
            assert!(
                verify_update_signature(&path, &update, Some(&test_key(OTHER_TEST_KEY))).is_err()
            );

            // Missing signature: refused at discovery and again before use.
            assert_eq!(
                super::discover_update(
                    "0.2.8",
                    &signed_release_json(platform, "Fixes"),
                    platform,
                    Some(&key),
                )
                .unwrap_err()
                .code,
                ErrorCode::PermissionDenied
            );
            let unsigned = UpdateInfo {
                signature: None,
                ..update.clone()
            };
            assert!(verify_update_signature(&path, &unsigned, Some(&key)).is_err());
            assert!(require_signature(&unsigned, Some(&key)).is_err());

            // The same bytes signed for the other platform's asset name do not
            // verify: the trusted comment binds each signature to its asset.
            let other = match platform {
                UpdatePlatform::Windows => UpdatePlatform::Macos,
                UpdatePlatform::Macos => UpdatePlatform::Windows,
            };
            let replayed = signature_line(other).replacen(&other.asset_name("0.2.9"), &name, 1);
            let replayed = super::discover_update(
                "0.2.8",
                &signed_release_json(platform, &replayed),
                platform,
                Some(&key),
            )
            .unwrap()
            .unwrap();
            assert!(verify_update_signature(&path, &replayed, Some(&key)).is_err());

            // Malformed or duplicated signature lines are rejected outright.
            for body in [
                format!("explorie-signature {name} not-base64 still-not-base64"),
                format!("explorie-signature {name}"),
                format!("{}\n{}", signature_line(platform), signature_line(platform)),
            ] {
                assert!(
                    super::discover_update(
                        "0.2.8",
                        &signed_release_json(platform, &body),
                        platform,
                        Some(&key),
                    )
                    .is_err(),
                    "{body}"
                );
            }
        }

        let other_key = super::discover_update(
            "0.2.8",
            &signed_release_json(UpdatePlatform::Macos, OTHER_KEY_MACOS_SIGNATURE),
            UpdatePlatform::Macos,
            Some(&key),
        )
        .unwrap()
        .unwrap();
        let path = temp.path().join(UpdatePlatform::Macos.asset_name("0.2.9"));
        assert!(verify_update_signature(&path, &other_key, Some(&key)).is_err());

        let rsign_key = test_key(RSIGN_TEST_KEY);
        let rsign = super::discover_update(
            "0.2.8",
            &signed_release_json(UpdatePlatform::Macos, RSIGN_MACOS_SIGNATURE),
            UpdatePlatform::Macos,
            Some(&rsign_key),
        )
        .unwrap()
        .unwrap();
        verify_update_signature(&path, &rsign, Some(&rsign_key)).unwrap();
        assert!(
            verify_update_signature(&path, &other_key, Some(&test_key(OTHER_TEST_KEY))).is_ok()
        );
    }

    #[test]
    fn silent_installer_arguments_request_an_app_relaunch() {
        assert!(WINDOWS_INSTALLER_ARGUMENTS.contains(&"/VERYSILENT"));
        assert!(WINDOWS_INSTALLER_ARGUMENTS.contains(&"/CLOSEAPPLICATIONS"));
        assert!(WINDOWS_INSTALLER_ARGUMENTS.contains(&"/RELAUNCHEXPLORIE"));
    }
}
