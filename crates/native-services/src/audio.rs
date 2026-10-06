//! Local audio playback owned by the native desktop runtime.
//!
//! Decoding and output use rodio, behind the `preview-audio` cargo feature.
//! Without it the service still validates requests, but every load fails with
//! an Unsupported error.

#[cfg(feature = "preview-audio")]
use std::fs::File;
use std::path::{Path, PathBuf};
#[cfg(feature = "preview-audio")]
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(feature = "preview-audio")]
use std::time::UNIX_EPOCH;

#[cfg(feature = "preview-audio")]
use rodio::{Decoder, DeviceSinkBuilder, Player, Source};
#[cfg(feature = "preview-audio")]
use sha2::{Digest, Sha256};

#[cfg(feature = "preview-audio")]
use crate::process::{ProcessError, run_with_timeout};
use crate::{BlockingTask, ErrorCode, ServiceContext, ServiceError, ServiceResult};

#[cfg(any(feature = "preview-audio", test))]
const DEFAULT_VOLUME: f32 = 0.8;
#[cfg(feature = "preview-audio")]
const AUDIO_TRANSCODE_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_AUDIO_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
#[cfg(feature = "preview-audio")]
const MAX_TRANSCODE_CACHE_ENTRIES: usize = 8;
#[cfg(feature = "preview-audio")]
const MAX_TRANSCODE_CACHE_BYTES: u64 = 256 * 1024 * 1024;

/// Snapshot consumed by the GPUI controls. Durations are milliseconds so the
/// boundary remains serializable and independent of the playback library.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioStatus {
    pub path: PathBuf,
    pub duration_ms: Option<u64>,
    pub position_ms: u64,
    pub playing: bool,
    pub finished: bool,
    pub volume: f32,
}

/// One decoded playback session. Implementations must remain safe to query
/// from the UI while the audio callback advances on its own thread.
pub trait AudioPlayback: Send + Sync {
    fn duration(&self) -> Option<Duration>;
    fn position(&self) -> Duration;
    fn is_paused(&self) -> bool;
    fn is_empty(&self) -> bool;
    fn play(&self);
    fn pause(&self);
    fn stop(&self);
    fn seek(&self, position: Duration) -> ServiceResult<()>;
    fn volume(&self) -> f32;
    fn set_volume(&self, volume: f32);
}

/// Decoder/output factory. Public so tests and alternate hosts can provide a
/// deterministic output device without changing production code paths.
pub trait AudioBackend: Send + Sync {
    fn open(&self, path: &Path) -> ServiceResult<Arc<dyn AudioPlayback>>;
}

#[cfg(feature = "preview-audio")]
struct RodioBackend {
    cache_dir: PathBuf,
    transcode_lock: Mutex<()>,
}

#[cfg(feature = "preview-audio")]
struct RodioPlayback {
    _device: rodio::MixerDeviceSink,
    player: Player,
    duration: Option<Duration>,
}

#[cfg(feature = "preview-audio")]
impl AudioBackend for RodioBackend {
    fn open(&self, path: &Path) -> ServiceResult<Arc<dyn AudioPlayback>> {
        match open_rodio_playback(path) {
            Ok(playback) => Ok(playback),
            Err(_) if matches!(audio_extension(path).as_str(), "opus" | "oga") => {
                let _guard = self
                    .transcode_lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let transcoded = transcode_opus(path, &self.cache_dir)?;
                open_rodio_playback(&transcoded)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(feature = "preview-audio")]
fn open_rodio_playback(path: &Path) -> ServiceResult<Arc<dyn AudioPlayback>> {
    let file = File::open(path).map_err(|error| {
        ServiceError::from(error).operation(format!("open audio {}", path.display()))
    })?;
    let decoder = Decoder::try_from(file).map_err(|error| {
        ServiceError::new(
            ErrorCode::Unsupported,
            format!("Unable to decode audio: {error}"),
        )
        .operation(format!("decode audio {}", path.display()))
    })?;
    let duration = decoder.total_duration();
    let device = DeviceSinkBuilder::open_default_sink().map_err(|error| {
        ServiceError::new(
            ErrorCode::RemoteUnavailable,
            format!("No usable audio output device is available: {error}"),
        )
        .retryable(true)
        .operation("open default audio output")
    })?;
    let player = Player::connect_new(device.mixer());
    player.set_volume(DEFAULT_VOLUME);
    player.append(decoder);
    player.pause();
    Ok(Arc::new(RodioPlayback {
        _device: device,
        player,
        duration,
    }))
}

#[cfg(feature = "preview-audio")]
impl AudioPlayback for RodioPlayback {
    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn position(&self) -> Duration {
        self.player.get_pos()
    }

    fn is_paused(&self) -> bool {
        self.player.is_paused()
    }

    fn is_empty(&self) -> bool {
        self.player.empty()
    }

    fn play(&self) {
        self.player.play();
    }

    fn pause(&self) {
        self.player.pause();
    }

    fn stop(&self) {
        self.player.stop();
    }

    fn seek(&self, position: Duration) -> ServiceResult<()> {
        self.player.try_seek(position).map_err(|error| {
            ServiceError::new(
                ErrorCode::Unsupported,
                format!("Unable to seek audio: {error}"),
            )
            .operation("seek audio")
        })
    }

    fn volume(&self) -> f32 {
        self.player.volume()
    }

    fn set_volume(&self, volume: f32) {
        self.player.set_volume(volume);
    }
}

/// Stands in for rodio in builds without the `preview-audio` feature.
#[cfg(not(feature = "preview-audio"))]
struct UnavailableAudioBackend;

#[cfg(not(feature = "preview-audio"))]
impl AudioBackend for UnavailableAudioBackend {
    fn open(&self, _path: &Path) -> ServiceResult<Arc<dyn AudioPlayback>> {
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Audio playback isn't included in this build",
        ))
    }
}

struct ActiveAudio {
    path: PathBuf,
    playback: Arc<dyn AudioPlayback>,
}

/// Single-session audio controller. Starting any new load invalidates and
/// stops the previous one, including a decoder still being prepared.
#[derive(Clone)]
pub struct AudioService {
    context: ServiceContext,
    backend: Arc<dyn AudioBackend>,
    active: Arc<Mutex<Option<ActiveAudio>>>,
    generation: Arc<AtomicU64>,
}

impl AudioService {
    pub fn new(context: ServiceContext) -> Self {
        #[cfg(feature = "preview-audio")]
        let backend = Arc::new(RodioBackend {
            cache_dir: context.resources().cache_dir.join("audio-preview"),
            transcode_lock: Mutex::new(()),
        });
        #[cfg(not(feature = "preview-audio"))]
        let backend = Arc::new(UnavailableAudioBackend);
        Self::with_backend(context, backend)
    }

    pub fn with_backend(context: ServiceContext, backend: Arc<dyn AudioBackend>) -> Self {
        Self {
            context,
            backend,
            active: Arc::new(Mutex::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Reuse the process-wide audio backend while keeping playback and
    /// cancellation state private to one window.
    pub fn fork_playback_scope(&self) -> Self {
        Self {
            context: self.context.clone(),
            backend: Arc::clone(&self.backend),
            active: Arc::new(Mutex::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn load(&self, path: PathBuf) -> BlockingTask<AudioStatus> {
        let generation = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.stop_active();
        let backend = Arc::clone(&self.backend);
        let active = Arc::clone(&self.active);
        let current_generation = Arc::clone(&self.generation);
        self.context.spawn_blocking(move || {
            validate_audio_path(&path)?;
            let playback = backend.open(&path)?;
            let mut guard = active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if current_generation.load(Ordering::Acquire) != generation {
                playback.stop();
                return Err(ServiceError::new(
                    ErrorCode::Cancelled,
                    "Audio load was superseded",
                ));
            }
            *guard = Some(ActiveAudio {
                path: path.clone(),
                playback,
            });
            status_from_active(guard.as_ref().expect("audio session was just installed"))
        })
    }

    pub fn status(&self) -> ServiceResult<AudioStatus> {
        let guard = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active = guard.as_ref().ok_or_else(no_active_audio)?;
        status_from_active(active)
    }

    pub fn play(&self) -> ServiceResult<AudioStatus> {
        self.with_active(|active| {
            if active.playback.is_empty() {
                active.playback.seek(Duration::ZERO)?;
            }
            active.playback.play();
            status_from_active(active)
        })
    }

    pub fn pause(&self) -> ServiceResult<AudioStatus> {
        self.with_active(|active| {
            active.playback.pause();
            status_from_active(active)
        })
    }

    pub fn seek(&self, position_ms: u64) -> ServiceResult<AudioStatus> {
        self.with_active(|active| {
            let requested = Duration::from_millis(position_ms);
            let position = active
                .playback
                .duration()
                .map_or(requested, |duration| requested.min(duration));
            active.playback.seek(position)?;
            status_from_active(active)
        })
    }

    pub fn set_volume(&self, volume: f32) -> ServiceResult<AudioStatus> {
        self.with_active(|active| {
            active.playback.set_volume(volume.clamp(0.0, 1.0));
            status_from_active(active)
        })
    }

    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.stop_active();
    }

    fn with_active<T>(
        &self,
        operation: impl FnOnce(&ActiveAudio) -> ServiceResult<T>,
    ) -> ServiceResult<T> {
        let guard = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(guard.as_ref().ok_or_else(no_active_audio)?)
    }

    fn stop_active(&self) {
        if let Some(active) = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            active.playback.stop();
        }
    }
}

fn validate_audio_path(path: &Path) -> ServiceResult<()> {
    let metadata = path.metadata().map_err(ServiceError::from)?;
    if !metadata.is_file() {
        return Err(ServiceError::new(
            ErrorCode::NotFound,
            format!("Audio file does not exist: {}", path.display()),
        ));
    }
    if metadata.len() > MAX_AUDIO_SOURCE_BYTES {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Audio preview is limited to files no larger than 512 MiB",
        ));
    }
    let extension = audio_extension(path);
    if !matches!(
        extension.as_str(),
        "mp3"
            | "wav"
            | "flac"
            | "ogg"
            | "opus"
            | "oga"
            | "m4a"
            | "m4b"
            | "aac"
            | "aif"
            | "aiff"
            | "caf"
            | "alac"
    ) {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            format!("Native audio playback does not support .{extension}"),
        ));
    }
    Ok(())
}

fn audio_extension(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

#[cfg(feature = "preview-audio")]
fn transcode_opus(path: &Path, cache_dir: &Path) -> ServiceResult<PathBuf> {
    let metadata = path.metadata().map_err(ServiceError::from)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |value| value.as_nanos());
    let mut hasher = Sha256::new();
    hasher.update(path.as_os_str().to_string_lossy().as_bytes());
    hasher.update(metadata.len().to_le_bytes());
    hasher.update(modified.to_le_bytes());
    let digest = format!("{:x}", hasher.finalize());
    let output = cache_dir.join(format!("{}-opus.flac", &digest[..24]));
    if output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
        return Ok(output);
    }
    std::fs::create_dir_all(cache_dir).map_err(ServiceError::from)?;
    let temporary = output.with_extension(format!("flac.{}.tmp", uuid::Uuid::new_v4()));
    let result = run_with_timeout(
        ffmpeg_command()
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
            .args(["-protocol_whitelist", "file,pipe", "-i"])
            .arg(path)
            .args(["-map", "0:a:0", "-vn", "-c:a", "flac", "-f", "flac"])
            .arg(&temporary),
        AUDIO_TRANSCODE_TIMEOUT,
        0,
        64 * 1024,
    )
    .map_err(|error| match error {
        ProcessError::Io(error) => ServiceError::new(
            ErrorCode::HelperMissing,
            format!("Opus playback requires the bundled FFmpeg helper: {error}"),
        ),
        ProcessError::TimedOut => ServiceError::new(
            ErrorCode::Busy,
            "Opus conversion timed out before playback could start",
        ),
    })?;
    if !result.status.success()
        || !temporary
            .metadata()
            .is_ok_and(|metadata| metadata.len() > 0)
    {
        let message = String::from_utf8_lossy(&result.stderr).trim().to_string();
        let _ = std::fs::remove_file(&temporary);
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            if message.is_empty() {
                "FFmpeg could not decode this Opus file".to_string()
            } else {
                format!("FFmpeg could not decode this Opus file: {message}")
            },
        ));
    }
    if output.exists() {
        std::fs::remove_file(&output).map_err(ServiceError::from)?;
    }
    std::fs::rename(&temporary, &output).map_err(ServiceError::from)?;
    prune_audio_cache(cache_dir);
    Ok(output)
}

#[cfg(feature = "preview-audio")]
fn ffmpeg_command() -> Command {
    let command = Command::new("ffmpeg");
    #[cfg(windows)]
    let mut command = command;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

#[cfg(feature = "preview-audio")]
fn prune_audio_cache(cache_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };
    let mut cached = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            Some((
                entry.path(),
                metadata.modified().unwrap_or(UNIX_EPOCH),
                metadata.len(),
            ))
        })
        .collect::<Vec<_>>();
    cached.sort_by_key(|(_, modified, _)| *modified);
    let mut bytes = cached.iter().map(|(_, _, size)| *size).sum::<u64>();
    while cached.len() > MAX_TRANSCODE_CACHE_ENTRIES || bytes > MAX_TRANSCODE_CACHE_BYTES {
        let (path, _, size) = cached.remove(0);
        if std::fs::remove_file(path).is_ok() {
            bytes = bytes.saturating_sub(size);
        }
    }
}

fn no_active_audio() -> ServiceError {
    ServiceError::new(ErrorCode::InvalidInput, "No audio preview is active")
}

fn status_from_active(active: &ActiveAudio) -> ServiceResult<AudioStatus> {
    let duration = active.playback.duration();
    let position = active.playback.position();
    let finished = active.playback.is_empty();
    Ok(AudioStatus {
        path: active.path.clone(),
        duration_ms: duration.map(duration_ms),
        position_ms: duration_ms(position),
        playing: !active.playback.is_paused() && !finished,
        finished,
        volume: active.playback.volume(),
    })
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[derive(Debug)]
    struct FakeState {
        position: Duration,
        paused: bool,
        stopped: bool,
        volume: f32,
    }

    impl Default for FakeState {
        fn default() -> Self {
            Self {
                position: Duration::ZERO,
                paused: true,
                stopped: false,
                volume: DEFAULT_VOLUME,
            }
        }
    }

    #[derive(Default)]
    struct FakePlayback {
        state: Mutex<FakeState>,
    }

    impl AudioPlayback for FakePlayback {
        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(120))
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
            state.stopped = true;
            state.paused = true;
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

    struct FixedBackend {
        playback: Arc<FakePlayback>,
    }

    impl AudioBackend for FixedBackend {
        fn open(&self, _path: &Path) -> ServiceResult<Arc<dyn AudioPlayback>> {
            Ok(self.playback.clone())
        }
    }

    fn fixture_service(root: &Path, playback: Arc<FakePlayback>) -> AudioService {
        AudioService::with_backend(
            ServiceContext::new(crate::ResourcePaths::test(root)),
            Arc::new(FixedBackend { playback }),
        )
    }

    #[test]
    fn playback_forks_share_the_backend_but_not_window_state() {
        let directory = tempfile::tempdir().unwrap();
        let service = fixture_service(directory.path(), Arc::new(FakePlayback::default()));
        let fork = service.fork_playback_scope();
        assert!(Arc::ptr_eq(&service.backend, &fork.backend));
        assert!(!Arc::ptr_eq(&service.active, &fork.active));
        assert!(!Arc::ptr_eq(&service.generation, &fork.generation));
    }

    #[test]
    fn loads_paused_and_controls_one_native_session() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("song.wav");
        std::fs::write(&path, b"fixture").unwrap();
        let playback = Arc::new(FakePlayback::default());
        let service = fixture_service(directory.path(), Arc::clone(&playback));

        let loaded = service.load(path.clone()).wait().unwrap();
        assert_eq!(loaded.path, path);
        assert_eq!(loaded.duration_ms, Some(120_000));
        assert!(!loaded.playing);

        assert!(service.play().unwrap().playing);
        assert_eq!(service.seek(130_000).unwrap().position_ms, 120_000);
        assert_eq!(service.set_volume(2.0).unwrap().volume, 1.0);
        assert!(!service.pause().unwrap().playing);

        service.stop();
        assert_eq!(service.status().unwrap_err().code, ErrorCode::InvalidInput);
        assert!(playback.state.lock().unwrap().stopped);
    }

    #[test]
    fn rejects_missing_and_unsupported_files_before_opening_output() {
        let directory = tempfile::tempdir().unwrap();
        let service = fixture_service(directory.path(), Arc::new(FakePlayback::default()));
        let missing = service
            .load(directory.path().join("missing.wav"))
            .wait()
            .unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);

        let unsupported = directory.path().join("song.wma");
        std::fs::write(&unsupported, b"fixture").unwrap();
        let error = service.load(unsupported).wait().unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);

        let opus = directory.path().join("song.opus");
        std::fs::write(&opus, b"fixture").unwrap();
        assert_eq!(service.load(opus.clone()).wait().unwrap().path, opus);
    }

    #[test]
    fn default_backend_rejects_undecodable_or_excluded_audio() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("noise.wav");
        std::fs::write(&path, b"not audio").unwrap();
        let service = AudioService::new(ServiceContext::new(crate::ResourcePaths::test(
            directory.path(),
        )));
        let error = service.load(path).wait().unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);
        assert_eq!(
            error.message == "Audio playback isn't included in this build",
            !cfg!(feature = "preview-audio"),
            "{error:?}"
        );
    }

    struct StaleBackend {
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl AudioBackend for StaleBackend {
        fn open(&self, path: &Path) -> ServiceResult<Arc<dyn AudioPlayback>> {
            if path.file_stem().is_some_and(|name| name == "first") {
                let _ = self.entered.send(());
                let _ = self.release.lock().unwrap().recv();
            }
            Ok(Arc::new(FakePlayback::default()))
        }
    }

    #[test]
    fn superseded_decoder_cannot_replace_the_newer_audio_session() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.wav");
        let second = directory.path().join("second.wav");
        std::fs::write(&first, b"fixture").unwrap();
        std::fs::write(&second, b"fixture").unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let service = AudioService::with_backend(
            ServiceContext::new(crate::ResourcePaths::test(directory.path())),
            Arc::new(StaleBackend {
                entered: entered_tx,
                release: Mutex::new(release_rx),
            }),
        );

        let first_task = service.load(first);
        entered_rx.recv().unwrap();
        let second_status = service.load(second.clone()).wait().unwrap();
        assert_eq!(second_status.path, second);
        release_tx.send(()).unwrap();
        assert_eq!(first_task.wait().unwrap_err().code, ErrorCode::Cancelled);
        assert_eq!(service.status().unwrap().path, second);
    }
}
