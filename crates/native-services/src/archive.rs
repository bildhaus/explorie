use crate::{
    ActiveOperation, ArchiveProgressEvent, BlockingTask, ErrorCode, ServiceContext, ServiceError,
    ServiceEvent, SharedState,
};
use explorie_core::archive::{self, CompressOptions};
pub use explorie_core::archive::{ArchiveFormat, ArchiveInfo, CompressionLevel};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressRequest {
    pub paths: Vec<PathBuf>,
    pub output_path: PathBuf,
    pub format: ArchiveFormat,
    pub compression_level: CompressionLevel,
    #[serde(default)]
    pub password: Option<String>,
    pub operation_id: String,
}

impl fmt::Debug for CompressRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompressRequest")
            .field("paths", &self.paths)
            .field("output_path", &self.output_path)
            .field("format", &self.format)
            .field("compression_level", &self.compression_level)
            .field("password", &self.password.as_ref().map(|_| "[redacted]"))
            .field("operation_id", &self.operation_id)
            .finish()
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractRequest {
    pub archive_path: PathBuf,
    pub output_dir: PathBuf,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub allow_extended_limits: bool,
    pub operation_id: String,
}

impl fmt::Debug for ExtractRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExtractRequest")
            .field("archive_path", &self.archive_path)
            .field("output_dir", &self.output_dir)
            .field("password", &self.password.as_ref().map(|_| "[redacted]"))
            .field("allow_extended_limits", &self.allow_extended_limits)
            .field("operation_id", &self.operation_id)
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CompressResult {
    pub output_path: PathBuf,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ExtractResult {
    pub output_dir: PathBuf,
    pub total_bytes: u64,
}

#[derive(Clone)]
pub struct ArchiveService {
    context: ServiceContext,
    shared: Arc<SharedState>,
}

impl ArchiveService {
    pub(crate) fn new(context: ServiceContext, shared: Arc<SharedState>) -> Self {
        Self { context, shared }
    }

    pub fn compress(&self, request: CompressRequest) -> BlockingTask<CompressResult> {
        let context = self.context.clone();
        let shared = Arc::clone(&self.shared);
        let guard = ActiveOperation::new(Arc::clone(&shared));
        self.context.spawn_blocking(move || {
            if request.paths.iter().any(|path| remote_root(&shared, path))
                || remote_root(&shared, &request.output_path)
            {
                return Err(ServiceError::new(
                    ErrorCode::RemoteUnavailable,
                    "Refusing to mutate a managed remote-drive root",
                ));
            }
            let _guard = guard;
            let operation_id = request.operation_id.clone();
            let total_bytes = archive::create_archive_with_progress(
                &request.paths,
                &request.output_path,
                &CompressOptions {
                    format: request.format,
                    compression_level: request.compression_level,
                    password: request.password,
                },
                |progress| {
                    context
                        .events()
                        .publish(ServiceEvent::ArchiveProgress(ArchiveProgressEvent {
                            operation_id: operation_id.clone(),
                            processed_bytes: progress.processed_bytes,
                            total_bytes: progress.total_bytes,
                            current_path: progress.current_path,
                        }));
                },
            )
            .map_err(ServiceError::from)?;
            Ok(CompressResult {
                output_path: request.output_path,
                total_bytes,
            })
        })
    }

    pub fn extract(&self, request: ExtractRequest) -> BlockingTask<ExtractResult> {
        let shared = Arc::clone(&self.shared);
        let guard = ActiveOperation::new(Arc::clone(&shared));
        let cancelled = Arc::new(AtomicBool::new(false));
        shared
            .cancellations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                archive_cancellation_key(&request.operation_id),
                Arc::clone(&cancelled),
            );
        self.context.spawn_blocking(move || {
            let _guard = guard;
            let result = (|| {
                if remote_root(&shared, &request.output_dir) {
                    return Err(ServiceError::new(
                        ErrorCode::RemoteUnavailable,
                        "Refusing to mutate a managed remote-drive root",
                    ));
                }
                let mut limits = if request.allow_extended_limits {
                    archive::ExtractionLimits::extended()
                } else {
                    archive::ExtractionLimits::default()
                };
                limits.max_available_bytes = extraction_available_bytes(&request.output_dir)?;
                let total_bytes = archive::extract_archive_with_password_and_limits(
                    &request.archive_path,
                    &request.output_dir,
                    request.password.as_deref(),
                    limits,
                    &cancelled,
                )
                .map_err(|error| {
                    let can_override = !request.allow_extended_limits
                        && error.kind() == std::io::ErrorKind::InvalidData
                        && error.to_string().contains("safety limit");
                    ServiceError::from(error).retryable(can_override)
                })?;
                Ok(ExtractResult {
                    output_dir: request.output_dir,
                    total_bytes,
                })
            })();
            shared
                .cancellations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&archive_cancellation_key(&request.operation_id));
            result
        })
    }

    pub fn list(&self, archive_path: PathBuf) -> BlockingTask<ArchiveInfo> {
        self.context.spawn_blocking(move || {
            archive::list_archive_contents(&archive_path).map_err(ServiceError::from)
        })
    }

    pub fn cancel(&self, operation_id: &str) -> bool {
        let cancellations = self
            .shared
            .cancellations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cancelled) = cancellations.get(&archive_cancellation_key(operation_id)) {
            cancelled.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }

    pub fn is_archive(&self, path: PathBuf) -> BlockingTask<bool> {
        self.context
            .spawn_blocking(move || Ok(archive::is_archive(Path::new(&path))))
    }
}

fn archive_cancellation_key(operation_id: &str) -> String {
    format!("archive:{operation_id}")
}

fn extraction_available_bytes(output_dir: &Path) -> Result<u64, ServiceError> {
    const MIN_RESERVED_BYTES: u64 = 64 * 1024 * 1024;
    const MAX_RESERVED_BYTES: u64 = 512 * 1024 * 1024;
    let check_path = output_dir.parent().unwrap_or(output_dir);
    let check_path = if check_path.is_absolute() {
        check_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(ServiceError::from)?
            .join(check_path)
    };
    let check_path = fs::canonicalize(&check_path).unwrap_or(check_path);
    let check_key = disk_path_key(&check_path);
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let disk = disks
        .iter()
        .filter(|disk| check_key.starts_with(&disk_path_key(disk.mount_point())))
        .max_by_key(|disk| disk_path_key(disk.mount_point()).len())
        .ok_or_else(|| {
            ServiceError::new(
                ErrorCode::Io,
                "Could not determine free space for archive extraction",
            )
        })?;
    let free = disk.available_space();
    let reserved = (free / 10).clamp(MIN_RESERVED_BYTES, MAX_RESERVED_BYTES);
    let available = free.saturating_sub(reserved);
    if available == 0 {
        return Err(ServiceError::new(
            ErrorCode::Io,
            "At least 64 MiB of free disk space must remain available during extraction",
        ));
    }
    Ok(available)
}

fn disk_path_key(path: &Path) -> String {
    crate::listing::normalize_path(path)
        .trim_start_matches("//?/")
        .to_string()
}

fn remote_root(shared: &SharedState, path: &Path) -> bool {
    shared
        .remote_roots
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&crate::listing::normalize_path(path))
}
