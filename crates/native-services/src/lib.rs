//! Native application services shared by the GPUI desktop target.
//!
//! The host constructs a [`ServiceContext`], calls into [`NativeServices`],
//! and receives progress and results as [`ServiceEvent`] values.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Context, Poll, Waker};

pub mod archive;
pub mod audio;
mod blocking;
pub mod clipboard;
pub mod helper;
pub mod image_metadata;
pub mod integration;
pub mod listing;
pub mod metadata;
pub mod model_preview;
pub mod mutations;
mod plugin_detection;
pub mod plugins;
pub mod preview;
mod process;
pub mod remote_drives;
mod rich_markdown;
pub mod rich_preview;
pub mod search;
pub mod updater;
pub mod video;
pub mod watcher;

pub use archive::{ArchiveFormat, ArchiveInfo, CompressionLevel};
pub use archive::{ArchiveService, CompressRequest, CompressResult, ExtractRequest, ExtractResult};
pub use audio::{AudioBackend, AudioPlayback, AudioService, AudioStatus};
pub use blocking::{BlockingTask, CancellationToken, job_cancelled};
pub use explorie_core::{
    ConflictPolicy, FileEntry, FileOperationKind, FileOperationProgress, FileOperationRequest,
    FileOperationResult, FileTreeSnapshot,
};
pub use image_metadata::{
    ImageGps, ImageMetadata, format_exif_date, is_image_metadata_path, parse_image_metadata,
};
pub use integration::{
    AppInfo, FinderTagsBackend, InstallCleanupOffer, IntegrationService, PlatformActionsBackend,
    SystemIntegrationStatus,
};
pub use listing::{DirInfo, DiskInfo, ListRequest, ListResponse, SystemLocations, VolumeLocation};
pub use metadata::MetadataService;
pub use model_preview::{ModelCamera, ModelFrame, ModelPreview};
pub use mutations::{
    BatchRenameItem, BatchRenamePair, BatchRenameResult, MutationService, PermanentDeleteFailure,
    PermanentDeleteResult, SafeMutationRequest,
};
pub use plugins::{PluginManager, PluginResult, PluginService, PluginSource, PluginStatus};
pub use preview::{
    DetectedPreviewKind, HelperStatus, PdfPagePreview, PreviewArtifact, PreviewDetection,
    PreviewService, TextHighlight, TextHighlightKind, TextPreview,
};
pub use remote_drives::{
    DisconnectResult, HELPER_APPROVAL_MESSAGE, RemoteControlRequest, RemoteDriveBackend,
    RemoteDriveEnvironment, RemoteDriveExitBlocker, RemoteDriveProcess, RemoteDriveProfile,
    RemoteDriveService, RemoteDriveState, RemoteDriveStatus, RemoteMountRequest,
    RemoteProcessStatus, validate_remote_drive_profile,
};
pub use rich_preview::{RichBlock, RichBlockKind, RichPreview, RichSpan, RichSpanStyle};
pub use search::{
    CombineMode, SearchCriteria, SearchIndexHealth, SearchProgressEvent, SearchResult,
    SearchService, SearchSource, SearchType,
};
pub use updater::{DownloadedUpdate, UpdateInfo, UpdateService};
pub use video::{VideoBackend, VideoFrame, VideoPlayback, VideoService, VideoStatus};
pub use watcher::{WatchSubscription, WatcherService};

/// Stable classifications for errors crossing the native UI boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidInput,
    NotFound,
    PermissionDenied,
    Conflict,
    Cancelled,
    Unsupported,
    HelperMissing,
    RemoteUnavailable,
    Io,
    Busy,
    Internal,
}

/// A serializable, user-safe service error.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub operation: Option<String>,
}

impl ServiceError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            operation: None,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn operation(mut self, operation: impl Into<String>) -> Self {
        self.operation = Some(operation.into());
        self
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ServiceError {}

impl From<std::io::Error> for ServiceError {
    fn from(error: std::io::Error) -> Self {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            std::io::ErrorKind::AlreadyExists => ErrorCode::Conflict,
            std::io::ErrorKind::Interrupted => ErrorCode::Cancelled,
            std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData => {
                ErrorCode::InvalidInput
            }
            std::io::ErrorKind::Unsupported => ErrorCode::Unsupported,
            _ => ErrorCode::Io,
        };
        Self::new(code, error.to_string())
    }
}

pub type ServiceResult<T> = Result<T, ServiceError>;

/// Resource locations supplied by the host application.
///
/// The service layer never discovers these locations itself.  Packaged hosts inject
/// the resource and cache directories, while tests can point both at a
/// disposable fixture.
#[derive(Clone, Debug)]
pub struct ResourcePaths {
    pub resource_dir: Option<PathBuf>,
    pub cache_dir: PathBuf,
    pub config_dir: PathBuf,
    pub manifest_dir: PathBuf,
    pub app_version: String,
    pub current_exe: Option<PathBuf>,
}

impl ResourcePaths {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        let cache_dir = cache_dir.into();
        Self {
            resource_dir: None,
            cache_dir,
            config_dir: dirs::config_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("explorie"),
            manifest_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            app_version: String::new(),
            current_exe: std::env::current_exe().ok(),
        }
    }

    pub fn with_resource_dir(mut self, resource_dir: Option<PathBuf>) -> Self {
        self.resource_dir = resource_dir;
        self
    }

    pub fn with_config_dir(mut self, config_dir: impl Into<PathBuf>) -> Self {
        self.config_dir = config_dir.into();
        self
    }

    pub fn with_cache_dir(mut self, cache_dir: impl Into<PathBuf>) -> Self {
        self.cache_dir = cache_dir.into();
        self
    }

    pub fn with_manifest_dir(mut self, manifest_dir: impl Into<PathBuf>) -> Self {
        self.manifest_dir = manifest_dir.into();
        self
    }

    pub fn with_app_version(mut self, version: impl Into<String>) -> Self {
        self.app_version = version.into();
        self
    }

    pub fn with_current_exe(mut self, current_exe: Option<PathBuf>) -> Self {
        self.current_exe = current_exe;
        self
    }

    pub fn test(root: &Path) -> Self {
        Self::new(root.join("cache"))
            .with_config_dir(root.join("config"))
            .with_resource_dir(Some(root.join("resources")))
            .with_manifest_dir(root)
            .with_current_exe(Some(root.join("explorie-test")))
    }
}

impl Default for ResourcePaths {
    fn default() -> Self {
        let cache = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("explorie");
        Self::new(cache)
    }
}

/// A typed event stream shared by all service groups.
#[derive(Clone, Default)]
pub struct ServiceEvents {
    subscribers: Arc<Mutex<Vec<mpsc::Sender<ServiceEvent>>>>,
    async_subscribers: Arc<Mutex<Vec<std::sync::Weak<ServiceEventQueue>>>>,
}

impl ServiceEvents {
    pub fn subscribe(&self) -> mpsc::Receiver<ServiceEvent> {
        let (sender, receiver) = mpsc::channel();
        self.subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(sender);
        receiver
    }

    pub fn subscribe_async(&self) -> ServiceSubscription {
        let queue = Arc::new(ServiceEventQueue::default());
        self.async_subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::downgrade(&queue));
        ServiceSubscription { queue }
    }

    pub(crate) fn publish(&self, event: ServiceEvent) {
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
        let mut async_subscribers = self
            .async_subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        async_subscribers.retain(|subscriber| {
            let Some(queue) = subscriber.upgrade() else {
                return false;
            };
            queue.publish(event.clone());
            true
        });
    }
}

#[derive(Default)]
struct ServiceEventQueue {
    events: Mutex<VecDeque<ServiceEvent>>,
    waker: Mutex<Option<Waker>>,
}

impl ServiceEventQueue {
    fn publish(&self, event: ServiceEvent) {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut event = Some(event);
        if let Some(ServiceEvent::PluginStatusChanged { id, contribution }) = event.as_ref() {
            for queued in events.iter_mut().rev() {
                if let ServiceEvent::PluginStatusChanged {
                    id: pending_id,
                    contribution: pending,
                } = queued
                    && pending_id == id
                    && pending.path == contribution.path
                    && pending.context_id == contribution.context_id
                    && pending.generation == contribution.generation
                {
                    if let Some(replacement) = event.take() {
                        *queued = replacement;
                    }
                    break;
                }
            }
        }
        if let Some(ServiceEvent::FileOperation(incoming)) = event.as_ref()
            && matches!(incoming.state, FileOperationState::Running)
        {
            for queued in events.iter_mut().rev() {
                let ServiceEvent::FileOperation(pending) = queued else {
                    continue;
                };
                if pending.job_id != incoming.job_id {
                    continue;
                }
                if matches!(pending.state, FileOperationState::Running)
                    && let Some(replacement) = event.take()
                {
                    *queued = replacement;
                }
                break;
            }
        }
        if let Some(ServiceEvent::ArchiveProgress(incoming)) = event.as_ref() {
            for queued in events.iter_mut().rev() {
                let ServiceEvent::ArchiveProgress(pending) = queued else {
                    continue;
                };
                if pending.operation_id == incoming.operation_id {
                    if let Some(replacement) = event.take() {
                        *queued = replacement;
                    }
                    break;
                }
            }
        }
        if let Some(ServiceEvent::SearchProgress(incoming)) = event.as_ref() {
            for queued in events.iter_mut().rev() {
                let ServiceEvent::SearchProgress(pending) = queued else {
                    continue;
                };
                if pending.request_id == incoming.request_id
                    && pending.entries.is_empty()
                    && incoming.entries.is_empty()
                {
                    if let Some(replacement) = event.take() {
                        *queued = replacement;
                    }
                    break;
                }
            }
        }
        if let Some(event) = event {
            events.push_back(event);
        }
        drop(events);
        if let Some(waker) = self
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            waker.wake();
        }
    }
}

pub struct ServiceSubscription {
    queue: Arc<ServiceEventQueue>,
}

pub struct ServiceNext<'a> {
    subscription: &'a ServiceSubscription,
}

impl ServiceSubscription {
    pub fn next(&self) -> ServiceNext<'_> {
        ServiceNext { subscription: self }
    }
}

impl Future for ServiceNext<'_> {
    type Output = ServiceEvent;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let queue = &self.subscription.queue;
        if let Some(event) = queue
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
        {
            return Poll::Ready(event);
        }
        *queue
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(context.waker().clone());
        if let Some(event) = queue
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
        {
            Poll::Ready(event)
        } else {
            Poll::Pending
        }
    }
}

/// Typed events emitted by native jobs and subscriptions.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ServiceEvent {
    PluginStatusChanged {
        id: String,
        contribution: explorie_plugin_protocol::Contribution,
    },
    PluginRegistryChanged,
    FileOperation(FileOperationEvent),
    ArchiveProgress(ArchiveProgressEvent),
    SearchProgress(SearchProgressEvent),
    RemoteDriveStatus(RemoteDriveStatus),
    RemoteDriveExitBlocked(RemoteDriveExitBlocker),
    MutationIdle,
    HelperStatus(HelperStatusEvent),
    Watcher(WatcherEvent),
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationEvent {
    pub job_id: String,
    pub state: FileOperationState,
    pub progress: Option<explorie_core::FileOperationProgress>,
    pub result: Option<explorie_core::FileOperationResult>,
    #[serde(default)]
    pub retryable_sources: Vec<PathBuf>,
    pub error: Option<ServiceError>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileOperationState {
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveProgressEvent {
    pub operation_id: String,
    pub processed_bytes: u64,
    pub total_bytes: u64,
    pub current_path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelperStatusEvent {
    pub helper: String,
    pub status: HelperStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatcherEvent {
    pub registration_id: u64,
    pub state: WatcherState,
    pub paths: Vec<PathBuf>,
    pub error: Option<ServiceError>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WatcherState {
    Changed,
    Failed,
    Stopped,
}

// Worker, preview-decoder, and archive callbacks rely on `catch_unwind` to turn
// panics from untrusted input into errors. `panic = "abort"` would silently turn
// every one of them into a whole-app crash, so refuse to build that way.
#[cfg(not(panic = "unwind"))]
compile_error!(
    "explorie-native-services requires panic = \"unwind\"; its catch_unwind crash guards do nothing under panic = \"abort\""
);

/// Host context shared by all native services.
#[derive(Clone)]
pub struct ServiceContext {
    resources: Arc<ResourcePaths>,
    events: ServiceEvents,
}

impl ServiceContext {
    pub fn new(resources: ResourcePaths) -> Self {
        Self {
            resources: Arc::new(resources),
            events: ServiceEvents::default(),
        }
    }

    pub fn resources(&self) -> &ResourcePaths {
        &self.resources
    }

    pub fn events(&self) -> ServiceEvents {
        self.events.clone()
    }

    pub fn subscribe(&self) -> mpsc::Receiver<ServiceEvent> {
        self.events.subscribe()
    }

    pub fn subscribe_async(&self) -> ServiceSubscription {
        self.events.subscribe_async()
    }

    /// Run blocking work on the shared worker pool. The job runs to
    /// completion even if the returned task is dropped; see
    /// [`BlockingTask::cancel_on_drop`] and [`Self::spawn_cancellable`] for work
    /// that may be abandoned.
    pub fn spawn_blocking<T, F>(&self, operation: F) -> BlockingTask<T>
    where
        T: Send + 'static,
        F: FnOnce() -> ServiceResult<T> + Send + 'static,
    {
        BlockingTask::spawn(blocking::BlockingPool::global(), operation)
    }

    /// Run side-effect-free blocking work that stops being useful once its
    /// caller loses interest. Dropping the task skips the job if it is still
    /// queued and cancels the token it receives if it is running.
    pub fn spawn_cancellable<T, F>(&self, operation: F) -> BlockingTask<T>
    where
        T: Send + 'static,
        F: FnOnce(&CancellationToken) -> ServiceResult<T> + Send + 'static,
    {
        BlockingTask::spawn_cancellable(blocking::BlockingPool::global(), operation)
    }
}

impl Default for ServiceContext {
    fn default() -> Self {
        Self::new(ResourcePaths::default())
    }
}

pub(crate) struct SharedState {
    pub events: ServiceEvents,
    pub remote_roots: Mutex<HashSet<String>>,
    pub next_job_id: AtomicU64,
    pub cancellations: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub active_mutations: AtomicU64,
    pub exit_requested: AtomicBool,
}

impl SharedState {
    fn new(events: ServiceEvents) -> Self {
        Self {
            events,
            remote_roots: Mutex::new(HashSet::new()),
            next_job_id: AtomicU64::new(0),
            cancellations: Mutex::new(HashMap::new()),
            active_mutations: AtomicU64::new(0),
            exit_requested: AtomicBool::new(false),
        }
    }
}

/// Keeps the process from exiting while a native mutation is in flight.
///
/// The guard is intentionally shared by all mutating service groups, not just
/// file-operation jobs. Metadata, archive, integration, and remote-drive
/// writes all need the same exit-blocking semantics.
pub(crate) struct ActiveOperation {
    shared: Arc<SharedState>,
}

impl ActiveOperation {
    pub(crate) fn new(shared: Arc<SharedState>) -> Self {
        shared.active_mutations.fetch_add(1, Ordering::AcqRel);
        Self { shared }
    }
}

impl Drop for ActiveOperation {
    fn drop(&mut self) {
        if self.shared.active_mutations.fetch_sub(1, Ordering::AcqRel) == 1
            && self.shared.exit_requested.swap(false, Ordering::AcqRel)
        {
            self.shared.events.publish(ServiceEvent::MutationIdle);
        }
    }
}

/// All native capabilities exposed as one GPUI-friendly service context.
#[derive(Clone)]
pub struct NativeServices {
    pub context: ServiceContext,
    pub audio: audio::AudioService,
    pub video: video::VideoService,
    pub listing: listing::ListingService,
    pub metadata: metadata::MetadataService,
    pub mutations: mutations::MutationService,
    pub archives: archive::ArchiveService,
    pub previews: preview::PreviewService,
    pub remotes: remote_drives::RemoteDriveService,
    pub search: search::SearchService,
    pub updater: updater::UpdateService,
    pub integration: integration::IntegrationService,
    pub plugins: plugins::PluginService,
    pub watcher: watcher::WatcherService,
}

impl NativeServices {
    pub fn new(resources: ResourcePaths) -> Self {
        let context = ServiceContext::new(resources);
        Self::from_context(context)
    }

    pub fn from_context(context: ServiceContext) -> Self {
        Self::from_context_with_backends(context, None, None, None, None, None)
    }

    /// Create a window-local service facade. Process-wide resources and
    /// mutation/remote state remain shared, while preview playback and search
    /// cancellation cannot interfere with another window.
    pub fn fork_window_scope(&self) -> Self {
        let mut services = self.clone();
        services.audio = self.audio.fork_playback_scope();
        services.video = self.video.fork_playback_scope();
        services.previews = self.previews.fork_cancellation_scope();
        services.search = self.search.fork_cancellation_scope();
        services
    }

    /// Construct services with a host-provided remote-drive backend.
    ///
    /// Production uses the system backend. Tests can inject process/helper
    /// implementations without changing platform safety checks or relying on
    /// process-wide environment switches.
    pub fn with_remote_backend(
        resources: ResourcePaths,
        backend: Arc<dyn remote_drives::RemoteDriveBackend>,
    ) -> Self {
        Self::from_context_with_backends(
            ServiceContext::new(resources),
            Some(backend),
            None,
            None,
            None,
            None,
        )
    }

    /// Construct services with an injected audio backend. This keeps rendered
    /// UI and service tests independent of the host's physical output device.
    pub fn with_audio_backend(
        resources: ResourcePaths,
        backend: Arc<dyn audio::AudioBackend>,
    ) -> Self {
        Self::from_context_with_backends(
            ServiceContext::new(resources),
            None,
            Some(backend),
            None,
            None,
            None,
        )
    }

    /// Construct services with an injected video backend. Rendered GPUI tests
    /// use this path so they do not depend on FFmpeg, a display codec, or an
    /// audio device.
    pub fn with_video_backend(
        resources: ResourcePaths,
        backend: Arc<dyn video::VideoBackend>,
    ) -> Self {
        Self::from_context_with_backends(
            ServiceContext::new(resources),
            None,
            None,
            Some(backend),
            None,
            None,
        )
    }

    pub fn with_finder_tags_backend(
        resources: ResourcePaths,
        backend: Arc<dyn integration::FinderTagsBackend>,
    ) -> Self {
        Self::from_context_with_backends(
            ServiceContext::new(resources),
            None,
            None,
            None,
            Some(backend),
            None,
        )
    }

    pub fn with_platform_actions_backend(
        resources: ResourcePaths,
        backend: Arc<dyn integration::PlatformActionsBackend>,
    ) -> Self {
        Self::from_context_with_backends(
            ServiceContext::new(resources),
            None,
            None,
            None,
            None,
            Some(backend),
        )
    }

    fn from_context_with_backends(
        context: ServiceContext,
        remote_backend: Option<Arc<dyn remote_drives::RemoteDriveBackend>>,
        audio_backend: Option<Arc<dyn audio::AudioBackend>>,
        video_backend: Option<Arc<dyn video::VideoBackend>>,
        finder_tags_backend: Option<Arc<dyn integration::FinderTagsBackend>>,
        platform_actions_backend: Option<Arc<dyn integration::PlatformActionsBackend>>,
    ) -> Self {
        let shared = Arc::new(SharedState::new(context.events()));
        Self {
            audio: match audio_backend {
                Some(backend) => audio::AudioService::with_backend(context.clone(), backend),
                None => audio::AudioService::new(context.clone()),
            },
            video: match video_backend {
                Some(backend) => video::VideoService::with_backend(context.clone(), backend),
                None => video::VideoService::new(context.clone()),
            },
            listing: listing::ListingService::new(context.clone(), Arc::clone(&shared)),
            metadata: metadata::MetadataService::new(context.clone(), Arc::clone(&shared)),
            mutations: mutations::MutationService::new(context.clone(), Arc::clone(&shared)),
            archives: archive::ArchiveService::new(context.clone(), Arc::clone(&shared)),
            previews: preview::PreviewService::new(context.clone()),
            remotes: match remote_backend {
                Some(backend) => remote_drives::RemoteDriveService::with_backend(
                    context.clone(),
                    Arc::clone(&shared),
                    backend,
                ),
                None => {
                    remote_drives::RemoteDriveService::new(context.clone(), Arc::clone(&shared))
                }
            },
            search: search::SearchService::new(context.clone()),
            updater: updater::UpdateService::new(context.clone()),
            integration: match (finder_tags_backend, platform_actions_backend) {
                (Some(backend), None) => integration::IntegrationService::with_finder_tags_backend(
                    context.clone(),
                    Arc::clone(&shared),
                    backend,
                ),
                (None, Some(backend)) => {
                    integration::IntegrationService::with_platform_actions_backend(
                        context.clone(),
                        Arc::clone(&shared),
                        backend,
                    )
                }
                (None, None) => {
                    integration::IntegrationService::new(context.clone(), Arc::clone(&shared))
                }
                (Some(finder_tags), Some(platform_actions)) => {
                    integration::IntegrationService::with_backends(
                        context.clone(),
                        Arc::clone(&shared),
                        finder_tags,
                        platform_actions,
                    )
                }
            },
            watcher: watcher::WatcherService::new(context.clone()),
            plugins: plugins::PluginService::new(context.clone(), Vec::new()),
            context,
        }
    }

    pub fn subscribe(&self) -> mpsc::Receiver<ServiceEvent> {
        self.context.subscribe()
    }

    pub fn subscribe_async(&self) -> ServiceSubscription {
        self.context.subscribe_async()
    }

    pub fn resources(&self) -> &ResourcePaths {
        self.context.resources()
    }
}

impl Default for NativeServices {
    fn default() -> Self {
        Self::new(ResourcePaths::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    struct WakeSignal(AtomicBool);

    impl std::task::Wake for WakeSignal {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn blocking_task_can_be_awaited_without_joining_the_calling_thread() {
        let context = ServiceContext::default();
        let mut task = context.spawn_blocking(|| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            Ok(42_u64)
        });
        let signal = Arc::new(WakeSignal(AtomicBool::new(false)));
        let waker = Waker::from(Arc::clone(&signal));
        let mut task = Pin::new(&mut task);
        let mut task_context = Context::from_waker(&waker);

        loop {
            match Future::poll(task.as_mut(), &mut task_context) {
                Poll::Ready(result) => {
                    assert_eq!(result.unwrap(), 42);
                    break;
                }
                Poll::Pending => {
                    while !signal.0.swap(false, Ordering::AcqRel) {
                        std::thread::yield_now();
                    }
                }
            }
        }
    }

    #[test]
    fn service_events_have_an_awaitable_native_subscription() {
        let events = ServiceEvents::default();
        let subscription = events.subscribe_async();
        events.publish(ServiceEvent::MutationIdle);

        assert!(matches!(
            pollster::block_on(subscription.next()),
            ServiceEvent::MutationIdle
        ));
    }

    #[test]
    fn async_service_events_coalesce_progress_without_dropping_terminal_state() {
        let queue = ServiceEventQueue::default();
        let progress_event = |processed_bytes| {
            ServiceEvent::FileOperation(FileOperationEvent {
                job_id: "job".into(),
                state: FileOperationState::Running,
                progress: Some(explorie_core::FileOperationProgress {
                    processed_entries: 0,
                    total_entries: 1,
                    processed_bytes,
                    total_bytes: 10,
                    current_path: Some(PathBuf::from("source")),
                }),
                result: None,
                retryable_sources: Vec::new(),
                error: None,
            })
        };
        queue.publish(progress_event(1));
        queue.publish(progress_event(5));
        queue.publish(progress_event(9));
        queue.publish(ServiceEvent::FileOperation(FileOperationEvent {
            job_id: "job".into(),
            state: FileOperationState::Completed,
            progress: None,
            result: Some(explorie_core::FileOperationResult {
                processed_entries: 1,
                processed_bytes: 10,
                targets: vec![PathBuf::from("destination")],
                target_snapshots: Vec::new(),
            }),
            retryable_sources: Vec::new(),
            error: None,
        }));

        let events = queue
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            ServiceEvent::FileOperation(FileOperationEvent {
                state: FileOperationState::Running,
                progress: Some(progress),
                ..
            }) if progress.processed_bytes == 9
        ));
        assert!(matches!(
            &events[1],
            ServiceEvent::FileOperation(FileOperationEvent {
                state: FileOperationState::Completed,
                ..
            })
        ));

        drop(events);
        queue.publish(ServiceEvent::ArchiveProgress(ArchiveProgressEvent {
            operation_id: "archive".into(),
            processed_bytes: 1,
            total_bytes: 10,
            current_path: "one".into(),
        }));
        queue.publish(ServiceEvent::ArchiveProgress(ArchiveProgressEvent {
            operation_id: "archive".into(),
            processed_bytes: 9,
            total_bytes: 10,
            current_path: "nine".into(),
        }));
        let events = queue
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(events.len(), 3);
        assert!(matches!(
            &events[2],
            ServiceEvent::ArchiveProgress(ArchiveProgressEvent {
                processed_bytes: 9,
                ..
            })
        ));

        let search_queue = ServiceEventQueue::default();
        let entry = explorie_core::FileEntry {
            id: uuid::Uuid::new_v4(),
            path: PathBuf::from("result.txt"),
            size: 1,
            modified: std::time::UNIX_EPOCH,
            hidden: false,
            is_dir: false,
            custom: HashMap::new(),
            is_symlink: false,
            is_junction: false,
            link_target: None,
            has_xattrs: false,
            is_package: false,
            link_target_is_dir: false,
            is_cloud_placeholder: false,
            tags: Vec::new(),
        };
        search_queue.publish(ServiceEvent::SearchProgress(SearchProgressEvent {
            request_id: "search".into(),
            phase: "results".into(),
            indexed_entries: 128,
            matched_entries: 1,
            current_path: entry.path.clone(),
            entries: vec![entry],
        }));
        search_queue.publish(ServiceEvent::SearchProgress(SearchProgressEvent {
            request_id: "search".into(),
            phase: "matching".into(),
            indexed_entries: 512,
            matched_entries: 1,
            current_path: PathBuf::from("later.txt"),
            entries: Vec::new(),
        }));
        let search_events = search_queue
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(search_events.len(), 2);
        assert!(matches!(
            &search_events[0],
            ServiceEvent::SearchProgress(SearchProgressEvent { entries, .. })
                if entries.len() == 1
        ));
    }

    #[test]
    fn native_result_wire_names_match_existing_desktop_consumers() {
        let disk = serde_json::to_value(DiskInfo {
            mount_point: "C:\\".into(),
            total_space: 10,
            available_space: 5,
            name: "System".into(),
        })
        .unwrap();
        assert!(disk.get("mount_point").is_some());
        assert!(disk.get("total_space").is_some());
        assert!(disk.get("available_space").is_some());

        let artifact = serde_json::to_value(PreviewArtifact {
            kind: "pdf".into(),
            path: PathBuf::from("preview.pdf"),
            mime_type: "application/pdf".into(),
            tool: "soffice".into(),
        })
        .unwrap();
        assert!(artifact.get("mime_type").is_some());

        let app = serde_json::to_value(AppInfo {
            name: "Preview".into(),
            path: PathBuf::from("/Applications/Preview.app"),
            bundle_id: Some("com.apple.Preview".into()),
            is_default: true,
        })
        .unwrap();
        assert!(app.get("bundle_id").is_some());
        assert_eq!(app.get("is_default"), Some(&serde_json::Value::Bool(true)));
        let legacy: AppInfo = serde_json::from_value(serde_json::json!({
            "name": "Preview",
            "path": "/Applications/Preview.app",
            "bundle_id": null
        }))
        .unwrap();
        assert!(!legacy.is_default);

        let archive = serde_json::to_value(CompressResult {
            output_path: PathBuf::from("bundle.zip"),
            total_bytes: 10,
        })
        .unwrap();
        assert!(archive.get("output_path").is_some());
        assert!(archive.get("total_bytes").is_some());

        let operation = serde_json::to_value(FileOperationEvent {
            job_id: "job".into(),
            state: FileOperationState::Failed,
            progress: None,
            result: None,
            retryable_sources: vec![PathBuf::from("unresolved.txt")],
            error: None,
        })
        .unwrap();
        assert!(operation.get("retryableSources").is_some());
    }

    #[test]
    fn credential_bearing_requests_redact_passwords_in_debug_output() {
        let secret = "do-not-print-this";
        let archive = CompressRequest {
            paths: vec![PathBuf::from("source.txt")],
            output_path: PathBuf::from("bundle.zip"),
            format: ArchiveFormat::Zip,
            compression_level: CompressionLevel::Normal,
            password: Some(secret.into()),
            operation_id: "job".into(),
        };
        assert!(!format!("{archive:?}").contains(secret));

        let extract = ExtractRequest {
            archive_path: PathBuf::from("bundle.zip"),
            output_dir: PathBuf::from("output"),
            password: Some(secret.into()),
            allow_extended_limits: false,
            operation_id: "extract-job".into(),
        };
        assert!(!format!("{extract:?}").contains(secret));

        let remote = RemoteMountRequest {
            profile: RemoteDriveProfile {
                id: "profile".into(),
                name: "Remote".into(),
                remote: "remote".into(),
                remote_path: String::new(),
                mount_target: "D:".into(),
            },
            rclone: PathBuf::from("rclone"),
            cache_dir: PathBuf::from("cache"),
            rc_url: "http://127.0.0.1:1/".into(),
            rc_user: "explorie".into(),
            rc_pass: secret.into(),
            helper_port: None,
        };
        assert!(!format!("{remote:?}").contains(secret));

        let control = RemoteControlRequest {
            rclone: PathBuf::from("rclone"),
            rc_url: "http://127.0.0.1:1/".into(),
            rc_user: "explorie".into(),
            rc_pass: secret.into(),
            endpoint: "core/quit".into(),
        };
        assert!(!format!("{control:?}").contains(secret));
    }
}
