//! Measures how much of the window periodic media updates redraw.
//!
//! Video playback polls every 66 ms and audio every 250 ms, and each poll
//! that changes what the preview shows notifies. These tests drive those
//! polls through the real timers in a realistic window (10,000 entries in
//! List view with the preview panel beside them) and record what each tick
//! re-renders. Run with `--nocapture` to see the numbers.

use std::fs;
use std::sync::Mutex;

use explorie_native_services::{
    AudioBackend, AudioPlayback, ResourcePaths, VideoBackend, VideoPlayback,
};
use gpui::{TestAppContext, VisualTestContext};
use uuid::Uuid;

use super::*;

const ENTRY_COUNT: usize = 10_000;
const TICKS: usize = 30;
const VIDEO_TICK: Duration = Duration::from_millis(66);
const AUDIO_TICK: Duration = Duration::from_millis(250);
const FRAME_WIDTH: u32 = 64;
const FRAME_HEIGHT: u32 = 36;

fn fixture_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!("explorie-render-perf-{}", Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    #[cfg(target_os = "macos")]
    let path = path.canonicalize().unwrap();
    path
}

fn file_entry(path: PathBuf, size: u64) -> FileEntry {
    FileEntry {
        id: Uuid::new_v4(),
        path,
        size,
        modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
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

/// A decoder that is always playing and advances only when the test says so.
pub(super) struct PlayingVideo {
    path: PathBuf,
    position_ms: Mutex<u64>,
    playing: Mutex<bool>,
    /// Hand out frames whose pixel buffer does not match their size, which
    /// the player fails to convert.
    malformed_frames: Mutex<bool>,
}

impl PlayingVideo {
    fn advance(&self, by: Duration) {
        *self.position_ms.lock().unwrap() += by.as_millis() as u64;
    }

    /// Advance to a new, unconvertible frame and stop playing there.
    pub(super) fn stop_on_malformed_frame(&self, by: Duration) {
        self.advance(by);
        *self.playing.lock().unwrap() = false;
        *self.malformed_frames.lock().unwrap() = true;
    }
}

impl VideoPlayback for PlayingVideo {
    fn status(&self) -> VideoStatus {
        VideoStatus {
            path: self.path.clone(),
            duration_ms: Some(600_000),
            position_ms: *self.position_ms.lock().unwrap(),
            width: FRAME_WIDTH,
            height: FRAME_HEIGHT,
            playing: *self.playing.lock().unwrap(),
            finished: false,
            has_audio: true,
            volume: 0.8,
        }
    }

    fn take_frame(&self) -> Option<VideoFrame> {
        Some(VideoFrame {
            width: FRAME_WIDTH,
            height: FRAME_HEIGHT,
            position_ms: *self.position_ms.lock().unwrap(),
            bgra: if *self.malformed_frames.lock().unwrap() {
                vec![128; 4].into()
            } else {
                vec![128; (FRAME_WIDTH * FRAME_HEIGHT * 4) as usize].into()
            },
        })
    }

    fn play(&self) -> ServiceResult<VideoStatus> {
        *self.playing.lock().unwrap() = true;
        Ok(self.status())
    }

    fn pause(&self) -> ServiceResult<VideoStatus> {
        *self.playing.lock().unwrap() = false;
        Ok(self.status())
    }

    fn seek(&self, position: Duration) -> ServiceResult<VideoStatus> {
        *self.position_ms.lock().unwrap() = position.as_millis() as u64;
        Ok(self.status())
    }

    fn set_volume(&self, _: f32) -> ServiceResult<VideoStatus> {
        Ok(self.status())
    }

    fn stop(&self) {
        *self.playing.lock().unwrap() = false;
    }
}

struct PlayingVideoBackend(Arc<PlayingVideo>);

impl VideoBackend for PlayingVideoBackend {
    fn open(&self, _: &Path) -> ServiceResult<Arc<dyn VideoPlayback>> {
        Ok(self.0.clone())
    }
}

/// An output device whose position advances only when the test says so.
#[derive(Default)]
pub(super) struct PlayingAudio {
    position: Mutex<Duration>,
    paused: Mutex<bool>,
    volume: Mutex<f32>,
}

impl PlayingAudio {
    fn advance(&self, by: Duration) {
        *self.position.lock().unwrap() += by;
    }
}

impl AudioPlayback for PlayingAudio {
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(600))
    }

    fn position(&self) -> Duration {
        *self.position.lock().unwrap()
    }

    fn is_paused(&self) -> bool {
        *self.paused.lock().unwrap()
    }

    fn is_empty(&self) -> bool {
        false
    }

    fn play(&self) {
        *self.paused.lock().unwrap() = false;
    }

    fn pause(&self) {
        *self.paused.lock().unwrap() = true;
    }

    fn stop(&self) {
        *self.paused.lock().unwrap() = true;
    }

    fn seek(&self, position: Duration) -> ServiceResult<()> {
        *self.position.lock().unwrap() = position;
        Ok(())
    }

    fn volume(&self) -> f32 {
        *self.volume.lock().unwrap()
    }

    fn set_volume(&self, volume: f32) {
        *self.volume.lock().unwrap() = volume;
    }
}

struct PlayingAudioBackend(Arc<PlayingAudio>);

impl AudioBackend for PlayingAudioBackend {
    fn open(&self, _: &Path) -> ServiceResult<Arc<dyn AudioPlayback>> {
        *self.0.paused.lock().unwrap() = true;
        *self.0.volume.lock().unwrap() = 0.8;
        Ok(self.0.clone())
    }
}

pub(super) struct Fixture {
    directory: PathBuf,
    pub(super) video_path: PathBuf,
    pub(super) audio_path: PathBuf,
    pub(super) video: Arc<PlayingVideo>,
    pub(super) audio: Arc<PlayingAudio>,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let directory = fixture_dir();
        let video_path = directory.join("clip.mp4");
        let audio_path = directory.join("track.mp3");
        fs::write(&video_path, b"fake decoder input").unwrap();
        fs::write(&audio_path, b"fake decoder input").unwrap();
        let video = Arc::new(PlayingVideo {
            path: video_path.clone(),
            position_ms: Mutex::new(0),
            playing: Mutex::new(false),
            malformed_frames: Mutex::new(false),
        });
        Self {
            directory,
            video_path,
            audio_path,
            video,
            audio: Arc::new(PlayingAudio::default()),
        }
    }

    fn services(&self) -> NativeServices {
        let resources = ResourcePaths::test(&self.directory);
        let mut services = NativeServices::with_video_backend(
            resources.clone(),
            Arc::new(PlayingVideoBackend(self.video.clone())),
        );
        services.audio = NativeServices::with_audio_backend(
            resources,
            Arc::new(PlayingAudioBackend(self.audio.clone())),
        )
        .audio;
        services
    }

    /// The media files plus enough other entries to fill the listing.
    fn entries(&self) -> Vec<FileEntry> {
        let extensions = ["txt", "png", "pdf", "rs", "zip", "md", "jpg", "json"];
        let mut entries = vec![
            file_entry(self.audio_path.clone(), 4_000_000),
            file_entry(self.video_path.clone(), 80_000_000),
        ];
        entries.extend((0..ENTRY_COUNT - entries.len()).map(|index| {
            file_entry(
                self.directory.join(format!(
                    "document-{index:05}.{}",
                    extensions[index % extensions.len()]
                )),
                index as u64 * 1_024,
            )
        }));
        entries
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

pub(super) fn wait_for(
    cx: &mut VisualTestContext,
    view: &Entity<DirectoryWindow>,
    what: &str,
    condition: impl Fn(&DirectoryWindow, &App) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        cx.run_until_parked();
        if view.update(cx, |view, cx| condition(view, cx)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

/// Open a 1440×900 window on `fixture` with 10,000 entries in List view and
/// the preview panel shown beside the listing.
pub(super) fn open_window<'a>(
    fixture: &Fixture,
    cx: &'a mut TestAppContext,
) -> (Entity<DirectoryWindow>, &'a mut VisualTestContext) {
    let services = fixture.services();
    let directory = fixture.directory.clone();
    let (view, cx) = cx.add_window_view(|_, cx| DirectoryWindow::new(directory, services, cx));
    cx.simulate_resize(gpui::size(px(1440.0), px(900.0)));
    let entries = fixture.entries();
    view.update(cx, |view, cx| {
        view.browser.set_view_mode(ViewMode::List);
        view.settings.view.show_preview_panel = true;
        view.browser.replace_entries(entries);
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    cx.run_until_parked();
    (view, cx)
}

pub(super) fn preview(cx: &mut VisualTestContext, view: &Entity<DirectoryWindow>, path: &Path) {
    view.update(cx, |view, cx| {
        view.browser.select(path.to_path_buf());
        view.preview_selected(cx);
    });
}

/// What one kind of periodic tick redrew, averaged over [`TICKS`] ticks.
#[derive(Debug)]
struct TickReport {
    label: &'static str,
    stats: RenderStats,
    /// Renders of the media player entity.
    media: usize,
    /// Times the window entity itself was notified.
    window_notifications: usize,
    frame_time: Duration,
}

impl TickReport {
    fn per_tick(&self, value: usize) -> f64 {
        value as f64 / TICKS as f64
    }

    fn print(&self) {
        let milliseconds =
            |time: Duration, count: usize| time.as_secs_f64() * 1_000.0 / count.max(1) as f64;
        eprintln!(
            "{}: per tick {:.2} root renders ({:.3} ms each building the root), \
             {:.2} media player renders, {:.2} listing renders ({:.1} rows built), \
             {:.2} sidebar renders; {:.3} ms per tick until parked",
            self.label,
            self.per_tick(self.stats.root),
            milliseconds(self.stats.root_time, self.stats.root),
            self.per_tick(self.media),
            self.per_tick(self.stats.listing),
            self.per_tick(self.stats.listing_rows),
            self.per_tick(self.stats.sidebar),
            milliseconds(self.frame_time, TICKS),
        );
    }
}

/// The window really is showing the listing, the sidebar and the preview
/// beside the listing. Debug bounds are recorded when an element paints, so
/// this holds for a frame that rendered the listing and the sidebar.
fn assert_realistic_layout(cx: &mut VisualTestContext) {
    let listing = cx.debug_bounds("file-surface").expect("listing is shown");
    let preview = cx.debug_bounds("preview-panel").expect("preview is shown");
    assert!(
        cx.debug_bounds("listing-drop-surface").is_some(),
        "listing is shown"
    );
    assert!(cx.debug_bounds("sidebar").is_some(), "sidebar is shown");
    assert!(
        preview.left() >= listing.right(),
        "preview is beside the list"
    );
}

/// Run [`TICKS`] ticks, advancing playback by `interval` before each, and
/// record what the window rendered in response.
fn measure_ticks(
    label: &'static str,
    cx: &mut VisualTestContext,
    view: &Entity<DirectoryWindow>,
    interval: Duration,
    advance_playback: impl Fn(Duration),
) -> TickReport {
    // Settle anything the setup left pending, draw the whole window once,
    // then start counting.
    cx.run_until_parked();
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    assert_realistic_layout(cx);
    view.update(cx, |view, cx| {
        view.render_stats = RenderStats::default();
        view.media.update(cx, |media, _| media.renders = 0);
    });
    let window_notifications = Rc::new(Cell::new(0));
    let subscription = cx.update(|_, cx| {
        let window_notifications = window_notifications.clone();
        cx.observe(view, move |_, _| {
            window_notifications.set(window_notifications.get() + 1)
        })
    });
    let mut frame_time = Duration::ZERO;
    for _ in 0..TICKS {
        advance_playback(interval);
        let started = Instant::now();
        cx.executor().advance_clock(interval);
        cx.run_until_parked();
        frame_time += started.elapsed();
    }
    drop(subscription);
    let (stats, media) = view.update(cx, |view, cx| {
        (view.render_stats, view.media.read(cx).renders)
    });
    TickReport {
        label,
        stats,
        media,
        window_notifications: window_notifications.get(),
        frame_time,
    }
}

fn video_report(cx: &mut TestAppContext) -> TickReport {
    let fixture = Fixture::new();
    let (view, cx) = open_window(&fixture, cx);
    preview(cx, &view, &fixture.video_path);
    wait_for(cx, &view, "the first video frame", |view, cx| {
        matches!(
            view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::Video,
                ..
            }
        ) && view.media.read(cx).video_frame.is_some()
    });
    view.update(cx, |view, cx| {
        view.media
            .update(cx, |media, cx| media.toggle_video_playback(cx))
    });
    wait_for(cx, &view, "video playback", |view, cx| {
        view.media
            .read(cx)
            .video_status
            .as_ref()
            .is_some_and(|status| status.playing)
    });
    let video = fixture.video.clone();
    let report = measure_ticks("video tick (66 ms)", cx, &view, VIDEO_TICK, |by| {
        video.advance(by)
    });
    view.update(cx, |view, cx| {
        assert_eq!(
            view.media.read(cx).video_frame_position_ms,
            Some(fixture.video.status().position_ms),
            "every tick must still show the newest frame"
        );
    });
    report
}

fn audio_report(cx: &mut TestAppContext) -> TickReport {
    let fixture = Fixture::new();
    let (view, cx) = open_window(&fixture, cx);
    preview(cx, &view, &fixture.audio_path);
    wait_for(cx, &view, "the audio preview", |view, _| {
        matches!(
            view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::Audio,
                ..
            }
        )
    });
    view.update(cx, |view, cx| {
        view.media
            .update(cx, |media, cx| media.toggle_audio_playback(cx))
    });
    let audio = fixture.audio.clone();
    let report = measure_ticks("audio tick (250 ms)", cx, &view, AUDIO_TICK, |by| {
        audio.advance(by)
    });
    view.update(cx, |view, cx| {
        assert_eq!(
            view.media
                .read(cx)
                .audio_status
                .as_ref()
                .map(|status| status.position_ms),
            Some(fixture.audio.position().as_millis() as u64),
            "every tick must still show the current position"
        );
    });
    report
}

#[gpui::test]
fn video_playback_ticks_render_budget(cx: &mut TestAppContext) {
    let report = video_report(cx);
    report.print();
    assert_eq!(
        report.media, TICKS,
        "each playing tick shows its new frame in one redraw"
    );
    assert_eq!(
        report.window_notifications, 0,
        "ticks notify only the player"
    );
    assert_eq!(report.stats.listing, 0, "the listing stays cached");
    assert_eq!(report.stats.sidebar, 0, "the sidebar stays cached");
}

#[gpui::test]
fn audio_playback_ticks_render_budget(cx: &mut TestAppContext) {
    let report = audio_report(cx);
    report.print();
    assert!(report.media >= TICKS, "each tick shows the new position");
    assert_eq!(
        report.window_notifications, 0,
        "ticks notify only the player"
    );
    assert_eq!(report.stats.listing, 0, "the listing stays cached");
    assert_eq!(report.stats.sidebar, 0, "the sidebar stays cached");
}
