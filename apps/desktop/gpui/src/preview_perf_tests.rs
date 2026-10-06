//! Tests for preview, media and image memory work that has to stay cheap.

use std::fs;
use std::sync::Mutex;

use explorie_native_services::{
    FinderTagsBackend, ResourcePaths, TextHighlight, VideoBackend, VideoPlayback,
};
use gpui::{Resource, TestAppContext, VisualTestContext};
use uuid::Uuid;

use super::tests::POLL_TIMEOUT;
use super::tests::remove_fixture;
use super::*;
use crate::image_memory::{BoundedImageCache, RetiredImages};

fn fixture_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!("explorie-perf-{}", Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    #[cfg(target_os = "macos")]
    let path = path.canonicalize().unwrap();
    path
}

fn file_entry(path: PathBuf) -> FileEntry {
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

fn write_png(path: &Path, width: u32, height: u32) {
    image::RgbaImage::from_pixel(width, height, image::Rgba([40, 120, 200, 255]))
        .save(path)
        .unwrap();
}

fn draw<V: Render>(cx: &mut VisualTestContext, view: &Entity<V>) {
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(1000.0), px(700.0)),
        |_, _| view.clone().into_element(),
    );
}

fn wait_for(
    cx: &mut VisualTestContext,
    view: &Entity<DirectoryWindow>,
    condition: impl Fn(&DirectoryWindow) -> bool,
) {
    // Generous: debug-build image decoding is slow on a loaded machine.
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        cx.run_until_parked();
        if view.update(cx, |view, _| condition(view)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("condition was not reached");
}

fn wait_for_media(
    cx: &mut VisualTestContext,
    view: &Entity<DirectoryWindow>,
    condition: impl Fn(&MediaPlayer) -> bool,
) {
    wait_for(cx, view, |_| true);
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        cx.run_until_parked();
        if view.update(cx, |view, cx| condition(view.media.read(cx))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("media condition was not reached");
}

fn globally_cached(cx: &mut VisualTestContext, path: &Path) -> bool {
    let resource = Resource::from(path.to_path_buf());
    cx.update(|_, cx| cx.has_asset::<gpui::ImgResourceLoader>(&resource))
}

fn test_frame() -> Arc<RenderImage> {
    Arc::new(RenderImage::new([image::Frame::new(
        image::RgbaImage::new(2, 2),
    )]))
}

struct FrameHost {
    retired: RetiredImages,
    renders: usize,
    /// Which render released each image.
    released_at: Vec<(gpui::ImageId, usize)>,
}

impl Render for FrameHost {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        let already_released = self.retired.released.len();
        self.retired.release(window);
        for id in &self.retired.released[already_released..] {
            self.released_at.push((*id, self.renders));
        }
        div()
    }
}

#[gpui::test]
fn retired_frames_are_released_one_render_after_leaving_the_scene(cx: &mut TestAppContext) {
    // GPUI's test atlas is private, so the test observes the ids handed to
    // `Window::drop_image` instead of the atlas contents.
    let (host, cx) = cx.add_window_view(|_, _| FrameHost {
        retired: RetiredImages::default(),
        renders: 0,
        released_at: Vec::new(),
    });
    let first = test_frame();
    let second = test_frame();

    let first_retired_after = host.update(cx, |host, _| {
        host.retired.retire(Some(first.clone()));
        host.renders
    });
    draw(cx, &host);
    let second_retired_after = host.update(cx, |host, _| {
        host.retired.retire(Some(second.clone()));
        host.renders
    });
    for _ in 0..3 {
        draw(cx, &host);
    }
    host.update(cx, |host, _| {
        assert_eq!(host.retired.released, vec![first.id, second.id]);
        assert_eq!(host.retired.pending(), 0);
        // The render right after retirement still presents the old scene;
        // the image may only go at the render after that.
        for (image, retired_after) in [
            (first.id, first_retired_after),
            (second.id, second_retired_after),
        ] {
            let released_at = host
                .released_at
                .iter()
                .find(|(id, _)| *id == image)
                .map(|(_, render)| *render)
                .unwrap();
            assert!(
                released_at >= retired_after + 2,
                "released at render {released_at}, retired after {retired_after}"
            );
        }
    });
}

struct SteppingVideo {
    path: PathBuf,
    position_ms: Mutex<u64>,
}

impl SteppingVideo {
    fn status_at(&self, position_ms: u64) -> VideoStatus {
        VideoStatus {
            path: self.path.clone(),
            duration_ms: Some(10_000),
            position_ms,
            width: 2,
            height: 2,
            playing: false,
            finished: false,
            has_audio: false,
            volume: 0.8,
        }
    }
}

impl VideoPlayback for SteppingVideo {
    fn status(&self) -> VideoStatus {
        self.status_at(*self.position_ms.lock().unwrap())
    }

    fn take_frame(&self) -> Option<VideoFrame> {
        Some(VideoFrame {
            width: 2,
            height: 2,
            position_ms: *self.position_ms.lock().unwrap(),
            bgra: vec![255; 16].into(),
        })
    }

    fn play(&self) -> ServiceResult<VideoStatus> {
        Ok(self.status())
    }

    fn pause(&self) -> ServiceResult<VideoStatus> {
        Ok(self.status())
    }

    fn seek(&self, position: Duration) -> ServiceResult<VideoStatus> {
        *self.position_ms.lock().unwrap() = position.as_millis() as u64;
        Ok(self.status())
    }

    fn set_volume(&self, _: f32) -> ServiceResult<VideoStatus> {
        Ok(self.status())
    }

    fn stop(&self) {}
}

struct SteppingVideoBackend(Arc<SteppingVideo>);

impl VideoBackend for SteppingVideoBackend {
    fn open(&self, _: &Path) -> ServiceResult<Arc<dyn VideoPlayback>> {
        Ok(self.0.clone())
    }
}

#[gpui::test]
fn replaced_and_closed_video_frames_leave_the_sprite_atlas(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let video_path = directory.join("clip.mp4");
    fs::write(&video_path, b"fake decoder input").unwrap();
    let playback = Arc::new(SteppingVideo {
        path: video_path.clone(),
        position_ms: Mutex::new(0),
    });
    let services = NativeServices::with_video_backend(
        ResourcePaths::test(&directory),
        Arc::new(SteppingVideoBackend(playback.clone())),
    );
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(cx, |view, cx| {
        view.browser
            .replace_entries(vec![file_entry(video_path.clone())]);
        view.browser.select(video_path.clone());
        view.preview_selected(cx);
    });
    wait_for_media(cx, &view, |media| media.video_frame.is_some());
    let first = view.update(cx, |view, cx| {
        view.media.read(cx).video_frame.clone().unwrap().id
    });

    *playback.position_ms.lock().unwrap() = 40;
    view.update(cx, |view, cx| {
        view.media
            .update(cx, |media, cx| media.refresh_video_frame(cx))
    });
    wait_for_media(cx, &view, |media| media.video_frame_position_ms == Some(40));
    let second = view.update(cx, |view, cx| {
        let second = view.media.read(cx).video_frame.clone().unwrap().id;
        assert_ne!(second, first);
        second
    });
    draw(cx, &view);
    draw(cx, &view);
    view.update(cx, |view, cx| {
        let retired = view.image_memory.retired.borrow();
        assert!(retired.released.contains(&first));
        assert!(!retired.released.contains(&second));
        drop(retired);
        view.close_preview(cx);
        assert!(view.media.read(cx).video_frame.is_none());
    });
    draw(cx, &view);
    draw(cx, &view);
    view.update(cx, |view, _| {
        let retired = view.image_memory.retired.borrow();
        assert!(retired.released.contains(&second));
        assert_eq!(retired.pending(), 0);
    });
    remove_fixture(&directory);
}

struct Gallery {
    cache: Entity<BoundedImageCache>,
    paths: Vec<PathBuf>,
    first: usize,
    visible: usize,
}

impl Render for Gallery {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let shown = self.paths[self.first..self.first + self.visible].to_vec();
        with_image_cache(
            &self.cache,
            div()
                .flex()
                .children(shown.into_iter().map(|path| img(path).size(px(8.0)))),
        )
    }
}

fn scroll_gallery(
    cx: &mut VisualTestContext,
    gallery: &Entity<Gallery>,
    check: impl Fn(&Gallery, &BoundedImageCache),
) {
    let (count, visible) = gallery.update(cx, |gallery, _| (gallery.paths.len(), gallery.visible));
    for first in (0..=count - visible).step_by(visible) {
        gallery.update(cx, |gallery, _| gallery.first = first);
        draw(cx, gallery);
        cx.run_until_parked();
        draw(cx, gallery);
        gallery.update(cx, |gallery, cx| check(gallery, gallery.cache.read(cx)));
    }
}

#[gpui::test]
fn bounded_image_cache_stays_within_its_item_and_byte_budgets(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let paths = (0..120)
        .map(|index| {
            let path = directory.join(format!("thumb-{index:03}.png"));
            write_png(&path, 4, 4);
            path
        })
        .collect::<Vec<_>>();
    let (gallery, cx) = cx.add_window_view(|_, cx| Gallery {
        cache: cx.new(|cx| BoundedImageCache::new(16, usize::MAX, cx)),
        paths: paths.clone(),
        first: 0,
        visible: 6,
    });
    scroll_gallery(cx, &gallery, |_, cache| {
        assert!(cache.len() <= 16, "{} images cached", cache.len());
    });
    gallery.update(cx, |gallery, cx| {
        let cache = gallery.cache.read(cx);
        assert!(cache.evicted() >= 120 - 16);
        assert_eq!(cache.decoded_bytes(), cache.len() * 4 * 4 * 4);
    });
    assert!(
        !globally_cached(cx, &paths[0]),
        "images inside the scope must bypass GPUI's unbounded asset cache"
    );

    // A byte budget of four decoded 4x4 images, with two visible at a time.
    gallery.update(cx, |gallery, cx| {
        gallery.cache = cx.new(|cx| BoundedImageCache::new(1_000, 4 * 64, cx));
        gallery.visible = 2;
    });
    scroll_gallery(cx, &gallery, |_, cache| {
        assert!(
            cache.decoded_bytes() <= 4 * 64,
            "{} bytes",
            cache.decoded_bytes()
        );
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn visible_images_are_never_evicted_even_past_the_item_budget(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let paths = (0..12)
        .map(|index| {
            let path = directory.join(format!("wide-{index:02}.png"));
            write_png(&path, 4, 4);
            path
        })
        .collect::<Vec<_>>();
    let (gallery, cx) = cx.add_window_view(|_, cx| Gallery {
        cache: cx.new(|cx| BoundedImageCache::new(4, usize::MAX, cx)),
        paths,
        first: 0,
        visible: 12,
    });
    draw(cx, &gallery);
    cx.run_until_parked();
    for _ in 0..3 {
        draw(cx, &gallery);
        cx.run_until_parked();
    }
    gallery.update(cx, |gallery, cx| {
        let cache = gallery.cache.read(cx);
        assert_eq!(cache.len(), 12, "on-screen images must not thrash");
        assert_eq!(cache.evicted(), 0);
    });
    remove_fixture(&directory);
}

#[gpui::test]
fn listing_icons_and_preview_images_decode_through_bounded_caches(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let mut entries = Vec::new();
    let mut icons = Vec::new();
    for index in 0..8 {
        let extension = format!("k{index}");
        let icon = directory.join(format!("icon-{index}.png"));
        write_png(&icon, 16, 16);
        let path = directory.join(format!("item-{index}.{extension}"));
        fs::write(&path, b"item").unwrap();
        entries.push(file_entry(path));
        icons.push((extension, icon));
    }
    let photo = directory.join("photo.png");
    write_png(&photo, 32, 24);
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(cx, |view, cx| {
        view.visuals.icon_loading_enabled = true;
        for (extension, icon) in &icons {
            let key = EntryIconKey::FileKind(extension.clone());
            let ready = EntryIconState::Ready(Some(icon.clone()));
            view.visuals
                .icons
                .insert(key.clone(), ready.clone(), view.visuals.frame);
            view.visuals.icons.set(&key, ready);
        }
        view.browser.replace_entries(entries.clone());
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    draw(cx, &view);
    cx.run_until_parked();
    draw(cx, &view);
    let cached = view.update(cx, |view, cx| {
        view.image_memory.listing_cache(cx).read(cx).len()
    });
    assert_eq!(cached, icons.len());
    for (_, icon) in &icons {
        assert!(!globally_cached(cx, icon));
    }

    view.update(cx, |view, cx| {
        view.settings.view.show_preview_panel = true;
        view.browser
            .replace_entries(vec![file_entry(photo.clone())]);
        view.browser.select(photo.clone());
        view.preview_selected(cx);
    });
    wait_for(cx, &view, |view| {
        matches!(
            &view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::Image(_),
                ..
            }
        )
    });
    draw(cx, &view);
    cx.run_until_parked();
    draw(cx, &view);
    let cached = view.update(cx, |view, cx| {
        view.image_memory.preview_cache(cx).read(cx).len()
    });
    assert_eq!(cached, 1);
    assert!(!globally_cached(cx, &photo));
    remove_fixture(&directory);
}

#[gpui::test]
fn large_photos_preview_a_downscaled_copy_and_small_ones_stay_direct(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let small = directory.join("small.png");
    let large = directory.join("large.png");
    write_png(&small, 640, 480);
    write_png(&large, 2_400, 1_800);
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let cache_dir = services.previews.cache_dir();
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    let preview_image = |cx: &mut VisualTestContext, path: &PathBuf| -> PathBuf {
        view.update(cx, |view, cx| {
            view.browser.replace_entries(vec![file_entry(path.clone())]);
            view.browser.select(path.clone());
            view.preview_selected(cx);
        });
        wait_for(cx, &view, |view| {
            matches!(
                &view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Image(_),
                    ..
                }
            )
        });
        view.update(cx, |view, _| match &view.preview.state {
            PreviewState::Ready {
                path: previewed,
                content: PreviewContent::Image(image),
            } => {
                assert_eq!(previewed, path);
                image.clone()
            }
            state => panic!("unexpected preview state: {state:?}"),
        })
    };

    assert_eq!(preview_image(cx, &small), small);
    let shown = preview_image(cx, &large);
    assert_ne!(shown, large);
    assert!(shown.starts_with(&cache_dir));
    let (width, height) = image::image_dimensions(&shown).unwrap();
    assert!(width <= 2_048 && height <= 2_048, "{width}x{height}");
    assert_eq!(width, 2_048);
    remove_fixture(&directory);
}

/// The per-line highlighting the preview used before documents were cached:
/// clip every valid highlight to the line on every frame.
fn reference_line_spans(
    text: &str,
    highlights: &[TextHighlight],
) -> Vec<Vec<(std::ops::Range<usize>, TextHighlightKind)>> {
    let mut next_line_start = 0;
    text.split('\n')
        .map(|raw_line| {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            let (start, end) = (next_line_start, next_line_start + line.len());
            next_line_start += raw_line.len() + 1;
            let mut spans = highlights
                .iter()
                .filter(|highlight| {
                    highlight.start < highlight.end
                        && highlight.end <= text.len()
                        && text.is_char_boundary(highlight.start)
                        && text.is_char_boundary(highlight.end)
                })
                .filter_map(|highlight| {
                    let clipped_start = highlight.start.max(start);
                    let clipped_end = highlight.end.min(end);
                    (clipped_start < clipped_end)
                        .then(|| (clipped_start - start..clipped_end - start, highlight.kind))
                })
                .collect::<Vec<_>>();
            spans.sort_by_key(|(range, _)| (range.start, range.end));
            spans
        })
        .collect()
}

#[test]
fn cached_text_documents_match_the_per_frame_highlighter() {
    let text = "/* block\r\ncomment */ fn café() {\r\n    let x = \"ß\";\n}\n";
    let highlight = |start, end, kind| TextHighlight { start, end, kind };
    let highlights = vec![
        highlight(0, 20, TextHighlightKind::Comment),
        highlight(21, 23, TextHighlightKind::Keyword),
        highlight(24, 29, TextHighlightKind::Function),
        highlight(24, 27, TextHighlightKind::Type),
        highlight(47, 51, TextHighlightKind::String),
        // Out of range, empty and inside a multi-byte character: dropped.
        highlight(40, 400, TextHighlightKind::Invalid),
        highlight(5, 5, TextHighlightKind::Invalid),
        highlight(27, 28, TextHighlightKind::Invalid),
    ];
    let document = TextPreviewDocument::new(TextPreview {
        text: text.to_string(),
        truncated: false,
        language: Some("Rust".to_string()),
        encoding: "UTF-8".to_string(),
        wrapped: false,
        highlights: highlights.clone(),
    });
    let reference = reference_line_spans(text, &highlights);
    assert_eq!(document.line_count(), reference.len());
    assert_eq!(document.highlight_count(), 5);
    for (line_index, expected) in reference.iter().enumerate() {
        let mut actual = document.line_syntax_spans(line_index).to_vec();
        actual.sort_by_key(|(range, _)| (range.start, range.end));
        assert_eq!(&actual, expected, "line {line_index}");
        let range = document.line_range(line_index).unwrap();
        assert!(!text[range.clone()].contains(['\r', '\n']));
        assert_eq!(document.line_for_offset(range.start), line_index);
    }
    assert!(document.line_syntax_spans(99).is_empty());

    let state = TextPreviewUiState {
        find: "LET".to_string(),
        ..Default::default()
    };
    let matches = state.find_matches(&document);
    assert_eq!(matches.len(), 1);
    assert_eq!(&text[matches[0].clone()], "let");
    assert_eq!(document.line_for_offset(matches[0].start), 2);
    let line = document.line_range(2).unwrap();
    let highlights = find_highlights_in_line(
        &matches,
        line.clone(),
        Some(&matches[0]),
        UiPalette::for_settings(&AppSettings::default(), WindowAppearance::Dark),
    );
    assert_eq!(highlights.len(), 1);
    assert_eq!(
        highlights[0].0,
        matches[0].start - line.start..matches[0].end - line.start
    );
    assert!(
        find_highlights_in_line(
            &matches,
            document.line_range(0).unwrap(),
            None,
            UiPalette::for_settings(&AppSettings::default(), WindowAppearance::Dark)
        )
        .is_empty()
    );
}

fn large_text_preview(lines: usize) -> TextPreview {
    let line = "let value = compute(\"explorie\", 42); // preview line\n";
    let text = line.repeat(lines);
    let highlights = (0..lines.min(50_000))
        .flat_map(|index| {
            let base = index * line.len();
            [
                TextHighlight {
                    start: base,
                    end: base + 3,
                    kind: TextHighlightKind::Keyword,
                },
                TextHighlight {
                    start: base + 20,
                    end: base + 30,
                    kind: TextHighlightKind::String,
                },
            ]
        })
        .collect();
    TextPreview {
        text,
        truncated: false,
        language: Some("Rust".to_string()),
        encoding: "UTF-8".to_string(),
        wrapped: false,
        highlights,
    }
}

fn frame_time<V: Render>(cx: &mut VisualTestContext, view: &Entity<V>) -> Duration {
    let started = Instant::now();
    draw(cx, view);
    started.elapsed()
}

/// Window root that renders nothing. Test windows redraw their root after
/// every update, so timing a `DirectoryWindow` drawn under this root measures
/// only that view.
struct EmptyRoot;

impl Render for EmptyRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[gpui::test]
fn text_preview_frames_do_not_scale_with_file_size(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let path = directory.join("large.rs");
    let (_, cx) = cx.add_window_view(|_, _| EmptyRoot);
    let text_view = |cx: &mut VisualTestContext, preview: TextPreview, wrap: bool| {
        let document = Arc::new(TextPreviewDocument::new(preview));
        cx.update(|_, cx| {
            let services = NativeServices::new(ResourcePaths::test(&directory));
            cx.new(|cx| {
                let mut view = DirectoryWindow::new(directory.clone(), services, cx);
                view.settings.view.show_preview_panel = true;
                view.preview.text.wrap_override = Some(wrap);
                view.preview.text.find = "explorie".to_string();
                view.preview.state = PreviewState::Ready {
                    path: path.clone(),
                    content: PreviewContent::Text(document),
                };
                view
            })
        })
    };

    // About 8 MiB with 100k highlights: the old render copied, compared and
    // rescanned all of it on every frame.
    let large_bytes = large_text_preview(160_000).text.len();
    assert!(large_bytes >= 8 * 1024 * 1024);
    for wrap in [false, true] {
        let view = text_view(cx, large_text_preview(160_000), wrap);
        draw(cx, &view);
        let scanned = text_preview_scanned_bytes();
        for _ in 0..4 {
            draw(cx, &view);
        }
        assert_eq!(
            text_preview_scanned_bytes(),
            scanned,
            "frames must not rescan the text (wrap: {wrap})"
        );
        view.update(cx, |view, _| {
            assert_eq!(view.text_preview_match_count(), 160_000);
            view.preview.text.find = "compute".to_string();
        });
        draw(cx, &view);
        draw(cx, &view);
        assert_eq!(
            text_preview_scanned_bytes(),
            scanned + large_bytes,
            "a new query scans the text exactly once"
        );
    }

    // Frame cost is flat: an 8 MiB document renders about as fast as a tiny
    // one. Frames of the two are interleaved so machine load hits both
    // alike, and the fastest frame of each is compared.
    let small_view = text_view(cx, large_text_preview(40), false);
    let large_view = text_view(cx, large_text_preview(160_000), false);
    draw(cx, &small_view);
    draw(cx, &large_view);
    let (mut small_frame, mut large_frame) = (Duration::MAX, Duration::MAX);
    for _ in 0..9 {
        small_frame = small_frame.min(frame_time(cx, &small_view));
        large_frame = large_frame.min(frame_time(cx, &large_view));
    }
    assert!(
        large_frame <= small_frame * 2 + Duration::from_millis(10),
        "small {small_frame:?}, large {large_frame:?}"
    );
    remove_fixture(&directory);
}

#[derive(Default)]
struct CountingFinderTags {
    requested: Mutex<Vec<PathBuf>>,
}

impl FinderTagsBackend for CountingFinderTags {
    fn supported(&self) -> bool {
        true
    }

    fn get(&self, path: &Path) -> std::io::Result<Vec<String>> {
        self.requested.lock().unwrap().push(path.to_path_buf());
        Ok(Vec::new())
    }

    fn set(&self, _: &Path, _: &[String]) -> std::io::Result<()> {
        Ok(())
    }
}

#[gpui::test]
fn held_arrow_keys_only_preview_where_the_selection_settles(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let files = (0..10)
        .map(|index| {
            let path = directory.join(format!("note-{index}.txt"));
            fs::write(&path, format!("note {index}")).unwrap();
            path
        })
        .collect::<Vec<_>>();
    let tags = Arc::new(CountingFinderTags::default());
    let services =
        NativeServices::with_finder_tags_backend(ResourcePaths::test(&directory), tags.clone());
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    let requested = |tags: &CountingFinderTags| tags.requested.lock().unwrap().clone();
    view.update(cx, |view, cx| {
        view.settings.view.show_preview_panel = true;
        view.browser
            .replace_entries(files.iter().cloned().map(file_entry).collect());
        view.browser.select(files[0].clone());
        view.sync_pinned_preview(cx);
    });
    wait_for(cx, &view, |view| {
        matches!(view.preview.state, PreviewState::Ready { .. })
    });

    // A single key press previews immediately.
    view.update(cx, |view, cx| {
        view.select_next(cx);
        assert!(view.preview.debounce.task.is_none());
        assert_eq!(view.preview.state.path(), Some(files[1].as_path()));
    });
    // Key repeats (no time passes on the test clock) only move the selection.
    view.update(cx, |view, cx| {
        for _ in 0..8 {
            view.select_next(cx);
        }
        assert!(view.preview.debounce.task.is_some());
        assert!(matches!(
            &view.preview.state,
            PreviewState::Loading { path } if path == &files[9]
        ));
    });
    cx.run_until_parked();
    let passed_over = &files[2..9];
    assert!(
        requested(&tags)
            .iter()
            .all(|path| !passed_over.contains(path)),
        "no jobs for files passed over: {:?}",
        requested(&tags)
    );

    cx.executor().advance_clock(KEYBOARD_PREVIEW_DEBOUNCE * 2);
    wait_for(cx, &view, |view| {
        matches!(
            &view.preview.state,
            PreviewState::Ready { path, .. } if path == &files[9]
        )
    });
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        if requested(&tags).contains(&files[9]) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let requested_now = requested(&tags);
    assert!(requested_now.contains(&files[0]));
    assert!(requested_now.contains(&files[9]));
    assert!(requested_now.iter().all(|path| !passed_over.contains(path)));

    // A click during a burst is immediate and cancels the pending preview.
    view.update(cx, |view, cx| {
        view.select_previous(cx);
        view.select_previous(cx);
        assert!(view.preview.debounce.task.is_some());
        view.browser.select(files[3].clone());
        view.sync_pinned_preview(cx);
        assert!(view.preview.debounce.task.is_none());
    });
    cx.executor().advance_clock(KEYBOARD_PREVIEW_DEBOUNCE * 2);
    wait_for(cx, &view, |view| {
        matches!(
            &view.preview.state,
            PreviewState::Ready { path, .. } if path == &files[3]
        )
    });
    assert!(!requested(&tags).contains(&files[7]));
    remove_fixture(&directory);
}

#[gpui::test]
fn quick_look_key_repeats_are_debounced_but_clicks_are_not(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let files = (0..5)
        .map(|index| {
            let path = directory.join(format!("page-{index}.txt"));
            fs::write(&path, format!("page {index}")).unwrap();
            path
        })
        .collect::<Vec<_>>();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, cx) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(cx, |view, cx| {
        view.browser
            .replace_entries(files.iter().cloned().map(file_entry).collect());
        view.browser.select(files[0].clone());
        view.open_quick_look(files[0].clone(), files.clone(), cx);

        view.navigate_preview(1, cx);
        assert!(
            view.preview.debounce.task.is_none(),
            "first key press is immediate"
        );
        view.navigate_preview(1, cx);
        assert!(
            view.preview.debounce.task.is_some(),
            "key repeat is deferred"
        );
        assert_eq!(view.preview.state.path(), Some(files[2].as_path()));

        view.navigate_preview_by_click(1, cx);
        assert!(
            view.preview.debounce.task.is_none(),
            "clicks load immediately"
        );
        assert_eq!(view.preview.state.path(), Some(files[3].as_path()));
    });
    wait_for(cx, &view, |view| {
        matches!(
            &view.preview.state,
            PreviewState::Ready { path, .. } if path == &files[3]
        )
    });
    remove_fixture(&directory);
}
