use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local};
use explorie_core::{FileEntry, FileOperationProgress};
use explorie_native_services::listing::{ListRequest, launch_directory_from_args};
use explorie_native_services::{
    AppInfo, ArchiveFormat, ArchiveInfo, ArchiveProgressEvent, AudioStatus, BatchRenameItem,
    BlockingTask, CombineMode, CompressRequest, CompressionLevel, ConflictPolicy,
    DetectedPreviewKind, DiskInfo, DownloadedUpdate, ErrorCode, ExtractRequest, FileOperationEvent,
    FileOperationKind, FileOperationRequest, FileOperationResult, HELPER_APPROVAL_MESSAGE,
    HelperStatus, ImageMetadata, InstallCleanupOffer, ModelCamera, ModelFrame, ModelPreview,
    NativeServices, PermanentDeleteResult, PreviewDetection, RemoteDriveEnvironment,
    RemoteDriveExitBlocker, RemoteDriveProfile, RemoteDriveState, RemoteDriveStatus, RichBlockKind,
    SearchCriteria, SearchProgressEvent, SearchResult, SearchSource, SearchType, ServiceError,
    ServiceEvent, ServiceResult, SystemIntegrationStatus, SystemLocations, TextHighlightKind,
    TextPreview, UpdateInfo, VideoFrame, VideoStatus, WatcherEvent, WatcherState, format_exif_date,
    is_image_metadata_path, validate_remote_drive_profile,
};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use gpui::WindowControlArea;
use gpui::{
    AccessibleAction, AnyElement, AnyView, App, AssetSource, Bounds, ClipboardItem, Context,
    ElementId, Entity, EventEmitter, ExternalPaths, FocusHandle, Focusable, FontWeight,
    HighlightStyle, KeyDownEvent, KeyUpEvent, ListAlignment, ListState, MouseButton, ObjectFit,
    Orientation, PathPromptOptions, Pixels, Render, RenderImage, Rgba, Role, ScrollHandle,
    ScrollStrategy, SharedString, StyleRefinement, StyledImage, StyledText, Subscription, Task,
    TitlebarOptions, UniformListScrollHandle, Window, WindowAppearance, WindowBounds,
    WindowOptions, deferred, div, img, list, point, prelude::*, px, rgb, svg, uniform_list,
};

mod app_menu;
mod batch_rename;
mod browser;
#[cfg(test)]
mod clipboard_tests;
mod column;
#[cfg(test)]
mod column_interaction_tests;
mod command;
mod custom_fields;
mod diagnostics;
#[cfg(test)]
mod entry_kind_tests;
mod error_reports;
mod file_clipboard;
#[cfg(test)]
mod finder_parity_tests;
mod native_text_input;
mod operation;
mod operation_recovery;
mod plugins_ui;
mod preview_panel;
#[cfg(test)]
mod preview_perf_tests;
#[cfg(test)]
mod render_perf_tests;
pub use plugins_ui::initialize_plugins;
mod entry_marks;
mod entry_visuals;
mod image_memory;
mod interaction;
#[cfg(test)]
mod layout_tests;
mod media_player;
#[cfg(test)]
mod media_player_tests;
mod palette;
mod prompt;
mod recovery;
mod remote_support;
mod rich_blocks;
mod runtime;
mod session;
mod settings;
mod shortcut;
mod single_instance;
mod smart_folder;
mod text_highlight;
mod ui_state;
mod undo_support;
mod widgets;
mod window;
mod window_pane;
#[cfg(test)]
mod window_pane_tests;
mod workspace;

pub use app_menu::install_app_menus;
use batch_rename::{BatchRenameEditor, BatchRenameMode};
use browser::{BrowserState, file_name, is_directory_entry, is_folder_like};
pub use browser::{EntryFilter, SortDirection, SortKey, ViewMode};
use column::{ColumnState, build_path_stack, leaf_alignment};
use command::{
    CommandContext, CommandId, CommandSpec, all_commands_with_shortcuts, filtered_commands,
};
use custom_fields::{CustomFieldInput, CustomFieldsEditor, FIELD_SUGGESTIONS, display_value};
use diagnostics::{DiagnosticsSnapshot, create_diagnostics_json};
use entry_marks::*;
use entry_visuals::*;
use error_reports::ErrorReportLog;
use file_clipboard::{FileClipboard, SystemClipboard};
use image_memory::{ImageMemory, with_image_cache};
use interaction::*;
use media_player::{MediaEvent, MediaPlayer};
use native_text_input::{NativeTextInput, NativeTextInputAppearance, NativeTextInputEvent};
use operation::{
    ClipboardKind, ClipboardState, CreatedKind, OperationQueue, OperationStatus, UndoAction,
    UndoLedger, UndoRecord,
};
use operation_recovery::{InterruptedOperation, OperationRecoveryStore, RecoveryDisposition};
use palette::*;
use preview_panel::{
    FinderTagsState, KEYBOARD_PREVIEW_DEBOUNCE, PhotoMetadataState, PreviewContent,
    PreviewDebounce, PreviewRoute, PreviewState, PreviewTab, finder_tag_color,
};
use prompt::{MutationPrompt, MutationPromptKind};
pub use recovery::RecoveryMarker;
use remote_support::*;
use rich_blocks::rich_block_element;
pub use runtime::WindowRuntime;
use runtime::*;
use session::{
    SessionState, SessionStore, SessionWindowPlacement, SharedSessionState, TabId,
    WindowSessionRegistry,
};
use settings::{
    AccentColor, AppSettings, AppearanceSettings, Density, FontChoice, SettingsStore, ThemeMode,
    ThemeSpec, validate_theme_map, validate_theme_name,
};
pub use shortcut::application_key_bindings;
use shortcut::{
    EDITABLE_SHORTCUTS, binding_for, binding_from_keystroke, display_binding,
    fixed_browser_bindings, validate_shortcut_overrides,
};
pub use single_instance::{
    SingleInstanceGuard, SingleInstancePrimary, SingleInstanceRequest, acquire_single_instance,
};
use smart_folder::{SmartFolderDraft, SmartFolderField};
use text_highlight::*;
use ui_state::*;
use undo_support::*;
use widgets::*;
use window::{
    ClipboardUi, ColumnViewUi, ContextMenuUi, DiskInfoState, EntryVisuals, ListingUi, MutationUi,
    NativeTextInputState, NavigationUi, OperationUi, OverlayUi, PreviewUi, QuickLookUi, RecoveryUi,
    RemoteDrivesUi, SearchScope, SearchUi, SettingsUi, SystemUi, ToastQueue, WatcherUi,
    WindowLayout, WorkspaceUi,
};
use window_pane::{PaneInputs, PaneKind, WindowPanes};
use workspace::{
    WorkspaceSnapshot, WorkspaceState, WorkspaceStore, WorkspaceTab, WorkspaceWindowState,
};

gpui::actions!(
    explorie,
    [
        GoBack,
        GoForward,
        GoUp,
        GoToFolder,
        Refresh,
        ToggleHidden,
        CycleFilter,
        SelectNext,
        SelectPrevious,
        OpenSelected,
        ClearSelection,
        ShowListView,
        ShowGridView,
        ShowColumnView,
        ColumnLeft,
        ColumnRight,
        FocusSearch,
        ToggleFolderSizes,
        NewWindow,
        MoveTabToNewWindow,
        NewTab,
        CloseTab,
        NextTab,
        PreviousTab,
        MoveTabLeft,
        MoveTabRight,
        MoveFavoriteUp,
        MoveFavoriteDown,
        ToggleFavorite,
        SelectAll,
        SelectNextRange,
        SelectPreviousRange,
        SaveSearch,
        CopySelected,
        CutSelected,
        Paste,
        TrashSelected,
        PermanentDeleteSelected,
        CreateArchive,
        ExtractArchive,
        InspectArchive,
        CloseArchiveInspection,
        PreviewSelected,
        ClosePreview,
        RetryPreview,
        ClearPreviewCache,
        RefreshPreviewHelpers,
        CycleArchiveFormat,
        CycleArchiveCompression,
        CancelOperation,
        ClearCompletedOperations,
        CycleConflictPolicy,
        RetryOperation,
        NewFolder,
        NewNote,
        RenameSelected,
        NewWebsiteLink,
        Undo,
        Redo,
        ToggleSettingsPanel,
        CloseSettingsPanel,
        ResetSettings,
        CycleTheme,
        CycleAccent,
        CycleDensity,
        CycleUiScale,
        IncreaseUiScale,
        DecreaseUiScale,
        ResetUiScale,
        CycleListRowHeight,
        CycleGridWidth,
        CycleFont,
        CycleBorderRadius,
        CycleIconSize,
        CycleUndoTimeout,
        ToggleSystemFiles,
        TogglePreviewPanel,
        ToggleStatusBar,
        ToggleConfirmDelete,
        ToggleScriptPreview,
        ToggleErrorReporting,
        ToggleRemoteDrives,
        ToggleReduceMotion,
        ToggleHighContrast,
        OpenCommandPalette,
        ToggleShortcutsOverlay,
        ToggleDiagnostics,
        ToggleWorkspaceManager,
        SaveWorkspace,
        ToggleRemoteDriveManager,
        CloseControlSurface,
        CopyDiagnostics,
        DismissRecovery,
        DismissToast,
    ]
);

pub const APP_IDENTIFIER: &str = "com.omershatz.explorie";
pub const APP_NAME: &str = "explorie";
pub const MACOS_SYSTEM_FONT_FAMILY: &str = ".SystemUIFont";
pub const DEFAULT_WINDOW_WIDTH: f32 = 1024.0;
pub const DEFAULT_WINDOW_HEIGHT: f32 = 768.0;
pub const MIN_WINDOW_WIDTH: f32 = 800.0;
pub const MIN_WINDOW_HEIGHT: f32 = 600.0;
const UI_SCALE_STEPS: &[f32] = &[0.9, 1.0, 1.1, 1.25, 1.4];

#[derive(Clone, Copy, Debug, Default)]
pub struct ExplorieAssets;

impl AssetSource for ExplorieAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "icons/icon.png" => Some(include_bytes!("../../native-assets/icons/icon.png")),
            "icons/titlebar-icon.png" => Some(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/titlebar-icon.png"
            ))),
            "icons/app-face.svg" => Some(include_bytes!("../assets/icons/app-face.svg")),
            "icons/arrow-left.svg" => Some(include_bytes!("../assets/icons/arrow-left.svg")),
            "icons/arrow-right.svg" => Some(include_bytes!("../assets/icons/arrow-right.svg")),
            "icons/arrow-up.svg" => Some(include_bytes!("../assets/icons/arrow-up.svg")),
            "icons/bookmark.svg" => Some(include_bytes!("../assets/icons/bookmark.svg")),
            "icons/check.svg" => Some(include_bytes!("../assets/icons/check.svg")),
            "icons/close.svg" => Some(include_bytes!("../assets/icons/close.svg")),
            "icons/cloud.svg" => Some(include_bytes!("../assets/icons/cloud.svg")),
            "icons/copy.svg" => Some(include_bytes!("../assets/icons/copy.svg")),
            "icons/cut.svg" => Some(include_bytes!("../assets/icons/cut.svg")),
            "icons/desktop.svg" => Some(include_bytes!("../assets/icons/desktop.svg")),
            "icons/download.svg" => Some(include_bytes!("../assets/icons/download.svg")),
            "icons/drive.svg" => Some(include_bytes!("../assets/icons/drive.svg")),
            "icons/edit.svg" => Some(include_bytes!("../assets/icons/edit.svg")),
            "icons/eye-closed.svg" => Some(include_bytes!("../assets/icons/eye-closed.svg")),
            "icons/eye.svg" => Some(include_bytes!("../assets/icons/eye.svg")),
            "icons/extract.svg" => Some(include_bytes!("../assets/icons/extract.svg")),
            "icons/file.svg" => Some(include_bytes!("../assets/icons/file.svg")),
            "icons/folder.svg" => Some(include_bytes!("../assets/icons/folder.svg")),
            "icons/frame.svg" => Some(include_bytes!("../assets/icons/frame.svg")),
            "icons/grid.svg" => Some(include_bytes!("../assets/icons/grid.svg")),
            "icons/home.svg" => Some(include_bytes!("../assets/icons/home.svg")),
            "icons/image.svg" => Some(include_bytes!("../assets/icons/image.svg")),
            "icons/info.svg" => Some(include_bytes!("../assets/icons/info.svg")),
            "icons/link.svg" => Some(include_bytes!("../assets/icons/link.svg")),
            "icons/list.svg" => Some(include_bytes!("../assets/icons/list.svg")),
            "icons/moon.svg" => Some(include_bytes!("../assets/icons/moon.svg")),
            "icons/music.svg" => Some(include_bytes!("../assets/icons/music.svg")),
            "icons/minus.svg" => Some(include_bytes!("../assets/icons/minus.svg")),
            "icons/more-vertical.svg" => Some(include_bytes!("../assets/icons/more-vertical.svg")),
            "icons/archive.svg" => Some(include_bytes!("../assets/icons/archive.svg")),
            "icons/paste.svg" => Some(include_bytes!("../assets/icons/paste.svg")),
            "icons/plus.svg" => Some(include_bytes!("../assets/icons/plus.svg")),
            "icons/redo.svg" => Some(include_bytes!("../assets/icons/redo.svg")),
            "icons/reload.svg" => Some(include_bytes!("../assets/icons/reload.svg")),
            "icons/search.svg" => Some(include_bytes!("../assets/icons/search.svg")),
            "icons/shield.svg" => Some(include_bytes!("../assets/icons/shield.svg")),
            "icons/sliders.svg" => Some(include_bytes!("../assets/icons/sliders.svg")),
            "icons/sort.svg" => Some(include_bytes!("../assets/icons/sort.svg")),
            "icons/sun.svg" => Some(include_bytes!("../assets/icons/sun.svg")),
            "icons/trash.svg" => Some(include_bytes!("../assets/icons/trash.svg")),
            "icons/undo.svg" => Some(include_bytes!("../assets/icons/undo.svg")),
            "icons/view-col.svg" => Some(include_bytes!("../assets/icons/view-col.svg")),
            "icons/video.svg" => Some(include_bytes!("../assets/icons/video.svg")),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        if path != "icons" {
            return Ok(Vec::new());
        }
        Ok([
            "icon.png",
            "titlebar-icon.png",
            "app-face.svg",
            "arrow-left.svg",
            "arrow-right.svg",
            "arrow-up.svg",
            "bookmark.svg",
            "check.svg",
            "close.svg",
            "cloud.svg",
            "copy.svg",
            "cut.svg",
            "desktop.svg",
            "download.svg",
            "drive.svg",
            "edit.svg",
            "eye-closed.svg",
            "eye.svg",
            "extract.svg",
            "file.svg",
            "folder.svg",
            "frame.svg",
            "grid.svg",
            "home.svg",
            "image.svg",
            "info.svg",
            "link.svg",
            "list.svg",
            "moon.svg",
            "music.svg",
            "minus.svg",
            "more-vertical.svg",
            "archive.svg",
            "paste.svg",
            "plus.svg",
            "redo.svg",
            "reload.svg",
            "search.svg",
            "shield.svg",
            "sliders.svg",
            "sort.svg",
            "sun.svg",
            "trash.svg",
            "undo.svg",
            "view-col.svg",
            "video.svg",
        ]
        .into_iter()
        .map(SharedString::from)
        .collect())
    }
}

const ENTRY_ICON_CACHE_LIMIT: usize = 256;
const ENTRY_THUMBNAIL_CACHE_LIMIT: usize = 128;
const ENTRY_THUMBNAIL_MAX_CONCURRENT: usize = 4;
const SELECTION_MARQUEE_THRESHOLD: f32 = 5.0;
const SELECTION_MARQUEE_EDGE_ZONE: f32 = 40.0;
const SELECTION_MARQUEE_SCROLL_STEP: f32 = 10.0;
const SELECTION_MARQUEE_GUTTER: f32 = 16.0;
const DEFAULT_COLUMN_VIEW_WIDTH: f32 = 280.0;
const MIN_COLUMN_VIEW_WIDTH: f32 = 180.0;
const MAX_COLUMN_VIEW_WIDTH: f32 = 640.0;
const GRID_HORIZONTAL_PADDING: f32 = 8.0;
const FILE_DRAG_EDGE_ZONE: f32 = 36.0;
const FILE_DRAG_SCROLL_STEP: f32 = 16.0;
const HIDDEN_ENTRY_OPACITY: f32 = 0.62;
const MIN_PREVIEW_PANEL_WIDTH: f32 = 280.0;
const MAX_PREVIEW_PANEL_WIDTH: f32 = 640.0;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum EntryIconKey {
    Folder,
    FileKind(String),
    Source {
        path: PathBuf,
        size: u64,
        modified: SystemTime,
        is_dir: bool,
    },
}

impl EntryIconKey {
    /// Entries whose system icon is specific to the item get their own key;
    /// the rest share one icon per folder or file extension.
    fn for_entry(entry: &FileEntry) -> Self {
        let extension = entry
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        // macOS: applications and other packages, aliases and special
        // folders, decided by the same rule the icon service follows.
        #[cfg(target_os = "macos")]
        let item_specific = explorie_native_services::preview::has_item_specific_icon(entry);
        // Windows: executables, shortcuts and icon files embed their own.
        #[cfg(not(target_os = "macos"))]
        let item_specific = matches!(extension.as_str(), "exe" | "lnk" | "ico");
        if entry.is_symlink || entry.is_junction || item_specific {
            Self::Source {
                path: entry.path.clone(),
                size: entry.size,
                modified: entry.modified,
                is_dir: entry.is_dir,
            }
        } else if entry.is_dir {
            Self::Folder
        } else {
            Self::FileKind(extension)
        }
    }
}

#[derive(Clone, Debug)]
enum EntryIconState {
    Loading,
    Ready(Option<PathBuf>),
}

#[derive(Clone, Debug)]
struct EntryIconRequest {
    key: EntryIconKey,
    path: PathBuf,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct EntryThumbnailKey {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
    max_size: u32,
}

impl EntryThumbnailKey {
    fn for_entry(entry: &FileEntry, max_size: u32) -> Self {
        Self {
            path: entry.path.clone(),
            size: entry.size,
            modified: entry.modified,
            max_size,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum EntryThumbnailState {
    Loading,
    Ready(PathBuf),
    Failed,
}

#[derive(Clone, Debug)]
struct EntryThumbnailRequest {
    key: EntryThumbnailKey,
    path: PathBuf,
    max_size: u32,
}

/// Whether Grid view asks the preview service for a thumbnail of `entry`
/// instead of showing its icon. Cloud placeholders never do: rendering one
/// would download it.
fn entry_supports_grid_thumbnail(entry: &FileEntry) -> bool {
    if entry.is_dir || entry.is_symlink || entry.is_junction || entry.is_cloud_placeholder {
        return false;
    }
    let extension = entry
        .path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Quick Look thumbnails these on macOS without helper applications. Keynote
    // (`key`) is left out because PEM private keys share its extension and
    // would show a failed thumbnail.
    if cfg!(target_os = "macos")
        && matches!(
            extension.as_str(),
            "heic"
                | "heif"
                | "psd"
                | "pdf"
                | "doc"
                | "docx"
                | "xls"
                | "xlsx"
                | "ppt"
                | "pptx"
                | "rtf"
                | "odt"
                | "pages"
                | "numbers"
                | "ttf"
                | "otf"
                | "ttc"
                | "usdz"
                | "usd"
                | "usda"
                | "usdc"
                | "reality"
                | "icns"
                | "exr"
                | "jp2"
        )
    {
        return true;
    }
    matches!(
        extension.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "bmp"
            | "webp"
            | "svg"
            | "svgz"
            | "avif"
            | "jxl"
            | "jpegxl"
            | "tif"
            | "tiff"
            | "ico"
            | "tga"
            | "dds"
            | "hdr"
            | "pnm"
            | "pbm"
            | "pgm"
            | "ppm"
            | "pam"
            | "qoi"
            | "dng"
            | "cr2"
            | "cr3"
            | "nef"
            | "arw"
            | "orf"
            | "rw2"
            | "raf"
            | "mp4"
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
            | "ogv"
            | "ts"
            | "vob"
            | "glb"
            | "gltf"
            | "obj"
            | "stl"
            | "ply"
            | "3mf"
            | "fbx"
    )
}

fn join_warnings(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(first), Some(second)) => Some(format!("{first} • {second}")),
        (Some(warning), None) | (None, Some(warning)) => Some(warning),
        (None, None) => None,
    }
}

fn next_f32(current: f32, values: &[f32]) -> f32 {
    let index = values
        .iter()
        .position(|value| (current - value).abs() < f32::EPSILON)
        .unwrap_or(0);
    values[(index + 1) % values.len()]
}

fn stepped_f32(current: f32, values: &[f32], direction: isize) -> f32 {
    if direction > 0 {
        values
            .iter()
            .copied()
            .find(|value| *value > current + f32::EPSILON)
            .unwrap_or_else(|| values.last().copied().unwrap_or(current))
    } else if direction < 0 {
        values
            .iter()
            .rev()
            .copied()
            .find(|value| *value < current - f32::EPSILON)
            .unwrap_or_else(|| values.first().copied().unwrap_or(current))
    } else {
        current
    }
}

fn next_u8(current: u8, values: &[u8]) -> u8 {
    let index = values
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    values[(index + 1) % values.len()]
}

fn next_u16(current: u16, values: &[u16]) -> u16 {
    let index = values
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    values[(index + 1) % values.len()]
}

fn next_u32(current: u32, values: &[u32]) -> u32 {
    let index = values
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    values[(index + 1) % values.len()]
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn format_audio_time(milliseconds: u64) -> String {
    let total_seconds = milliseconds / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = total_seconds % 3_600 / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn slider_fraction(bounds: Bounds<Pixels>, position_x: Pixels) -> f32 {
    let width = f32::from(bounds.size.width);
    if width <= 0.0 {
        return 0.0;
    }
    ((f32::from(position_x) - f32::from(bounds.left())) / width).clamp(0.0, 1.0)
}

fn stepped_slider_fraction(index: usize, step_count: usize) -> f32 {
    if step_count <= 1 {
        return 0.0;
    }
    index.min(step_count - 1) as f32 / (step_count - 1) as f32
}

fn media_slider_track(value: f32, palette: UiPalette) -> gpui::Div {
    let value = value.clamp(0.0, 1.0);
    let thumb_size = 10.0 * palette.scale;
    div()
        .relative()
        .w_full()
        .h(px(thumb_size))
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px((thumb_size - 4.0 * palette.scale) / 2.0))
                .h(px(4.0 * palette.scale))
                .rounded_full()
                .overflow_hidden()
                .bg(palette.border)
                .child(div().h_full().w(gpui::relative(value)).bg(palette.accent)),
        )
        .child(
            div()
                .absolute()
                .left(gpui::relative(value))
                .top_0()
                .ml(px(-thumb_size / 2.0))
                .size(px(thumb_size))
                .rounded_full()
                .border_1()
                .border_color(palette.surface)
                .bg(palette.accent)
                .shadow_sm(),
        )
}

fn monospace_font_family() -> &'static str {
    if cfg!(windows) {
        "Consolas"
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "monospace"
    }
}

fn unknown_preview_detection() -> PreviewDetection {
    PreviewDetection {
        kind: DetectedPreviewKind::Unknown,
        description: "Unknown file".to_string(),
        mime_type: Some("application/octet-stream".to_string()),
        byte_sample: None,
    }
}

fn render_video_frame(frame: &VideoFrame) -> Option<Arc<RenderImage>> {
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra.to_vec())?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}

fn render_model_frame(frame: &ModelFrame) -> Option<Arc<RenderImage>> {
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.to_vec())?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}

fn font_family(settings: &AppSettings) -> String {
    if settings.appearance.font == FontChoice::Custom
        && !settings.appearance.font_custom.trim().is_empty()
    {
        return settings.appearance.font_custom.trim().to_string();
    }
    match settings.appearance.font {
        FontChoice::Mono | FontChoice::Custom => {
            if cfg!(windows) {
                "Consolas"
            } else {
                "Menlo"
            }
        }
        FontChoice::System => {
            if cfg!(target_os = "macos") {
                MACOS_SYSTEM_FONT_FAMILY
            } else if cfg!(windows) {
                "Segoe UI"
            } else {
                "sans-serif"
            }
        }
        FontChoice::Serif => "Georgia",
    }
    .to_string()
}

pub fn desktop_window_options(bounds: Bounds<Pixels>) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(gpui::size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
        titlebar: Some(TitlebarOptions {
            title: Some(APP_NAME.into()),
            appears_transparent: cfg!(any(windows, target_os = "macos")),
            traffic_light_position: if cfg!(target_os = "macos") {
                Some(point(px(9.0), px(9.0)))
            } else {
                None
            },
        }),
        is_resizable: true,
        app_id: Some(APP_IDENTIFIER.to_string()),
        ..Default::default()
    }
}

pub fn parse_startup_path(args: impl IntoIterator<Item = OsString>) -> Option<PathBuf> {
    let mut filtered = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--load-plugin" {
            args.next();
        } else {
            filtered.push(arg);
        }
    }
    launch_directory_from_args(filtered)
}

fn apply_smart_folder_browser_state(browser: &mut SessionState, criteria: &SearchCriteria) {
    browser.set_view_mode(ViewMode::List);
    browser.set_search_query(
        criteria
            .name_pattern
            .clone()
            .filter(|_| !criteria.name_regex)
            .unwrap_or_default(),
    );
    browser.set_filter(match criteria.type_filter {
        SearchType::All => EntryFilter::All,
        SearchType::Files => EntryFilter::Files,
        SearchType::Folders => EntryFilter::Folders,
    });
}

#[derive(Debug)]
pub enum DirectoryEvent {
    Listed {
        generation: u64,
        request: ListRequest,
        entries: Vec<FileEntry>,
        /// Non-fatal problems met while listing, joined for the status line.
        warning: Option<String>,
    },
    Failed {
        generation: u64,
        request: ListRequest,
        error: ServiceError,
    },
}

pub async fn list_directory_task(
    services: NativeServices,
    generation: u64,
    request: ListRequest,
) -> DirectoryEvent {
    // A superseded listing (its task was dropped) is skipped if it has not
    // started yet.
    match services
        .listing
        .list_with_warnings(request.clone())
        .cancel_on_drop()
        .await
    {
        Ok(response) => DirectoryEvent::Listed {
            generation,
            request,
            warning: response.warning(),
            entries: response.entries,
        },
        Err(error) => DirectoryEvent::Failed {
            generation,
            request,
            error,
        },
    }
}

#[derive(Debug)]
pub enum SearchEvent {
    Completed {
        generation: u64,
        result: SearchResult,
    },
    Failed {
        generation: u64,
        error: ServiceError,
    },
}

async fn permanently_delete_items(
    services: NativeServices,
    items: Vec<(PathBuf, bool)>,
) -> ServiceResult<PermanentDeleteResult> {
    services.mutations.delete_permanently_batch(items).await
}

fn archive_extension(format: ArchiveFormat) -> &'static str {
    match format {
        ArchiveFormat::Zip => ".zip",
        ArchiveFormat::TarGz => ".tar.gz",
        ArchiveFormat::Tar => ".tar",
        ArchiveFormat::SevenZ => ".7z",
        ArchiveFormat::Rar => ".rar",
    }
}

fn archive_format_label(format: ArchiveFormat) -> &'static str {
    match format {
        ArchiveFormat::Zip => "ZIP",
        ArchiveFormat::TarGz => "TAR.GZ",
        ArchiveFormat::Tar => "TAR",
        ArchiveFormat::SevenZ => "7Z",
        ArchiveFormat::Rar => "RAR",
    }
}

fn compression_label(level: CompressionLevel) -> &'static str {
    match level {
        CompressionLevel::None => "None",
        CompressionLevel::Fast => "Fast",
        CompressionLevel::Normal => "Normal",
        CompressionLevel::Best => "Best",
    }
}

fn archive_base_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "extracted".to_string());
    let lowercase = name.to_ascii_lowercase();
    for suffix in [".tar.gz", ".tgz", ".zip", ".tar", ".7z", ".rar"] {
        if lowercase.ends_with(suffix) {
            return name[..name.len() - suffix.len()].to_string();
        }
    }
    if explorie_core::archive::is_archive(path)
        && let Some(stem) = path.file_stem()
    {
        return stem.to_string_lossy().into_owned();
    }
    format!("{name}-extracted")
}

fn valid_leaf_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.ends_with([' ', '.'])
        && !name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
        })
}

#[derive(Clone, Debug)]
struct TextPreviewUiState {
    wrap_override: Option<bool>,
    find: String,
    find_match: usize,
    find_matches: std::cell::RefCell<Option<TextFindMatches>>,
    unwrapped_scroll: UniformListScrollHandle,
    wrapped_list: ListState,
}

impl Default for TextPreviewUiState {
    fn default() -> Self {
        Self {
            wrap_override: None,
            find: String::new(),
            find_match: 0,
            find_matches: std::cell::RefCell::new(None),
            unwrapped_scroll: UniformListScrollHandle::new(),
            wrapped_list: ListState::new(0, ListAlignment::Top, px(400.0)),
        }
    }
}

impl TextPreviewUiState {
    fn reset(&mut self) {
        self.wrap_override = None;
        self.find.clear();
        self.find_match = 0;
        self.find_matches.replace(None);
        self.unwrapped_scroll
            .0
            .borrow()
            .base_handle
            .set_offset(gpui::point(px(0.0), px(0.0)));
        self.wrapped_list.reset(0);
    }
}

pub struct DirectoryWindow {
    plugin_ui: plugins_ui::PluginUiState,
    browser: SessionState,
    services: NativeServices,
    window_lifetime: Option<WindowLifetime>,
    /// Listing load state, scrolling, warnings and type-to-select.
    listing: ListingUi,
    /// The search field and the smart-folder search in flight.
    search: SearchUi,
    /// System locations, shell integration, install cleanup and updates.
    system: SystemUi,
    /// Space on the current folder's volume.
    disk: DiskInfoState,
    /// Filesystem watcher status and the refreshes it requested.
    watcher: WatcherUi,
    /// Column view columns, their scrolling and their selection.
    column_view: ColumnViewUi,
    service_event_task: Option<Task<()>>,
    shared_state_task: Option<Task<()>>,
    shared_state_revision: u64,
    operations_revision: u64,
    single_instance_tasks: Vec<Task<()>>,
    focus_handle: FocusHandle,
    status_message: Option<String>,
    calculate_folder_sizes: bool,
    session_store: Option<SessionStore>,
    settings: AppSettings,
    settings_store: Option<Arc<SettingsStore>>,
    workspaces: WorkspaceState,
    workspace_store: Option<Arc<WorkspaceStore>>,
    /// Workspace manager editing state and loads in flight.
    workspace_ui: WorkspaceUi,
    smart_folder_editor: Option<SmartFolderDraft>,
    /// Panel sizes, listing geometry and window bounds.
    layout: WindowLayout,
    /// Resizes, the selection marquee and file drags in progress.
    pointer: PointerInteractions,
    favorite_focus_handles: Vec<FocusHandle>,
    /// Remote drives: helper status, manager UI and connection work.
    remote: RemoteDrivesUi,
    batch_rename: Option<BatchRenameEditor>,
    /// Mutation prompt and the mutation jobs in flight.
    mutation: MutationUi,
    /// Settings panel, its editors and its confirmation dialog.
    settings_ui: SettingsUi,
    /// Open control surface or toolbar menu and its focus handling.
    overlay: OverlayUi,
    #[cfg(target_os = "macos")]
    title_bar_drag_pending: bool,
    /// The native text field and the editor it is attached to.
    text_input: NativeTextInputState,
    /// Interrupted-operation recovery and its notice.
    recovery: RecoveryUi,
    /// The toast on screen and the ones queued behind it.
    toasts: ToastQueue,
    error_reports: ErrorReportLog,
    /// The file context menu and the task completing it.
    context_menu: ContextMenuUi,
    palette: UiPalette,
    operations: OperationQueue,
    /// Operation panel, conflict prompts and undo/archive progress.
    operation_ui: OperationUi,
    /// Copy/cut state and the system file clipboard.
    clipboard: ClipboardUi,
    /// Go to Folder, the breadcrumb editor and the folder picker.
    navigation_ui: NavigationUi,
    /// Preview panel and inspector state and the jobs that load it.
    preview: PreviewUi,
    /// Quick Look overlay state.
    quick_look: QuickLookUi,
    image_memory: ImageMemory,
    /// Audio, video and 3D-model playback for the previewed item.
    media: Entity<MediaPlayer>,
    /// Entry icon and thumbnail caches and their load queues.
    visuals: EntryVisuals,
    /// The listing and sidebar, rendered as cached child views.
    panes: WindowPanes,
    undo_ledger: UndoLedger,
    #[cfg(test)]
    last_rendered_items: usize,
    #[cfg(test)]
    render_stats: RenderStats,
}

fn watcher_disposition(
    current_generation: u64,
    current_paths: &[PathBuf],
    event_generation: u64,
    watched_paths: &[PathBuf],
    event: &WatcherEvent,
) -> WatcherDisposition {
    if event_generation != current_generation || watched_paths != current_paths {
        return WatcherDisposition::Ignore;
    }

    match event.state {
        WatcherState::Changed => WatcherDisposition::Refresh,
        WatcherState::Failed => WatcherDisposition::Stop(event.error.as_ref().map_or_else(
            || "Filesystem watcher failed".to_string(),
            ToString::to_string,
        )),
        WatcherState::Stopped => WatcherDisposition::Stop("Filesystem watcher stopped".to_string()),
    }
}

fn constrain_workspace_bounds(
    saved: WorkspaceWindowState,
    current: gpui::Bounds<gpui::Pixels>,
    displays: &[gpui::Bounds<gpui::Pixels>],
) -> gpui::Bounds<gpui::Pixels> {
    let current_x = f32::from(current.origin.x);
    let current_y = f32::from(current.origin.y);
    let mut x = saved.x.unwrap_or(current_x);
    let mut y = saved.y.unwrap_or(current_y);
    let mut width = saved.width.unwrap_or(f32::from(current.size.width));
    let mut height = saved.height.unwrap_or(f32::from(current.size.height));
    let desired_center = (x + width / 2.0, y + height / 2.0);

    let target = displays
        .iter()
        .filter(|display| display.size.width > px(0.0) && display.size.height > px(0.0))
        .max_by(|left, right| {
            let score = |display: &&gpui::Bounds<gpui::Pixels>| {
                let left = f32::from(display.origin.x);
                let top = f32::from(display.origin.y);
                let right = left + f32::from(display.size.width);
                let bottom = top + f32::from(display.size.height);
                let intersection_width = (x + width).min(right) - x.max(left);
                let intersection_height = (y + height).min(bottom) - y.max(top);
                let area = intersection_width.max(0.0) * intersection_height.max(0.0);
                if area > 0.0 {
                    (1, area)
                } else {
                    let center_x = left + (right - left) / 2.0;
                    let center_y = top + (bottom - top) / 2.0;
                    let distance = (desired_center.0 - center_x).powi(2)
                        + (desired_center.1 - center_y).powi(2);
                    (0, -distance)
                }
            };
            score(left)
                .partial_cmp(&score(right))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

    if let Some(display) = target {
        let display_x = f32::from(display.origin.x);
        let display_y = f32::from(display.origin.y);
        let display_width = f32::from(display.size.width);
        let display_height = f32::from(display.size.height);
        width = width.clamp(MIN_WINDOW_WIDTH.min(display_width), display_width);
        height = height.clamp(MIN_WINDOW_HEIGHT.min(display_height), display_height);
        x = x.clamp(display_x, display_x + display_width - width);
        y = y.clamp(display_y, display_y + display_height - height);
    }

    gpui::Bounds::new(gpui::point(px(x), px(y)), gpui::size(px(width), px(height)))
}

pub fn initial_window_bounds(
    config_dir: &Path,
    fallback: gpui::Bounds<gpui::Pixels>,
    cx: &mut App,
) -> gpui::Bounds<gpui::Pixels> {
    let Some(placement) = settings::load_window_placement(config_dir) else {
        return fallback;
    };
    let displays: Vec<_> = cx
        .displays()
        .into_iter()
        .map(|display| display.bounds())
        .collect();
    constrain_workspace_bounds(
        WorkspaceWindowState {
            width: placement.width,
            height: placement.height,
            x: placement.x,
            y: placement.y,
        },
        fallback,
        &displays,
    )
}

#[cfg(all(windows, not(test)))]
fn move_native_window(
    window: &gpui::Window,
    origin: gpui::Point<gpui::Pixels>,
) -> Result<(), String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
    };

    let handle = HasWindowHandle::window_handle(window).map_err(|error| error.to_string())?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Ok(());
    };
    let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    if hwnd.is_null() {
        return Ok(());
    }
    let moved = unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            f32::from(origin.x).round() as i32,
            f32::from(origin.y).round() as i32,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

#[cfg(all(target_os = "macos", not(test)))]
#[allow(deprecated, unexpected_cfgs)]
fn move_native_window(
    window: &gpui::Window,
    origin: gpui::Point<gpui::Pixels>,
) -> Result<(), String> {
    use cocoa::appkit::{NSScreen, NSWindow};
    use cocoa::base::{id, nil};
    use cocoa::foundation::NSPoint;
    use objc::{msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = HasWindowHandle::window_handle(window).map_err(|error| error.to_string())?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Ok(());
    };
    let view = handle.ns_view.as_ptr() as id;
    let native_window: id = unsafe { msg_send![view, window] };
    if native_window == nil {
        return Err("AppKit window handle is unavailable".to_string());
    }
    let screen = unsafe { NSWindow::screen(native_window) };
    if screen == nil {
        return Err("AppKit screen is unavailable".to_string());
    }
    let screen_frame = unsafe { NSScreen::frame(screen) };
    let top_left = NSPoint::new(
        screen_frame.origin.x + f64::from(f32::from(origin.x)),
        screen_frame.origin.y + screen_frame.size.height - f64::from(f32::from(origin.y)),
    );
    unsafe { NSWindow::setFrameTopLeftPoint_(native_window, top_left) };
    Ok(())
}

#[cfg(any(not(any(windows, target_os = "macos")), test))]
fn move_native_window(
    _window: &gpui::Window,
    _origin: gpui::Point<gpui::Pixels>,
) -> Result<(), String> {
    Ok(())
}

impl Drop for DirectoryWindow {
    fn drop(&mut self) {
        self.sync_active_tab_view_state();
        self.browser.set_window_placement(SessionWindowPlacement {
            width: self.layout.last_window_bounds.width,
            height: self.layout.last_window_bounds.height,
            x: self.layout.last_window_bounds.x,
            y: self.layout.last_window_bounds.y,
        });
        if let Some(store) = &self.session_store {
            let _ = store.save(&self.browser);
        }
    }
}

impl Focusable for DirectoryWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

async fn expand_go_to_folder_path(
    services: &NativeServices,
    input: &str,
) -> Result<PathBuf, ServiceError> {
    let input = input.trim();
    if input == "~" || input.starts_with("~/") || input.starts_with("~\\") {
        let home = services.integration.home_dir().await?;
        let remainder = input[1..].trim_start_matches(['/', '\\']);
        return Ok(if remainder.is_empty() {
            home
        } else {
            home.join(remainder)
        });
    }
    Ok(PathBuf::from(input))
}

fn go_to_folder_parent_prefix(expanded: &Path, current_path: &Path) -> (PathBuf, String) {
    let value = expanded.to_string_lossy();
    let separator = value.rfind(['/', '\\']);
    if let Some(index) = separator {
        let parent = &value[..=index];
        let prefix = value[index + 1..].to_string();
        return (PathBuf::from(parent), prefix);
    }
    #[cfg(windows)]
    if value.len() <= 2
        && value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic())
        && (value.len() == 1 || value.ends_with(':'))
    {
        return (
            PathBuf::from(format!("{}:\\", value.chars().next().unwrap())),
            String::new(),
        );
    }
    (current_path.to_path_buf(), value.into_owned())
}

fn go_to_folder_error(error: &ServiceError) -> String {
    match &error.code {
        ErrorCode::NotFound => "Path does not exist".to_string(),
        ErrorCode::PermissionDenied => "Access denied".to_string(),
        ErrorCode::InvalidInput => "Path is not a directory".to_string(),
        _ if error.message.to_lowercase().contains("not a directory") => {
            "Path is not a directory".to_string()
        }
        _ => "Invalid path".to_string(),
    }
}

fn path_label(path: &std::path::Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn most_specific_location_index(
    current_path: &Path,
    locations: &[(String, PathBuf, &'static str)],
) -> Option<usize> {
    locations
        .iter()
        .enumerate()
        .filter(|(_, (_, path, _))| current_path.starts_with(path))
        .max_by_key(|(_, (_, path, _))| path.components().count())
        .map(|(index, _)| index)
}

fn recovery_session_context(tab_count: usize, path: &Path) -> String {
    format!(
        "{tab_count} tab{} • Last: {} • Restored from the last atomic snapshot",
        if tab_count == 1 { "" } else { "s" },
        path_label(path)
    )
}

#[cfg(test)]
mod tests;
