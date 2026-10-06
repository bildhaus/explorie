//! File icons, grid visuals, and entry formatting.

use crate::*;

pub(crate) fn selected_control_color(selected: bool, palette: UiPalette) -> gpui::Rgba {
    if selected {
        palette.selected
    } else {
        palette.control
    }
}

pub(crate) fn file_operation_label(kind: FileOperationKind) -> &'static str {
    match kind {
        FileOperationKind::Copy => "Copy",
        FileOperationKind::Move => "Move",
        FileOperationKind::Trash => "Trash",
    }
}

pub(crate) fn conflict_policy_label(policy: ConflictPolicy) -> &'static str {
    match policy {
        ConflictPolicy::Error => "ask",
        ConflictPolicy::Rename => "keep both",
        ConflictPolicy::Replace => "replace",
    }
}

pub(crate) fn sort_label(browser: &BrowserState, key: &SortKey) -> String {
    if browser.sort_key() == *key {
        format!("{} {}", key.label(), browser.sort_direction().indicator())
    } else {
        let label = key.label();
        let mut characters = label.chars();
        characters.next().map_or_else(String::new, |first| {
            first.to_uppercase().collect::<String>() + characters.as_str()
        })
    }
}

/// List view widths for every column but Name, which takes what they leave.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ListColumnWidths {
    pub(crate) custom: Vec<f32>,
    pub(crate) size: f32,
    pub(crate) modified: f32,
}

/// Name gets at least this much before the other columns give up any of
/// their preferred width; ~12 characters of the default font plus the icon.
const LIST_NAME_TARGET_WIDTH: f32 = 200.0;
const LIST_SIZE_WIDTH: (f32, f32) = (80.0, 110.0);
const LIST_MODIFIED_WIDTH: (f32, f32) = (118.0, 130.0);
const LIST_CUSTOM_WIDTH: (f32, f32) = (56.0, 120.0);

/// Lays out the list's fixed columns for a `viewport_width` list. Widths the
/// user set (`Some`) are kept. The rest take their preferred widths while Name
/// still gets [`LIST_NAME_TARGET_WIDTH`], then shrink together toward their
/// minimums (custom columns the most) so Name keeps the remaining space.
pub(crate) fn list_column_widths(
    viewport_width: f32,
    size: Option<f32>,
    modified: Option<f32>,
    custom: &[Option<f32>],
) -> ListColumnWidths {
    // Without custom columns the built-ins keep their roomier legacy widths.
    let (size_range, modified_range) = if custom.is_empty() {
        ((LIST_SIZE_WIDTH.0, 120.0), (LIST_MODIFIED_WIDTH.0, 140.0))
    } else {
        (LIST_SIZE_WIDTH, LIST_MODIFIED_WIDTH)
    };
    let columns = custom
        .iter()
        .map(|width| (*width, LIST_CUSTOM_WIDTH))
        .chain([(size, size_range), (modified, modified_range)])
        .collect::<Vec<_>>();
    let fixed: f32 = columns.iter().filter_map(|(width, _)| *width).sum();
    let (minimum, preferred) = columns
        .iter()
        .filter(|(width, _)| width.is_none())
        .fold((0.0, 0.0), |(minimum, preferred), (_, range)| {
            (minimum + range.0, preferred + range.1)
        });
    let available = viewport_width - SELECTION_MARQUEE_GUTTER - fixed - LIST_NAME_TARGET_WIDTH;
    let share = if preferred > minimum {
        ((available - minimum) / (preferred - minimum)).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let mut widths = columns.into_iter().map(|(width, (minimum, preferred))| {
        width.unwrap_or_else(|| (minimum + (preferred - minimum) * share).floor())
    });
    let custom = widths.by_ref().take(custom.len()).collect();
    let size = widths.next().unwrap_or(size_range.1);
    let modified = widths.next().unwrap_or(modified_range.1);
    ListColumnWidths {
        custom,
        size,
        modified,
    }
}

/// How byte counts are scaled for display.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SizeUnits {
    /// Finder: 1 KB = 1000 bytes.
    Decimal,
    /// Explorer: 1 KB = 1024 bytes, still labelled KB.
    Binary,
}

impl SizeUnits {
    pub(crate) const PLATFORM: Self = if cfg!(target_os = "macos") {
        Self::Decimal
    } else {
        Self::Binary
    };

    fn base(self) -> f64 {
        match self {
            Self::Decimal => 1000.0,
            Self::Binary => 1024.0,
        }
    }

    /// Scales `bytes` to the largest unit below `units.len()` whose value,
    /// rounded to the precision `decimals` picks, stays under one step up,
    /// so 999,999 bytes reads "1.0 MB" rather than "1000.0 KB".
    fn scale(
        self,
        bytes: u64,
        units: usize,
        decimals: impl Fn(usize, f64) -> usize,
    ) -> (f64, usize) {
        let base = self.base();
        let mut value = bytes as f64;
        let mut unit = 0;
        while unit + 1 < units {
            let factor = 10_f64.powi(decimals(unit, value) as i32);
            if (value * factor).round() / factor < base {
                break;
            }
            value /= base;
            unit += 1;
        }
        (value, unit)
    }
}

pub(crate) fn format_size(bytes: u64) -> String {
    format_size_in(bytes, SizeUnits::PLATFORM)
}

pub(crate) fn format_size_in(bytes: u64, units: SizeUnits) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (value, unit) = units.scale(bytes, UNITS.len(), |unit, _| usize::from(unit > 0));
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub(crate) fn format_preview_size(bytes: u64) -> String {
    format_preview_size_in(bytes, SizeUnits::PLATFORM)
}

pub(crate) fn format_preview_size_in(bytes: u64, units: SizeUnits) -> String {
    if bytes == 0 {
        return "0 Bytes".to_string();
    }
    const UNITS: [&str; 5] = ["Bytes", "KB", "MB", "GB", "TB"];
    let decimals = |unit: usize, value: f64| usize::from(unit > 0 && value < 10.0);
    let (value, unit) = units.scale(bytes, UNITS.len(), decimals);
    format!(
        "{value:.precision$} {}",
        UNITS[unit],
        precision = decimals(unit, value)
    )
}

pub(crate) fn format_metadata_modified(modified: SystemTime) -> String {
    let date: DateTime<Local> = modified.into();
    date.format("%Y-%m-%d %H:%M:%S").to_string()
}

pub(crate) fn metadata_row(
    label: impl Into<String>,
    value: impl Into<String>,
    palette: UiPalette,
) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap_3()
        .text_sm()
        .child(
            div()
                .w(px(96.0))
                .min_w(px(96.0))
                .text_color(palette.muted)
                .child(label.into()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(palette.text)
                .child(value.into()),
        )
        .into_any_element()
}

pub(crate) fn photo_metadata_rows(
    metadata: &ImageMetadata,
    gps_revealed: bool,
) -> Vec<(&'static str, String)> {
    let mut rows = Vec::with_capacity(7);
    if let Some(camera) = &metadata.camera {
        rows.push(("Camera", camera.clone()));
    }
    if let Some(taken_at) = &metadata.taken_at {
        rows.push(("Taken", format_exif_date(taken_at)));
    }
    if let (Some(width), Some(height)) = (metadata.width, metadata.height) {
        rows.push(("Dimensions", format!("{width} × {height}")));
    }
    if let Some(caption) = &metadata.caption {
        rows.push(("Caption", caption.clone()));
    }
    if !metadata.keywords.is_empty() {
        rows.push(("Keywords", metadata.keywords.join(", ")));
    }
    if gps_revealed && let Some(gps) = metadata.gps {
        rows.push(("Latitude", format!("{:.6}", gps.latitude)));
        rows.push(("Longitude", format!("{:.6}", gps.longitude)));
    }
    rows
}

pub(crate) fn entry_icon_visual(
    entry: &FileEntry,
    native_icon: Option<PathBuf>,
    size: f32,
    palette: UiPalette,
) -> AnyElement {
    // macOS system icons of links already carry Finder's alias arrow.
    let icon_shows_link = cfg!(target_os = "macos") && native_icon.is_some();
    let base = if let Some(path) = native_icon {
        img(path).w(px(size)).h(px(size)).into_any_element()
    } else if entry.is_dir {
        fallback_folder_icon(size, palette)
    } else {
        fallback_file_icon(&entry.path, size, palette)
    };

    let link_badge = entry_link_badge(entry).filter(|_| !icon_shows_link);
    div()
        .relative()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .w(px(size))
        .h(px(size))
        .child(base)
        .when_some(link_badge, |icon, badge| {
            let badge_size = (size * 0.46).clamp(9.0, 15.0);
            icon.child(
                div()
                    .absolute()
                    .right_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(badge_size))
                    .h(px(badge_size))
                    .rounded_sm()
                    .border_1()
                    .border_color(palette.border)
                    .bg(palette.window)
                    .text_color(palette.accent)
                    .text_size(px((badge_size * 0.72).max(8.0)))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(badge),
            )
        })
        .into_any_element()
}

pub(crate) fn fallback_folder_icon(size: f32, palette: UiPalette) -> AnyElement {
    let (front, back, highlight, outline) = if palette.dark {
        (rgb(0xdda846), rgb(0xb9791f), rgb(0xf0c66b), rgb(0x8d5a16))
    } else {
        (rgb(0xd69225), rgb(0xb86e16), rgb(0xefb44d), rgb(0x92530e))
    };
    let inset = size * 0.04;
    let radius = (size * 0.12).clamp(2.0, 6.0);

    div()
        .relative()
        .w(px(size))
        .h(px(size))
        .debug_selector(|| "fallback-icon-folder".to_string())
        .child(
            div()
                .absolute()
                .left(px(inset))
                .top(px(size * 0.12))
                .w(px(size * 0.46))
                .h(px(size * 0.28))
                .rounded(px(radius * 0.75))
                .border_1()
                .border_color(outline)
                .bg(back),
        )
        .child(
            div()
                .absolute()
                .left(px(inset))
                .top(px(size * 0.28))
                .w(px(size - inset * 2.0))
                .h(px(size * 0.68))
                .rounded(px(radius))
                .border_1()
                .border_color(outline)
                .bg(front),
        )
        .child(
            div()
                .absolute()
                .left(px(size * 0.12))
                .top(px(size * 0.38))
                .w(px(size * 0.76))
                .h(px((size * 0.06).max(1.0)))
                .rounded_full()
                .bg(highlight),
        )
        .into_any_element()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FallbackFileIconKind {
    Image,
    Audio,
    Video,
    Archive,
    Code,
    Pdf,
    Document,
    Spreadsheet,
    Presentation,
    Generic,
}

impl FallbackFileIconKind {
    pub(crate) fn debug_name(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Archive => "archive",
            Self::Code => "code",
            Self::Pdf => "pdf",
            Self::Document => "document",
            Self::Spreadsheet => "spreadsheet",
            Self::Presentation => "presentation",
            Self::Generic => "generic",
        }
    }
}

pub(crate) struct FallbackFileIconSpec {
    pub(crate) kind: FallbackFileIconKind,
    pub(crate) label: String,
    pub(crate) accent: Rgba,
}

pub(crate) fn fallback_file_icon(path: &Path, size: f32, palette: UiPalette) -> AnyElement {
    let spec = fallback_file_icon_spec(path, palette.dark);
    let debug_name = spec.kind.debug_name();
    let paper = if palette.dark {
        rgb(0x2c3239)
    } else {
        rgb(0xf7f9fb)
    };
    let outline = if palette.dark {
        rgb(0x707a85)
    } else {
        rgb(0x89939e)
    };
    let x = size * 0.12;
    let y = size * 0.04;
    let width = size * 0.76;
    let height = size * 0.92;
    let radius = (size * 0.1).clamp(1.5, 5.0);
    let label_size = if spec.label.len() > 3 {
        (size * 0.18).clamp(5.5, 9.0)
    } else {
        (size * 0.22).clamp(6.5, 11.0)
    };

    div()
        .relative()
        .w(px(size))
        .h(px(size))
        .debug_selector(move || format!("fallback-icon-{debug_name}"))
        .child(
            div()
                .absolute()
                .left(px(x))
                .top(px(y))
                .flex()
                .items_center()
                .justify_center()
                .w(px(width))
                .h(px(height))
                .rounded(px(radius))
                .border_1()
                .border_color(outline)
                .bg(paper)
                .text_color(spec.accent)
                .text_size(px(label_size))
                .font_weight(FontWeight::SEMIBOLD)
                .child(spec.label),
        )
        .child(
            div()
                .absolute()
                .left(px(size * 0.25))
                .top(px(size * 0.75))
                .w(px(size * 0.5))
                .h(px((size * 0.07).max(1.0)))
                .rounded_full()
                .bg(with_alpha(
                    spec.accent,
                    if palette.dark { 0.34 } else { 0.24 },
                )),
        )
        .into_any_element()
}

pub(crate) fn grid_entry_visual(
    entry: &FileEntry,
    thumbnail: Option<EntryThumbnailState>,
    native_icon: Option<PathBuf>,
    icon_size: f32,
    thumbnail_height: f32,
    palette: UiPalette,
) -> AnyElement {
    match thumbnail {
        Some(EntryThumbnailState::Ready(path)) => div()
            .flex()
            .w_full()
            .h(px(thumbnail_height))
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .rounded_sm()
            .child(img(path).w_full().h_full().object_fit(ObjectFit::Contain))
            .into_any_element(),
        Some(EntryThumbnailState::Loading) => div()
            .flex()
            .w_full()
            .h(px(thumbnail_height))
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .rounded_sm()
            .border_1()
            .border_color(palette.border)
            .bg(palette.control)
            .text_xs()
            .text_color(palette.muted)
            .child("Loading…")
            .into_any_element(),
        Some(EntryThumbnailState::Failed) => div()
            .flex()
            .w_full()
            .h(px(thumbnail_height))
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .rounded_sm()
            .border_1()
            .border_color(rgb(0xc94f5d))
            .bg(with_alpha(
                rgb(0xc94f5d),
                if palette.dark { 0.12 } else { 0.07 },
            ))
            .debug_selector(|| "thumbnail-failed-card".to_string())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .px_2()
                    .text_center()
                    .text_xs()
                    .line_height(px(14.0 * palette.scale))
                    .text_color(rgb(0xe06c78))
                    .child(
                        div()
                            .debug_selector(|| "thumbnail-failed-primary".to_string())
                            .child("Thumbnail"),
                    )
                    .child(
                        div()
                            .debug_selector(|| "thumbnail-failed-secondary".to_string())
                            .child("unavailable"),
                    ),
            )
            .into_any_element(),
        None => entry_icon_visual(entry, native_icon, icon_size, palette),
    }
}

pub(crate) fn entry_link_badge(entry: &FileEntry) -> Option<&'static str> {
    if entry.is_junction {
        Some("⤴")
    } else if entry.is_symlink {
        Some("↗")
    } else {
        None
    }
}

pub(crate) fn fallback_file_icon_spec(path: &Path, dark: bool) -> FallbackFileIconSpec {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (kind, label, accent) = match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "heic" => (
            FallbackFileIconKind::Image,
            "IMG",
            if dark { rgb(0xc58af9) } else { rgb(0x7e3bb5) },
        ),
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "wma" => (
            FallbackFileIconKind::Audio,
            "AUD",
            if dark { rgb(0xf080b7) } else { rgb(0xb32970) },
        ),
        "mp4" | "webm" | "m4v" | "mov" | "avi" | "mkv" | "wmv" | "flv" => (
            FallbackFileIconKind::Video,
            "VID",
            if dark { rgb(0x72b7f2) } else { rgb(0x1769aa) },
        ),
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" => (
            FallbackFileIconKind::Archive,
            "ZIP",
            if dark { rgb(0xf2a65a) } else { rgb(0xb75b00) },
        ),
        "rs" | "js" | "jsx" | "ts" | "tsx" | "html" | "css" | "json" | "toml" | "yaml" | "yml"
        | "py" | "go" | "swift" | "kt" | "java" | "c" | "cpp" | "h" | "hpp" => (
            FallbackFileIconKind::Code,
            "DEV",
            if dark { rgb(0x72d39b) } else { rgb(0x167442) },
        ),
        "pdf" => (
            FallbackFileIconKind::Pdf,
            "PDF",
            if dark { rgb(0xff7b72) } else { rgb(0xb42318) },
        ),
        "doc" | "docx" | "odt" | "rtf" => (
            FallbackFileIconKind::Document,
            "DOC",
            if dark { rgb(0x78a9ff) } else { rgb(0x2457a7) },
        ),
        "xls" | "xlsx" | "ods" | "csv" => (
            FallbackFileIconKind::Spreadsheet,
            "XLS",
            if dark { rgb(0x66c58a) } else { rgb(0x1f7a45) },
        ),
        "ppt" | "pptx" | "odp" => (
            FallbackFileIconKind::Presentation,
            "PPT",
            if dark { rgb(0xf39a72) } else { rgb(0xb5481d) },
        ),
        _ => (
            FallbackFileIconKind::Generic,
            "",
            if dark { rgb(0xa9b1ba) } else { rgb(0x59636e) },
        ),
    };
    let label = if label.is_empty() && extension.is_empty() {
        "FILE".to_string()
    } else if label.is_empty() {
        extension
            .chars()
            .take(3)
            .collect::<String>()
            .to_ascii_uppercase()
    } else {
        label.to_string()
    };
    FallbackFileIconSpec {
        kind,
        label,
        accent,
    }
}

pub(crate) fn entry_detail(
    entry: &FileEntry,
    calculate_folder_sizes: bool,
    name_folders: bool,
) -> String {
    if entry.is_junction {
        "Junction".to_string()
    } else if entry.is_symlink {
        "Symlink".to_string()
    } else if entry.is_dir && !calculate_folder_sizes {
        if name_folders {
            "Folder".to_string()
        } else {
            String::new()
        }
    } else {
        format_size(entry.size)
    }
}

/// The list's Modified column: "Today, 14:32", "Yesterday", or an ISO date,
/// all in the local time zone so items modified near midnight land on the
/// day the user saw them change.
pub(crate) fn format_modified(modified: SystemTime) -> String {
    format_modified_at(modified, &Local::now())
}

/// [`format_modified`] relative to an explicit "now", whose time zone is also
/// the one the timestamp is shown in.
pub(crate) fn format_modified_at<Tz: chrono::TimeZone>(
    modified: SystemTime,
    now: &DateTime<Tz>,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    if modified < UNIX_EPOCH {
        return "—".to_string();
    }
    let modified = DateTime::<chrono::Utc>::from(modified).with_timezone(&now.timezone());
    let day = modified.date_naive();
    let today = now.date_naive();
    if day == today {
        format!("Today, {}", modified.format("%H:%M"))
    } else if today.pred_opt() == Some(day) {
        // "Yesterday, 09:10" overflows the default column in the mono font.
        "Yesterday".to_string()
    } else {
        modified.format("%Y-%m-%d").to_string()
    }
}

/// Finder shows volumes by name ("Macintosh HD"); Explorer shows the label
/// with the drive letter ("Local Disk (C:)"). Unnamed volumes show their path.
pub(crate) fn volume_label(volume: &explorie_native_services::VolumeLocation) -> String {
    let name = volume.name.trim();
    if name.is_empty() {
        return volume.path.clone();
    }
    if cfg!(windows) {
        format!("{name} ({})", volume.path.trim_end_matches(['\\', '/']))
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone};

    fn at(zone: &FixedOffset, date: (i32, u32, u32), time: (u32, u32)) -> DateTime<FixedOffset> {
        zone.with_ymd_and_hms(date.0, date.1, date.2, time.0, time.1, 0)
            .unwrap()
    }

    #[test]
    fn sizes_use_finder_decimal_units_and_explorer_binary_units() {
        use SizeUnits::{Binary, Decimal};
        assert_eq!(format_size_in(999, Decimal), "999 B");
        assert_eq!(format_size_in(1_000, Decimal), "1.0 KB");
        assert_eq!(format_size_in(1_500, Decimal), "1.5 KB");
        assert_eq!(format_size_in(2_500_000, Decimal), "2.5 MB");
        assert_eq!(format_size_in(999_999, Decimal), "1.0 MB");
        assert_eq!(format_size_in(1_000, Binary), "1000 B");
        assert_eq!(format_size_in(1_536, Binary), "1.5 KB");
        assert_eq!(format_size_in(1_048_576, Binary), "1.0 MB");
        assert_eq!(format_size_in(1_048_575, Binary), "1.0 MB");
        assert_eq!(format_size_in(u64::MAX, Binary), "16777216.0 TB");

        assert_eq!(format_preview_size_in(0, Decimal), "0 Bytes");
        assert_eq!(format_preview_size_in(512, Decimal), "512 Bytes");
        assert_eq!(format_preview_size_in(1_500, Decimal), "1.5 KB");
        assert_eq!(format_preview_size_in(12_000, Decimal), "12 KB");
        assert_eq!(format_preview_size_in(999_600, Decimal), "1.0 MB");
        assert_eq!(format_preview_size_in(1_000_000_000, Decimal), "1.0 GB");
        assert_eq!(format_preview_size_in(1_536, Binary), "1.5 KB");
        assert_eq!(format_preview_size_in(12_288, Binary), "12 KB");
        assert_eq!(format_preview_size_in(1_048_576, Binary), "1.0 MB");

        let (size, preview) = (format_size(1_000_000), format_preview_size(1_000_000));
        if cfg!(target_os = "macos") {
            assert_eq!((size.as_str(), preview.as_str()), ("1.0 MB", "1.0 MB"));
        } else {
            assert_eq!((size.as_str(), preview.as_str()), ("976.6 KB", "977 KB"));
        }
    }

    #[test]
    fn modified_dates_use_the_local_day_near_midnight() {
        // 23:30 UTC on 26 September is already the 27th two hours east of UTC.
        let modified: SystemTime = chrono::Utc
            .with_ymd_and_hms(2026, 9, 26, 23, 30, 0)
            .unwrap()
            .into();
        let east = FixedOffset::east_opt(2 * 3600).unwrap();
        let west = FixedOffset::west_opt(5 * 3600).unwrap();

        assert_eq!(
            format_modified_at(modified, &at(&east, (2026, 10, 5), (9, 0))),
            "2026-09-27"
        );
        assert_eq!(
            format_modified_at(modified, &at(&west, (2026, 10, 5), (9, 0))),
            "2026-09-26"
        );
        assert_eq!(
            format_modified_at(modified, &at(&east, (2026, 9, 27), (10, 0))),
            "Today, 01:30"
        );
        assert_eq!(
            format_modified_at(modified, &at(&west, (2026, 9, 26), (20, 0))),
            "Today, 18:30"
        );
    }

    #[test]
    fn recent_modified_dates_are_relative_and_compact() {
        let zone = FixedOffset::east_opt(0).unwrap();
        let now = at(&zone, (2026, 3, 1), (8, 0));
        let modified = |date, time| SystemTime::from(at(&zone, date, time));

        assert_eq!(
            format_modified_at(modified((2026, 3, 1), (0, 0)), &now),
            "Today, 00:00"
        );
        assert_eq!(
            format_modified_at(modified((2026, 2, 28), (23, 59)), &now),
            "Yesterday"
        );
        assert_eq!(
            format_modified_at(modified((2026, 2, 27), (12, 0)), &now),
            "2026-02-27"
        );
        // Clock skew: a timestamp from the future is shown as a plain date.
        assert_eq!(
            format_modified_at(modified((2026, 3, 2), (9, 0)), &now),
            "2026-03-02"
        );
        assert_eq!(
            format_modified_at(UNIX_EPOCH - Duration::from_secs(1), &now),
            "—"
        );
        for label in ["Today, 00:00", "Yesterday", "2026-02-27"] {
            assert!(label.chars().count() <= 12, "{label} fits the column");
        }
    }

    #[test]
    fn list_columns_yield_width_to_name_before_truncating_it() {
        // 1024 px window with the sidebar and inspector open.
        let tight = list_column_widths(420.0, None, None, &[None]);
        assert_eq!(
            tight,
            ListColumnWidths {
                custom: vec![56.0],
                size: 80.0,
                modified: 118.0,
            }
        );
        // Wide windows keep the preferred widths.
        let wide = list_column_widths(1_200.0, None, None, &[None]);
        assert_eq!(
            wide,
            ListColumnWidths {
                custom: vec![120.0],
                size: 110.0,
                modified: 130.0,
            }
        );
        assert_eq!(
            list_column_widths(1_200.0, None, None, &[]),
            ListColumnWidths {
                custom: Vec::new(),
                size: 120.0,
                modified: 140.0,
            }
        );
        // In between, the columns shrink together and Name keeps its target.
        let middle = list_column_widths(520.0, None, None, &[None]);
        assert!(tight.size < middle.size && middle.size < wide.size);
        let taken = middle.custom[0] + middle.size + middle.modified;
        assert!(520.0 - SELECTION_MARQUEE_GUTTER - taken >= LIST_NAME_TARGET_WIDTH);
        // Widths the user set win, even when they crowd Name.
        let resized = list_column_widths(420.0, Some(200.0), None, &[Some(150.0)]);
        assert_eq!(resized.size, 200.0);
        assert_eq!(resized.custom, vec![150.0]);
        assert_eq!(resized.modified, 118.0);
    }
}
