//! Grid layout, selection marquee, and drag payload types.

use crate::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GridLayoutMetrics {
    pub(crate) columns: usize,
    pub(crate) item_width: f32,
    pub(crate) item_height: f32,
    pub(crate) gap: f32,
    pub(crate) row_height: f32,
    pub(crate) thumbnail_height: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct GridPoint {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct GridRect {
    pub(crate) left: f32,
    pub(crate) top: f32,
    pub(crate) right: f32,
    pub(crate) bottom: f32,
}

impl GridRect {
    pub(crate) fn between(start: GridPoint, end: GridPoint) -> Self {
        Self {
            left: start.x.min(end.x),
            top: start.y.min(end.y),
            right: start.x.max(end.x),
            bottom: start.y.max(end.y),
        }
    }

    pub(crate) fn width(self) -> f32 {
        self.right - self.left
    }

    pub(crate) fn height(self) -> f32 {
        self.bottom - self.top
    }

    pub(crate) fn intersects(self, other: Self) -> bool {
        !(self.right < other.left
            || self.left > other.right
            || self.bottom < other.top
            || self.top > other.bottom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MarqueeLayout {
    List { row_height: f32 },
    Grid(GridLayoutMetrics),
    Column { index: usize, row_height: f32 },
}

#[derive(Clone, Debug)]
pub(crate) struct SelectionMarquee {
    pub(crate) start_window: GridPoint,
    pub(crate) start_content: GridPoint,
    pub(crate) current_content: GridPoint,
    pub(crate) activated: bool,
    pub(crate) additive: bool,
    pub(crate) layout: MarqueeLayout,
    pub(crate) initial_selection: BTreeSet<PathBuf>,
    pub(crate) paths: Vec<PathBuf>,
}

impl SelectionMarquee {
    pub(crate) fn content_rect(&self) -> GridRect {
        GridRect::between(self.start_content, self.current_content)
    }

    pub(crate) fn update(&mut self, window: GridPoint, content: GridPoint) {
        self.current_content = content;
        self.activated |= (window.x - self.start_window.x).abs() >= SELECTION_MARQUEE_THRESHOLD
            || (window.y - self.start_window.y).abs() >= SELECTION_MARQUEE_THRESHOLD;
    }
}

pub(crate) fn grid_card_rect(index: usize, metrics: GridLayoutMetrics) -> GridRect {
    let row = index / metrics.columns.max(1);
    let column = index % metrics.columns.max(1);
    let left = GRID_HORIZONTAL_PADDING + column as f32 * (metrics.item_width + metrics.gap);
    let top = row as f32 * metrics.row_height;
    GridRect {
        left,
        top,
        right: left + metrics.item_width,
        bottom: top + metrics.item_height,
    }
}

pub(crate) fn grid_marquee_hit_indices(
    marquee: GridRect,
    item_count: usize,
    metrics: GridLayoutMetrics,
) -> Vec<usize> {
    (0..item_count)
        .filter(|index| marquee.intersects(grid_card_rect(*index, metrics)))
        .collect()
}

pub(crate) fn row_marquee_hit_indices(
    marquee: GridRect,
    item_count: usize,
    row_height: f32,
    viewport_width: f32,
) -> Vec<usize> {
    (0..item_count)
        .filter(|index| {
            let top = *index as f32 * row_height;
            marquee.intersects(GridRect {
                left: 0.0,
                top,
                right: viewport_width,
                bottom: top + row_height,
            })
        })
        .collect()
}

pub(crate) fn selection_marquee_scroll_delta(pointer_y: f32, viewport_height: f32) -> f32 {
    if pointer_y < SELECTION_MARQUEE_EDGE_ZONE {
        SELECTION_MARQUEE_SCROLL_STEP
    } else if pointer_y > viewport_height - SELECTION_MARQUEE_EDGE_ZONE {
        -SELECTION_MARQUEE_SCROLL_STEP
    } else {
        0.0
    }
}

pub(crate) fn file_drag_scroll_delta(pointer_y: f32, viewport_height: f32) -> f32 {
    if pointer_y < FILE_DRAG_EDGE_ZONE {
        FILE_DRAG_SCROLL_STEP
    } else if pointer_y > viewport_height - FILE_DRAG_EDGE_ZONE {
        -FILE_DRAG_SCROLL_STEP
    } else {
        0.0
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FavoriteDrag {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) position: gpui::Point<gpui::Pixels>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileDragItem {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) is_dir: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct FileDrag {
    pub(crate) items: Arc<OnceLock<Vec<FileDragItem>>>,
    pub(crate) position: gpui::Point<gpui::Pixels>,
    // Only the Windows and macOS native drag-out path reads this.
    #[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
    pub(crate) native_export_attempted: Arc<AtomicBool>,
}

#[cfg(test)]
thread_local! {
    pub(crate) static MULTI_FILE_DRAG_BUILDS: Cell<usize> = const { Cell::new(0) };
}

impl FileDrag {
    pub(crate) fn from_entries(entries: Vec<FileEntry>) -> Self {
        #[cfg(test)]
        if entries.len() > 1 {
            MULTI_FILE_DRAG_BUILDS.with(|count| count.set(count.get() + 1));
        }
        Self {
            items: Arc::new(OnceLock::from(
                entries
                    .into_iter()
                    .map(|entry| FileDragItem {
                        name: file_name(&entry),
                        path: entry.path,
                        is_dir: entry.is_dir,
                    })
                    .collect::<Vec<_>>(),
            )),
            position: gpui::Point::default(),
            native_export_attempted: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn deferred() -> Self {
        Self {
            items: Arc::default(),
            position: gpui::Point::default(),
            native_export_attempted: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn items(&self) -> &[FileDragItem] {
        self.items.get().map_or(&[], Vec::as_slice)
    }

    pub(crate) fn initialize(
        &self,
        source: &FileEntry,
        selection: &BTreeSet<PathBuf>,
        entries: &[Arc<FileEntry>],
    ) {
        self.items.get_or_init(|| {
            let item = |entry: &FileEntry| FileDragItem {
                name: file_name(entry),
                path: entry.path.clone(),
                is_dir: entry.is_dir,
            };
            let mut items = if selection.contains(&source.path) {
                entries
                    .iter()
                    .filter(|entry| selection.contains(&entry.path))
                    .map(|entry| item(entry))
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            if items.is_empty() {
                items.push(item(source));
            }
            #[cfg(test)]
            if items.len() > 1 {
                MULTI_FILE_DRAG_BUILDS.with(|count| count.set(count.get() + 1));
            }
            items
        });
    }

    pub(crate) fn at(mut self, position: gpui::Point<gpui::Pixels>) -> Self {
        self.position = position;
        self
    }
}

impl Render for FileDrag {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let modifiers = window.modifiers();
        let copy = modifiers.control || modifiers.alt;
        let visible = self.items().iter().take(4);
        let count = self.items().len();
        div()
            .pl(self.position.x - px(86.0))
            .pt(self.position.y - px(22.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(172.0))
                    .px_3()
                    .py_2()
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(0x4ea1ff))
                    .bg(with_alpha(rgb(0x182430), 0.94))
                    .shadow_md()
                    .children(visible.map(|item| {
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .child(if item.is_dir { "▰" } else { "▪" })
                            .child(div().min_w_0().truncate().child(item.name.clone()))
                    }))
                    .child(
                        div()
                            .mt_1()
                            .pt_1()
                            .border_t_1()
                            .border_color(rgb(0x3d6387))
                            .text_xs()
                            .text_color(rgb(0x9bc9f5))
                            .child(format!(
                                "{} {count} item{}",
                                if copy { "Copy" } else { "Move" },
                                if count == 1 { "" } else { "s" }
                            )),
                    ),
            )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FileDragHoverTarget {
    Folder(PathBuf),
    Tab(TabId),
}

pub(crate) fn comparable_drop_path(path: &Path) -> String {
    let mut normalized = path.to_string_lossy().replace('\\', "/");
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    #[cfg(windows)]
    normalized.make_ascii_lowercase();
    normalized
}

pub(crate) fn valid_file_drop_target(
    target: &Path,
    drag: &FileDrag,
    kind: FileOperationKind,
) -> bool {
    if drag.items().is_empty() {
        return false;
    }
    let target = comparable_drop_path(target);
    if kind == FileOperationKind::Move
        && drag.items().iter().all(|item| {
            item.path
                .parent()
                .is_some_and(|parent| comparable_drop_path(parent) == target)
        })
    {
        return false;
    }
    drag.items().iter().all(|item| {
        let source = comparable_drop_path(&item.path);
        target != source && (!item.is_dir || !target.starts_with(&format!("{source}/")))
    })
}

pub(crate) fn file_drag_operation(window: &Window) -> FileOperationKind {
    let modifiers = window.modifiers();
    if modifiers.control || modifiers.alt {
        FileOperationKind::Copy
    } else {
        FileOperationKind::Move
    }
}

/// What dropping `sources` on the folder `target` does, as in Finder and
/// Explorer: within one volume a drop moves and across volumes it copies.
/// Option (Ctrl on Windows) forces a copy, Command (Shift on Windows) forces
/// a move. Sources whose volume cannot be determined copy, which never
/// removes anything. This checks the filesystem, so external drops call it
/// off the UI thread.
pub(crate) fn drop_operation(
    modifiers: gpui::Modifiers,
    sources: &[PathBuf],
    target: &Path,
) -> FileOperationKind {
    if modifiers.alt || modifiers.control {
        return FileOperationKind::Copy;
    }
    let force_move = if cfg!(target_os = "macos") {
        modifiers.platform
    } else {
        modifiers.shift
    };
    if force_move {
        return FileOperationKind::Move;
    }
    let Some(target_volume) = volume_of(target, true) else {
        return FileOperationKind::Copy;
    };
    if sources
        .iter()
        .all(|source| volume_of(source, false).is_some_and(|volume| volume == target_volume))
    {
        FileOperationKind::Move
    } else {
        FileOperationKind::Copy
    }
}

/// An identifier of the volume holding `path`: the item itself for a source
/// (a link lives where the link is), what it resolves to for a target.
#[cfg(unix)]
fn volume_of(path: &Path, follow: bool) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let metadata = if follow {
        std::fs::metadata(path)
    } else {
        std::fs::symlink_metadata(path)
    };
    metadata.ok().map(|metadata| metadata.dev())
}

/// An identifier of the volume holding `path`: its drive or UNC share.
#[cfg(windows)]
fn volume_of(path: &Path, follow: bool) -> Option<String> {
    let resolved = if follow {
        std::fs::canonicalize(path).ok()?
    } else {
        // The item itself must exist (a link is judged where it lives, so
        // its parent is resolved); a missing source has no volume.
        std::fs::symlink_metadata(path).ok()?;
        std::fs::canonicalize(path.parent()?).ok()?
    };
    match resolved.components().next()? {
        std::path::Component::Prefix(prefix) => {
            Some(prefix.as_os_str().to_string_lossy().to_ascii_lowercase())
        }
        _ => None,
    }
}

impl FavoriteDrag {
    pub(crate) fn new(path: PathBuf, name: String) -> Self {
        Self {
            path,
            name,
            position: gpui::Point::default(),
        }
    }

    pub(crate) fn at(mut self, position: gpui::Point<gpui::Pixels>) -> Self {
        self.position = position;
        self
    }
}

impl Render for FavoriteDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .pl(self.position.x - px(70.0))
            .pt(self.position.y - px(16.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .w(px(140.0))
                    .h(px(32.0))
                    .px_2()
                    .border_1()
                    .border_color(rgb(0x4ea1ff))
                    .bg(with_alpha(rgb(0x264f78), 0.92))
                    .text_sm()
                    .shadow_md()
                    .child("★")
                    .child(div().min_w_0().truncate().child(self.name.clone())),
            )
    }
}

pub(crate) fn grid_layout_metrics(
    container_width: f32,
    grid_min_width: u16,
    density: Density,
    ui_scale: f32,
) -> GridLayoutMetrics {
    let item_width = f32::from(grid_min_width);
    let thumbnail_height = (72.0 * ui_scale).clamp(52.0, 96.0);
    // A card stacks the thumbnail, up to two lines of `text_sm` name and a
    // `text_xs` detail line, separated by `gap_1`, inside a 1px border. The
    // window's rem size is 16px times the UI scale, and GPUI's default line
    // height is phi times the font size. Narrow cards used to be shorter than
    // that, so two-line names pushed the size into the next row.
    let rem = 16.0 * ui_scale;
    let line_height = |font_rems: f32| (font_rems * rem * 1.618_034).round();
    let content_height = thumbnail_height
        + 2.0 * line_height(0.875)
        + line_height(0.75)
        + 2.0 * (0.25 * rem).round()
        + 2.0
        + (8.0 * ui_scale).round();
    let item_height = (item_width * 0.85).round().max(content_height.ceil());
    let gap = ((if density == Density::Compact {
        8.0
    } else {
        12.0
    }) * ui_scale)
        .round()
        .max(1.0);
    let columns = (container_width.max(0.0) / (item_width + gap))
        .floor()
        .max(1.0) as usize;
    GridLayoutMetrics {
        columns,
        item_width,
        item_height,
        gap,
        row_height: item_height + gap,
        thumbnail_height,
    }
}
