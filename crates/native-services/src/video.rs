//! Local video playback owned by the native desktop runtime.
//!
//! Soundtracks play through rodio, behind the `preview-audio` cargo feature;
//! without it videos play silently and report no audio.

use std::io::Read;
#[cfg(feature = "preview-audio")]
use std::num::{NonZeroU16, NonZeroU32};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(feature = "preview-audio")]
use rodio::{DeviceSinkBuilder, Player, buffer::SamplesBuffer};
use serde_json::Value;

use crate::process::{ProcessError, run_with_timeout};
use crate::{BlockingTask, ErrorCode, ServiceContext, ServiceError, ServiceResult};

const FRAME_RATE: u64 = 15;
const MAX_FRAME_WIDTH: u32 = 960;
const MAX_FRAME_HEIGHT: u32 = 540;
const FRAME_QUEUE_DEPTH: usize = 3;
#[cfg(feature = "preview-audio")]
const AUDIO_CHANNELS: u16 = 2;
#[cfg(feature = "preview-audio")]
const AUDIO_SAMPLE_RATE: u32 = 48_000;
#[cfg(feature = "preview-audio")]
const AUDIO_CHUNK_FRAMES: usize = 4_800;
#[cfg(feature = "preview-audio")]
const MAX_QUEUED_AUDIO_CHUNKS: usize = 8;
const DEFAULT_VOLUME: f32 = 0.8;
const TOOL_CHECK_TIMEOUT: Duration = Duration::from_secs(3);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const FRAME_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_PROBE_OUTPUT: usize = 1024 * 1024;

/// One decoded BGRA frame. The byte buffer is reference-counted so polling
/// never duplicates the multi-megabyte payload between the service and GPUI.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub position_ms: u64,
    pub bgra: Arc<[u8]>,
}

/// Snapshot consumed by the GPUI transport.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoStatus {
    pub path: PathBuf,
    pub duration_ms: Option<u64>,
    pub position_ms: u64,
    pub width: u32,
    pub height: u32,
    pub playing: bool,
    pub finished: bool,
    pub has_audio: bool,
    pub volume: f32,
}

/// One playback session. Production uses a bounded FFmpeg stream; tests can
/// inject deterministic sessions without codecs or physical audio hardware.
pub trait VideoPlayback: Send + Sync {
    fn status(&self) -> VideoStatus;
    fn take_frame(&self) -> Option<VideoFrame>;
    fn play(&self) -> ServiceResult<VideoStatus>;
    fn pause(&self) -> ServiceResult<VideoStatus>;
    fn seek(&self, position: Duration) -> ServiceResult<VideoStatus>;
    fn set_volume(&self, volume: f32) -> ServiceResult<VideoStatus>;
    fn stop(&self);
}

/// Decoder/output factory used by [`VideoService`].
pub trait VideoBackend: Send + Sync {
    fn open(&self, path: &Path) -> ServiceResult<Arc<dyn VideoPlayback>>;
}

struct FfmpegVideoBackend;

#[derive(Clone, Copy)]
struct VideoMetadata {
    duration_ms: Option<u64>,
    source_width: u32,
    source_height: u32,
    frame_width: u32,
    frame_height: u32,
    /// Whether the soundtrack will be heard: the file has an audio stream and
    /// this build can play it.
    has_audio: bool,
}

struct PlaybackState {
    position_ms: u64,
    started_at: Option<Instant>,
    finished: bool,
    latest_frame: Option<VideoFrame>,
    decoder: Option<DecoderSession>,
}

struct FfmpegVideoPlayback {
    path: PathBuf,
    ffmpeg: PathBuf,
    metadata: VideoMetadata,
    control: Mutex<()>,
    volume: Mutex<f32>,
    state: Mutex<PlaybackState>,
}

struct DecoderSession {
    stop: Arc<AtomicBool>,
    frame_rx: Option<mpsc::Receiver<VideoFrame>>,
    video_child: Child,
    #[cfg(feature = "preview-audio")]
    soundtrack: Option<Soundtrack>,
    threads: Vec<JoinHandle<()>>,
}

/// A second FFmpeg process decoding the audio stream into a rodio player on
/// the default output device.
#[cfg(feature = "preview-audio")]
struct Soundtrack {
    child: Child,
    player: Arc<Player>,
    reader: JoinHandle<()>,
    _device: rodio::MixerDeviceSink,
}

impl Drop for DecoderSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.frame_rx.take();
        let _ = self.video_child.kill();
        #[cfg(feature = "preview-audio")]
        if let Some(soundtrack) = self.soundtrack.take() {
            soundtrack.finish();
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        let _ = self.video_child.wait();
    }
}

impl DecoderSession {
    fn start(
        path: &Path,
        ffmpeg: &Path,
        metadata: VideoMetadata,
        position_ms: u64,
        volume: f32,
    ) -> ServiceResult<Self> {
        let seek = ffmpeg_time(position_ms);
        let scale = format!(
            "fps={FRAME_RATE},scale={}:{}",
            metadata.frame_width, metadata.frame_height
        );
        let mut video_child = helper_command(ffmpeg)
            .args(["-nostdin", "-v", "error", "-ss", &seek])
            .args(["-protocol_whitelist", "file,pipe", "-i"])
            .arg(path)
            .args([
                "-map", "0:v:0", "-an", "-vf", &scale, "-pix_fmt", "bgra", "-f", "rawvideo",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                ServiceError::new(
                    ErrorCode::HelperMissing,
                    format!("Unable to start FFmpeg video decoder: {error}"),
                )
                .retryable(true)
            })?;
        let stdout = video_child.stdout.take().ok_or_else(|| {
            ServiceError::new(ErrorCode::Internal, "FFmpeg video pipe was unavailable")
        })?;
        let stop = Arc::new(AtomicBool::new(false));
        let (frame_tx, frame_rx) = mpsc::sync_channel(FRAME_QUEUE_DEPTH);
        let frame_stop = Arc::clone(&stop);
        let frame_width = metadata.frame_width;
        let frame_height = metadata.frame_height;
        let video_thread = thread::spawn(move || {
            read_video_frames(
                stdout,
                frame_tx,
                frame_stop,
                frame_width,
                frame_height,
                position_ms,
            );
        });

        #[cfg(feature = "preview-audio")]
        let soundtrack = if metadata.has_audio {
            Soundtrack::start(path, ffmpeg, &seek, volume, &stop)
        } else {
            None
        };
        #[cfg(not(feature = "preview-audio"))]
        let _ = volume;

        Ok(Self {
            stop,
            frame_rx: Some(frame_rx),
            video_child,
            #[cfg(feature = "preview-audio")]
            soundtrack,
            threads: vec![video_thread],
        })
    }

    fn take_latest_frame(&mut self) -> Option<VideoFrame> {
        let receiver = self.frame_rx.as_ref()?;
        let mut latest = None;
        for _ in 0..FRAME_QUEUE_DEPTH {
            match receiver.try_recv() {
                Ok(frame) => latest = Some(frame),
                Err(_) => break,
            }
        }
        latest
    }
}

#[cfg(feature = "preview-audio")]
impl Soundtrack {
    /// Start the soundtrack at `seek`, or `None` (silent playback) when there
    /// is no output device or the FFmpeg audio decoder cannot start.
    fn start(
        path: &Path,
        ffmpeg: &Path,
        seek: &str,
        volume: f32,
        stop: &Arc<AtomicBool>,
    ) -> Option<Self> {
        let mut output = DeviceSinkBuilder::open_default_sink().ok()?;
        output.log_on_drop(false);
        let player = Arc::new(Player::connect_new(output.mixer()));
        player.set_volume(volume);
        let mut child = helper_command(ffmpeg)
            .args(["-nostdin", "-v", "error", "-ss", seek])
            .args(["-protocol_whitelist", "file,pipe", "-i"])
            .arg(path)
            .args([
                "-map", "0:a:0", "-vn", "-ac", "2", "-ar", "48000", "-f", "f32le", "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let audio_stdout = child.stdout.take()?;
        let audio_stop = Arc::clone(stop);
        let thread_player = Arc::clone(&player);
        let reader = thread::spawn(move || {
            read_audio_samples(audio_stdout, thread_player, audio_stop);
        });
        Some(Self {
            child,
            player,
            reader,
            _device: output,
        })
    }

    /// Silence the output and reap the decoder process and its reader.
    fn finish(mut self) {
        let _ = self.child.kill();
        self.player.stop();
        let _ = self.reader.join();
        let _ = self.child.wait();
    }
}

fn read_video_frames(
    mut stdout: impl Read,
    sender: mpsc::SyncSender<VideoFrame>,
    stop: Arc<AtomicBool>,
    width: u32,
    height: u32,
    start_ms: u64,
) {
    let Some(frame_bytes) = frame_byte_len(width, height) else {
        return;
    };
    let mut index = 0_u64;
    while !stop.load(Ordering::Acquire) {
        let mut bytes = vec![0_u8; frame_bytes];
        if stdout.read_exact(&mut bytes).is_err() {
            break;
        }
        let frame = VideoFrame {
            width,
            height,
            position_ms: start_ms.saturating_add(index.saturating_mul(1_000) / FRAME_RATE),
            bgra: bytes.into(),
        };
        if sender.send(frame).is_err() {
            break;
        }
        index = index.saturating_add(1);
    }
}

#[cfg(feature = "preview-audio")]
fn read_audio_samples(mut stdout: impl Read, player: Arc<Player>, stop: Arc<AtomicBool>) {
    let bytes_per_chunk = AUDIO_CHUNK_FRAMES * usize::from(AUDIO_CHANNELS) * size_of::<f32>();
    let channels = NonZeroU16::new(AUDIO_CHANNELS).expect("audio channel count is nonzero");
    let sample_rate = NonZeroU32::new(AUDIO_SAMPLE_RATE).expect("audio sample rate is nonzero");
    let mut bytes = vec![0_u8; bytes_per_chunk];
    while !stop.load(Ordering::Acquire) {
        while player.len() > MAX_QUEUED_AUDIO_CHUNKS && !stop.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(10));
        }
        let mut filled = 0;
        while filled < bytes.len() && !stop.load(Ordering::Acquire) {
            match stdout.read(&mut bytes[filled..]) {
                Ok(0) => break,
                Ok(count) => filled += count,
                Err(_) => return,
            }
        }
        let sample_bytes = filled - filled % size_of::<f32>();
        if sample_bytes == 0 {
            break;
        }
        let samples = bytes[..sample_bytes]
            .as_chunks::<{ size_of::<f32>() }>()
            .0
            .iter()
            .map(|sample| f32::from_le_bytes(*sample))
            .collect::<Vec<_>>();
        player.append(SamplesBuffer::new(channels, sample_rate, samples));
    }
}

impl VideoBackend for FfmpegVideoBackend {
    fn open(&self, path: &Path) -> ServiceResult<Arc<dyn VideoPlayback>> {
        validate_video_path(path)?;
        let ffmpeg = available_tool("ffmpeg").ok_or_else(|| {
            ServiceError::new(
                ErrorCode::HelperMissing,
                "Install FFmpeg to play video previews.",
            )
            .retryable(true)
        })?;
        let ffprobe = available_tool("ffprobe").ok_or_else(|| {
            ServiceError::new(
                ErrorCode::HelperMissing,
                "Install FFmpeg with ffprobe to inspect video previews.",
            )
            .retryable(true)
        })?;
        let metadata = probe_video(path, &ffprobe)?;
        let frame = extract_frame(path, &ffmpeg, metadata, 0)?;
        Ok(Arc::new(FfmpegVideoPlayback {
            path: path.to_path_buf(),
            ffmpeg,
            metadata,
            control: Mutex::new(()),
            volume: Mutex::new(DEFAULT_VOLUME),
            state: Mutex::new(PlaybackState {
                position_ms: 0,
                started_at: None,
                finished: false,
                latest_frame: Some(frame),
                decoder: None,
            }),
        }))
    }
}

impl FfmpegVideoPlayback {
    fn status_locked(&self, state: &mut PlaybackState) -> VideoStatus {
        let position_ms = current_position(state, self.metadata.duration_ms);
        if self
            .metadata
            .duration_ms
            .is_some_and(|duration| position_ms >= duration)
        {
            state.finished = true;
            state.started_at = None;
            state.position_ms = position_ms;
        }
        VideoStatus {
            path: self.path.clone(),
            duration_ms: self.metadata.duration_ms,
            position_ms,
            width: self.metadata.source_width,
            height: self.metadata.source_height,
            playing: state.started_at.is_some() && !state.finished,
            finished: state.finished,
            has_audio: self.metadata.has_audio,
            volume: *self
                .volume
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        }
    }
}

impl VideoPlayback for FfmpegVideoPlayback {
    fn status(&self) -> VideoStatus {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.status_locked(&mut state)
    }

    fn take_frame(&self) -> Option<VideoFrame> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(frame) = state
            .decoder
            .as_mut()
            .and_then(DecoderSession::take_latest_frame)
        {
            state.latest_frame = Some(frame);
        }
        state.latest_frame.clone()
    }

    fn play(&self) -> ServiceResult<VideoStatus> {
        let _control = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (position_ms, previous) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.started_at.is_some() {
                return Ok(self.status_locked(&mut state));
            }
            if state.finished {
                state.position_ms = 0;
                state.finished = false;
            }
            (state.position_ms, state.decoder.take())
        };
        drop(previous);
        let volume = *self
            .volume
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let decoder =
            DecoderSession::start(&self.path, &self.ffmpeg, self.metadata, position_ms, volume)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.decoder = Some(decoder);
        state.started_at = Some(Instant::now());
        Ok(self.status_locked(&mut state))
    }

    fn pause(&self) -> ServiceResult<VideoStatus> {
        let _control = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let decoder = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.position_ms = current_position(&state, self.metadata.duration_ms);
            state.started_at = None;
            state.decoder.take()
        };
        drop(decoder);
        Ok(self.status())
    }

    fn seek(&self, position: Duration) -> ServiceResult<VideoStatus> {
        let _control = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let requested = u64::try_from(position.as_millis()).unwrap_or(u64::MAX);
        let position_ms = self
            .metadata
            .duration_ms
            .map_or(requested, |duration| requested.min(duration));
        let (was_playing, finished, decoder) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let was_playing = state.started_at.is_some();
            state.position_ms = position_ms;
            state.started_at = None;
            state.finished = self
                .metadata
                .duration_ms
                .is_some_and(|duration| position_ms >= duration);
            (was_playing, state.finished, state.decoder.take())
        };
        drop(decoder);
        let frame = extract_frame(&self.path, &self.ffmpeg, self.metadata, position_ms)?;
        let decoder = if was_playing && !finished {
            let volume = *self
                .volume
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Some(DecoderSession::start(
                &self.path,
                &self.ffmpeg,
                self.metadata,
                position_ms,
                volume,
            )?)
        } else {
            None
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.latest_frame = Some(frame);
        state.decoder = decoder;
        if was_playing && !finished {
            state.started_at = Some(Instant::now());
        }
        Ok(self.status_locked(&mut state))
    }

    fn set_volume(&self, volume: f32) -> ServiceResult<VideoStatus> {
        let volume = volume.clamp(0.0, 1.0);
        *self
            .volume
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = volume;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        #[cfg(feature = "preview-audio")]
        if let Some(soundtrack) = state
            .decoder
            .as_ref()
            .and_then(|decoder| decoder.soundtrack.as_ref())
        {
            soundtrack.player.set_volume(volume);
        }
        Ok(self.status_locked(&mut state))
    }

    fn stop(&self) {
        let _control = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let decoder = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.started_at = None;
            state.position_ms = 0;
            state.finished = false;
            state.decoder.take()
        };
        drop(decoder);
    }
}

impl Drop for FfmpegVideoPlayback {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.decoder.take();
    }
}

/// Single-session video controller. Loading a new preview supersedes and
/// stops the previous decoder before any replacement can become visible.
#[derive(Clone)]
pub struct VideoService {
    context: ServiceContext,
    backend: Arc<dyn VideoBackend>,
    active: Arc<Mutex<Option<Arc<dyn VideoPlayback>>>>,
    generation: Arc<AtomicU64>,
    command_sequence: Arc<CommandSequence>,
}

#[derive(Default)]
struct CommandSequence {
    next: AtomicU64,
    serving: Mutex<u64>,
    ready: Condvar,
}

impl CommandSequence {
    fn issue(self: &Arc<Self>) -> CommandTurn {
        CommandTurn {
            sequence: Arc::clone(self),
            ticket: self.next.fetch_add(1, Ordering::AcqRel),
        }
    }
}

struct CommandTurn {
    sequence: Arc<CommandSequence>,
    ticket: u64,
}

impl CommandTurn {
    fn wait(&self) {
        let mut serving = self
            .sequence
            .serving
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *serving != self.ticket {
            serving = self
                .sequence
                .ready
                .wait(serving)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

impl Drop for CommandTurn {
    fn drop(&mut self) {
        let mut serving = self
            .sequence
            .serving
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *serving == self.ticket {
            *serving = serving.wrapping_add(1);
            self.sequence.ready.notify_all();
        }
    }
}

impl VideoService {
    pub fn new(context: ServiceContext) -> Self {
        Self::with_backend(context, Arc::new(FfmpegVideoBackend))
    }

    pub fn with_backend(context: ServiceContext, backend: Arc<dyn VideoBackend>) -> Self {
        Self {
            context,
            backend,
            active: Arc::new(Mutex::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
            command_sequence: Arc::new(CommandSequence::default()),
        }
    }

    /// Reuse the process-wide decoder backend while keeping playback,
    /// sequencing, and cancellation state private to one window.
    pub fn fork_playback_scope(&self) -> Self {
        Self {
            context: self.context.clone(),
            backend: Arc::clone(&self.backend),
            active: Arc::new(Mutex::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
            command_sequence: Arc::new(CommandSequence::default()),
        }
    }

    pub fn load(&self, path: PathBuf) -> BlockingTask<VideoStatus> {
        let generation = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.stop_active();
        let backend = Arc::clone(&self.backend);
        let active = Arc::clone(&self.active);
        let current_generation = Arc::clone(&self.generation);
        let turn = self.command_sequence.issue();
        self.context.spawn_blocking(move || {
            turn.wait();
            let playback = backend.open(&path)?;
            let mut active = active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if current_generation.load(Ordering::Acquire) != generation {
                playback.stop();
                return Err(ServiceError::new(
                    ErrorCode::Cancelled,
                    "Video load was superseded",
                ));
            }
            let status = playback.status();
            *active = Some(playback);
            Ok(status)
        })
    }

    pub fn status(&self) -> ServiceResult<VideoStatus> {
        Ok(self.active()?.status())
    }

    pub fn take_frame(&self) -> ServiceResult<Option<VideoFrame>> {
        Ok(self.active()?.take_frame())
    }

    pub fn play(&self) -> BlockingTask<VideoStatus> {
        self.run(|playback| playback.play())
    }

    pub fn pause(&self) -> BlockingTask<VideoStatus> {
        self.run(|playback| playback.pause())
    }

    pub fn seek(&self, position_ms: u64) -> BlockingTask<VideoStatus> {
        self.run(move |playback| playback.seek(Duration::from_millis(position_ms)))
    }

    pub fn set_volume(&self, volume: f32) -> ServiceResult<VideoStatus> {
        self.active()?.set_volume(volume)
    }

    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.stop_active();
    }

    fn run(
        &self,
        operation: impl FnOnce(Arc<dyn VideoPlayback>) -> ServiceResult<VideoStatus> + Send + 'static,
    ) -> BlockingTask<VideoStatus> {
        let active = Arc::clone(&self.active);
        let generation = Arc::clone(&self.generation);
        let expected_generation = generation.load(Ordering::Acquire);
        let turn = self.command_sequence.issue();
        self.context.spawn_blocking(move || {
            turn.wait();
            let playback = {
                let active = active
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if generation.load(Ordering::Acquire) != expected_generation {
                    return Err(ServiceError::new(
                        ErrorCode::Cancelled,
                        "Video command was superseded",
                    ));
                }
                active.clone().ok_or_else(|| {
                    ServiceError::new(ErrorCode::InvalidInput, "No video preview is active")
                })?
            };
            let result = operation(playback)?;
            if generation.load(Ordering::Acquire) != expected_generation {
                return Err(ServiceError::new(
                    ErrorCode::Cancelled,
                    "Video command was superseded",
                ));
            }
            Ok(result)
        })
    }

    fn active(&self) -> ServiceResult<Arc<dyn VideoPlayback>> {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(|| ServiceError::new(ErrorCode::InvalidInput, "No video preview is active"))
    }

    fn stop_active(&self) {
        if let Some(playback) = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            playback.stop();
        }
    }
}

fn current_position(state: &PlaybackState, duration_ms: Option<u64>) -> u64 {
    let elapsed = state.started_at.map_or(0, |started| {
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
    });
    let position = state.position_ms.saturating_add(elapsed);
    duration_ms.map_or(position, |duration| position.min(duration))
}

fn validate_video_path(path: &Path) -> ServiceResult<()> {
    if !path.is_file() {
        return Err(ServiceError::new(
            ErrorCode::NotFound,
            format!("Video file does not exist: {}", path.display()),
        ));
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(
        extension.as_str(),
        "mp4"
            | "webm"
            | "m4v"
            | "mov"
            | "avi"
            | "mkv"
            | "wmv"
            | "flv"
            | "m2ts"
            | "mts"
            | "mpeg"
            | "mpg"
            | "3gp"
    ) {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            format!("Native video playback does not support .{extension}"),
        ));
    }
    Ok(())
}

fn available_tool(name: &str) -> Option<PathBuf> {
    run_with_timeout(
        helper_command(name).arg("-version"),
        TOOL_CHECK_TIMEOUT,
        0,
        0,
    )
    .ok()?
    .status
    .success()
    .then(|| PathBuf::from(name))
}

fn probe_video(path: &Path, ffprobe: &Path) -> ServiceResult<VideoMetadata> {
    let output = run_with_timeout(
        helper_command(ffprobe)
            .args([
                "-v",
                "error",
                "-protocol_whitelist",
                "file,pipe",
                "-show_entries",
                "stream=codec_type,width,height:format=duration",
                "-of",
                "json",
            ])
            .arg(path),
        PROBE_TIMEOUT,
        MAX_PROBE_OUTPUT,
        0,
    )
    .map_err(|error| match error {
        ProcessError::Io(error) => ServiceError::new(
            ErrorCode::HelperMissing,
            format!("Unable to start ffprobe: {error}"),
        ),
        ProcessError::TimedOut => ServiceError::new(
            ErrorCode::InvalidInput,
            "FFmpeg timed out while inspecting this video.",
        ),
    })?;
    if !output.status.success() {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "FFmpeg could not inspect this video.",
        ));
    }
    if output.stdout_truncated {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "ffprobe returned more metadata than Explorie can safely process.",
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            format!("ffprobe returned invalid metadata: {error}"),
        )
    })?;
    let streams = value
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| ServiceError::new(ErrorCode::InvalidInput, "Video has no media streams"))?;
    let video = streams
        .iter()
        .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"))
        .ok_or_else(|| ServiceError::new(ErrorCode::InvalidInput, "File has no video stream"))?;
    let source_width = video
        .get("width")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| ServiceError::new(ErrorCode::InvalidInput, "Video width is invalid"))?;
    let source_height = video
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| ServiceError::new(ErrorCode::InvalidInput, "Video height is invalid"))?;
    let duration_ms = value
        .pointer("/format/duration")
        .and_then(Value::as_str)
        .and_then(|duration| duration.parse::<f64>().ok())
        .filter(|duration| duration.is_finite() && *duration > 0.0)
        .map(|duration| (duration * 1_000.0).round().min(u64::MAX as f64) as u64);
    let (frame_width, frame_height) = target_dimensions(source_width, source_height);
    Ok(VideoMetadata {
        duration_ms,
        source_width,
        source_height,
        frame_width,
        frame_height,
        has_audio: cfg!(feature = "preview-audio")
            && streams
                .iter()
                .any(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("audio")),
    })
}

fn target_dimensions(width: u32, height: u32) -> (u32, u32) {
    let scale = (MAX_FRAME_WIDTH as f64 / f64::from(width))
        .min(MAX_FRAME_HEIGHT as f64 / f64::from(height))
        .min(1.0);
    let even = |value: u32| (value.max(2) / 2) * 2;
    (
        even((f64::from(width) * scale).round() as u32),
        even((f64::from(height) * scale).round() as u32),
    )
}

fn extract_frame(
    path: &Path,
    ffmpeg: &Path,
    metadata: VideoMetadata,
    position_ms: u64,
) -> ServiceResult<VideoFrame> {
    let seek = ffmpeg_time(position_ms);
    let scale = format!("scale={}:{}", metadata.frame_width, metadata.frame_height);
    let expected =
        frame_byte_len(metadata.frame_width, metadata.frame_height).ok_or_else(|| {
            ServiceError::new(ErrorCode::InvalidInput, "Decoded video frame is too large")
        })?;
    let output = run_with_timeout(
        helper_command(ffmpeg)
            .args(["-nostdin", "-v", "error", "-ss", &seek])
            .args(["-protocol_whitelist", "file,pipe", "-i"])
            .arg(path)
            .args([
                "-map",
                "0:v:0",
                "-frames:v",
                "1",
                "-vf",
                &scale,
                "-pix_fmt",
                "bgra",
                "-f",
                "rawvideo",
                "pipe:1",
            ]),
        FRAME_TIMEOUT,
        expected.saturating_add(1),
        0,
    )
    .map_err(|error| match error {
        ProcessError::Io(error) => ServiceError::new(
            ErrorCode::HelperMissing,
            format!("Unable to start FFmpeg frame decoder: {error}"),
        ),
        ProcessError::TimedOut => ServiceError::new(
            ErrorCode::InvalidInput,
            "FFmpeg timed out while decoding this video frame.",
        ),
    })?;
    if !output.status.success() || output.stdout_truncated || output.stdout.len() != expected {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "FFmpeg could not decode a frame from this video.",
        ));
    }
    Ok(VideoFrame {
        width: metadata.frame_width,
        height: metadata.frame_height,
        position_ms,
        bgra: output.stdout.into(),
    })
}

fn frame_byte_len(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)
}

fn ffmpeg_time(position_ms: u64) -> String {
    format!("{}.{:03}", position_ms / 1_000, position_ms % 1_000)
}

fn helper_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let command = Command::new(program);
    #[cfg(windows)]
    let mut command = command;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    struct FakePlayback {
        status: Mutex<VideoStatus>,
        frame: Mutex<Option<VideoFrame>>,
        stopped: AtomicBool,
        operations: Mutex<Vec<String>>,
        seek_delay_ms: AtomicU64,
    }

    impl FakePlayback {
        fn new(path: PathBuf) -> Self {
            Self {
                status: Mutex::new(VideoStatus {
                    path,
                    duration_ms: Some(2_000),
                    position_ms: 0,
                    width: 320,
                    height: 180,
                    playing: false,
                    finished: false,
                    has_audio: true,
                    volume: DEFAULT_VOLUME,
                }),
                frame: Mutex::new(Some(VideoFrame {
                    width: 2,
                    height: 2,
                    position_ms: 0,
                    bgra: vec![0; 16].into(),
                })),
                stopped: AtomicBool::new(false),
                operations: Mutex::new(Vec::new()),
                seek_delay_ms: AtomicU64::new(0),
            }
        }
    }

    impl VideoPlayback for FakePlayback {
        fn status(&self) -> VideoStatus {
            self.status.lock().unwrap().clone()
        }

        fn take_frame(&self) -> Option<VideoFrame> {
            self.frame.lock().unwrap().clone()
        }

        fn play(&self) -> ServiceResult<VideoStatus> {
            self.operations.lock().unwrap().push("play".to_string());
            let mut status = self.status.lock().unwrap();
            status.playing = true;
            status.finished = false;
            Ok(status.clone())
        }

        fn pause(&self) -> ServiceResult<VideoStatus> {
            self.operations.lock().unwrap().push("pause".to_string());
            let mut status = self.status.lock().unwrap();
            status.playing = false;
            Ok(status.clone())
        }

        fn seek(&self, position: Duration) -> ServiceResult<VideoStatus> {
            self.operations
                .lock()
                .unwrap()
                .push(format!("seek:{}", position.as_millis()));
            let delay = self.seek_delay_ms.load(Ordering::Acquire);
            if delay > 0 {
                thread::sleep(Duration::from_millis(delay));
            }
            let mut status = self.status.lock().unwrap();
            status.position_ms = u64::try_from(position.as_millis()).unwrap();
            Ok(status.clone())
        }

        fn set_volume(&self, volume: f32) -> ServiceResult<VideoStatus> {
            let mut status = self.status.lock().unwrap();
            status.volume = volume.clamp(0.0, 1.0);
            Ok(status.clone())
        }

        fn stop(&self) {
            self.stopped.store(true, Ordering::Release);
        }
    }

    #[derive(Default)]
    struct FakeBackend {
        opened: Mutex<Vec<Arc<FakePlayback>>>,
    }

    impl VideoBackend for FakeBackend {
        fn open(&self, path: &Path) -> ServiceResult<Arc<dyn VideoPlayback>> {
            let playback = Arc::new(FakePlayback::new(path.to_path_buf()));
            self.opened.lock().unwrap().push(Arc::clone(&playback));
            Ok(playback)
        }
    }

    #[test]
    fn playback_forks_share_the_backend_but_not_window_state() {
        let root = tempdir().unwrap();
        let service = VideoService::with_backend(
            ServiceContext::new(crate::ResourcePaths::test(root.path())),
            Arc::new(FakeBackend::default()),
        );
        let fork = service.fork_playback_scope();
        assert!(Arc::ptr_eq(&service.backend, &fork.backend));
        assert!(!Arc::ptr_eq(&service.active, &fork.active));
        assert!(!Arc::ptr_eq(&service.generation, &fork.generation));
        assert!(!Arc::ptr_eq(
            &service.command_sequence,
            &fork.command_sequence
        ));
    }

    #[test]
    fn service_controls_one_bounded_video_session() {
        let root = tempdir().unwrap();
        let backend = Arc::new(FakeBackend::default());
        let service = VideoService::with_backend(
            ServiceContext::new(crate::ResourcePaths::test(root.path())),
            backend.clone(),
        );
        let path = root.path().join("clip.mp4");
        fs::write(&path, b"fixture").unwrap();

        let loaded = pollster::block_on(service.load(path.clone())).unwrap();
        assert_eq!(loaded.path, path);
        assert!(!loaded.playing);
        assert_eq!(service.take_frame().unwrap().unwrap().bgra.len(), 16);
        assert!(pollster::block_on(service.play()).unwrap().playing);
        assert!(!pollster::block_on(service.pause()).unwrap().playing);
        assert_eq!(
            pollster::block_on(service.seek(1_250)).unwrap().position_ms,
            1_250
        );
        assert_eq!(service.set_volume(2.0).unwrap().volume, 1.0);
        service.stop();
        assert!(
            backend.opened.lock().unwrap()[0]
                .stopped
                .load(Ordering::Acquire)
        );
        assert_eq!(service.status().unwrap_err().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn asynchronous_video_commands_execute_in_submission_order() {
        let root = tempdir().unwrap();
        let backend = Arc::new(FakeBackend::default());
        let service = VideoService::with_backend(
            ServiceContext::new(crate::ResourcePaths::test(root.path())),
            backend.clone(),
        );
        let path = root.path().join("ordered.mp4");
        fs::write(&path, b"fixture").unwrap();
        pollster::block_on(service.load(path)).unwrap();
        let playback = backend.opened.lock().unwrap()[0].clone();
        playback.seek_delay_ms.store(100, Ordering::Release);

        let seek = service.seek(400);
        let pause = service.pause();
        let play = service.play();
        pollster::block_on(seek).unwrap();
        pollster::block_on(pause).unwrap();
        pollster::block_on(play).unwrap();

        assert_eq!(
            *playback.operations.lock().unwrap(),
            ["seek:400", "pause", "play"]
        );
    }

    #[test]
    fn validates_extensions_dimensions_and_frame_sizes() {
        let root = tempdir().unwrap();
        let missing = root.path().join("missing.mp4");
        assert_eq!(
            validate_video_path(&missing).unwrap_err().code,
            ErrorCode::NotFound
        );
        let text = root.path().join("notes.txt");
        fs::write(&text, b"not video").unwrap();
        assert_eq!(
            validate_video_path(&text).unwrap_err().code,
            ErrorCode::Unsupported
        );
        assert_eq!(target_dimensions(1_920, 1_080), (960, 540));
        assert_eq!(target_dimensions(320, 240), (320, 240));
        assert_eq!(frame_byte_len(960, 540), Some(2_073_600));
        assert_eq!(ffmpeg_time(12_345), "12.345");
    }

    #[test]
    fn ffmpeg_backend_decodes_and_streams_a_real_fixture_when_available() {
        let (Some(ffmpeg), Some(_ffprobe)) = (available_tool("ffmpeg"), available_tool("ffprobe"))
        else {
            eprintln!("skipping FFmpeg video integration test: helpers unavailable");
            return;
        };
        let root = tempdir().unwrap();
        let path = root.path().join("clip.mp4");
        let status = Command::new(ffmpeg)
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x90:rate=15",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "1",
                "-c:v",
                "mpeg4",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        let service =
            VideoService::new(ServiceContext::new(crate::ResourcePaths::test(root.path())));
        let loaded = pollster::block_on(service.load(path)).unwrap();
        assert_eq!((loaded.width, loaded.height), (160, 90));
        assert_eq!(loaded.has_audio, cfg!(feature = "preview-audio"));
        let poster = service.take_frame().unwrap().unwrap();
        assert_eq!(poster.bgra.len(), 160 * 90 * 4);
        assert!(pollster::block_on(service.play()).unwrap().playing);
        let mut frame = service.take_frame().unwrap().unwrap();
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(20));
            frame = service.take_frame().unwrap().unwrap();
            if frame.position_ms > 0 {
                break;
            }
        }
        assert!(frame.position_ms > 0);
        assert!(frame.position_ms <= 400);
        let paused = pollster::block_on(service.pause()).unwrap();
        assert!(!paused.playing);
        service.stop();

        let malformed = root.path().join("malformed.mp4");
        fs::write(&malformed, b"not a video").unwrap();
        let error = pollster::block_on(service.load(malformed)).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("inspect"));

        let missing = root.path().join("missing.mp4");
        assert_eq!(
            pollster::block_on(service.load(missing)).unwrap_err().code,
            ErrorCode::NotFound
        );
    }
}
