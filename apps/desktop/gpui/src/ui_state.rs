//! Small state types for window overlays, editors, and panels.

use crate::*;

/// Pointer gestures in progress: panel and column resizes, the selection
/// marquee, and file drags with their hover target.
#[derive(Default)]
pub(crate) struct PointerInteractions {
    pub(crate) sidebar_resize: Option<SidebarResize>,
    pub(crate) preview_panel_resize: Option<PreviewPanelResize>,
    pub(crate) list_column_resize: Option<ListColumnResize>,
    pub(crate) column_view_resize: Option<ColumnViewResize>,
    pub(crate) selection_marquee: Option<SelectionMarquee>,
    pub(crate) file_drag_sources: BTreeSet<PathBuf>,
    pub(crate) file_drag_selection: Option<Arc<BTreeSet<PathBuf>>>,
    pub(crate) file_drag_hover_target: Option<FileDragHoverTarget>,
    pub(crate) file_drag_hover_task: Option<Task<()>>,
}

#[derive(Debug)]
pub(crate) enum ListingState {
    Loading,
    Ready,
    Failed(String),
}

#[derive(Debug)]
pub(crate) enum WatchStatus {
    Starting,
    Watching,
    Unavailable(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControlSurface {
    Closed,
    CommandPalette,
    Shortcuts,
    Diagnostics,
    SmartFolders,
    Workspaces,
    RemoteDrives,
    BatchRename,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolbarMenu {
    Closed,
    BackHistory,
    ForwardHistory,
    Create,
    View,
    Sort,
    Filter,
    More,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SettingsTab {
    #[default]
    General,
    Integration,
    Plugins,
    Appearance,
    Themes,
    Shortcuts,
    About,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingsSelector {
    Accent,
    Font,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SettingsConfirmation {
    ResetSettings,
    ResetShortcut(String),
    ResetAllShortcuts,
    ClearPreviewCache,
    DeleteTheme(String),
    RemoveCustomField(String),
    InstallUpdate(UpdateInfo),
    CleanupInstallMedia(InstallCleanupOffer),
    DiscardDraft(PendingDraftAction),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UpdateStatus {
    Idle,
    Checking,
    UpToDate,
    Available(UpdateInfo),
    Downloading(UpdateInfo),
    Ready(DownloadedUpdate),
    Installing,
    Failed(String),
}

#[derive(Clone)]
pub(crate) enum UpdateAction {
    Check,
    Download(UpdateInfo),
    Install(DownloadedUpdate),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingDraftAction {
    CloseControlSurface,
    OpenControlSurface(ControlSurface),
    ToggleToolbarMenu(ToolbarMenu),
    OpenSettings,
    CloseSettings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingsSlider {
    UiScale,
    ListRowHeight,
    GridWidth,
    IconSize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextInputTarget {
    PluginSetting,
    Search,
    ControlQuery,
    GoToFolder,
    Breadcrumb,
    MutationPrompt,
    AppearanceValue,
    NamedTheme,
    PreviewFind,
    CustomField,
    FinderTag,
}

#[derive(Clone, Debug)]
pub(crate) enum ColumnSelectionTarget {
    First,
    Path(PathBuf),
}

impl SettingsTab {
    pub(crate) const ALL: [Self; 7] = [
        Self::General,
        Self::Integration,
        Self::Plugins,
        Self::Appearance,
        Self::Themes,
        Self::Shortcuts,
        Self::About,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Integration => "System Integration",
            Self::Plugins => "Integrations",
            Self::Appearance => "Appearance",
            Self::Themes => "Themes",
            Self::Shortcuts => "Shortcuts",
            Self::About => "About",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToastKind {
    Success,
    Warning,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FileConflictChoice {
    Skip,
    Replace,
    KeepBoth,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppearanceValueKind {
    Accent,
    Font,
}

#[derive(Clone, Debug)]
pub(crate) struct AppearanceValueEditor {
    pub(crate) kind: AppearanceValueKind,
    pub(crate) input: String,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NamedThemeEditor {
    pub(crate) input: String,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ShortcutEditor {
    pub(crate) command_id: String,
    pub(crate) binding: String,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FolderSuggestion {
    pub(crate) path: PathBuf,
    pub(crate) recent: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct GoToFolderState {
    pub(crate) input: String,
    pub(crate) replace_on_type: bool,
    pub(crate) error: Option<String>,
    pub(crate) suggestions: Vec<FolderSuggestion>,
    pub(crate) selected: Option<usize>,
    pub(crate) validating: bool,
    pub(crate) generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct BreadcrumbEditor {
    pub(crate) input: String,
    pub(crate) replace_on_type: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SidebarResize {
    pub(crate) start_x: f32,
    pub(crate) start_width: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PreviewPanelResize {
    pub(crate) start_x: f32,
    pub(crate) start_width: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct ListColumnResize {
    pub(crate) key: String,
    pub(crate) start_x: f32,
    pub(crate) start_width: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct ColumnViewResize {
    pub(crate) path: PathBuf,
    pub(crate) start_x: f32,
    pub(crate) start_width: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum MediaSliderKind {
    AudioSeek,
    AudioVolume,
    VideoSeek,
    VideoVolume,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MediaSliderDrag {
    pub(crate) kind: MediaSliderKind,
    pub(crate) bounds: Bounds<Pixels>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ModelDrag {
    pub(crate) start_x: f32,
    pub(crate) start_y: f32,
    pub(crate) camera: ModelCamera,
    pub(crate) pan: bool,
}

impl AppearanceValueEditor {
    pub(crate) fn title(&self) -> &'static str {
        match self.kind {
            AppearanceValueKind::Accent => "Custom accent color",
            AppearanceValueKind::Font => "Custom font family",
        }
    }

    pub(crate) fn hint(&self) -> &'static str {
        match self.kind {
            AppearanceValueKind::Accent => "Use #RRGGBB",
            AppearanceValueKind::Font => "Use an installed font family name",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FileConflictPrompt {
    pub(crate) job_id: String,
    pub(crate) request: FileOperationRequest,
    pub(crate) apply_to_all: bool,
}

impl FileConflictPrompt {
    pub(crate) fn current_source(&self) -> Option<&Path> {
        self.request.sources.first().map(PathBuf::as_path)
    }

    pub(crate) fn destination_path(&self) -> Option<PathBuf> {
        Some(
            self.request
                .destination
                .as_ref()?
                .join(self.current_source()?.file_name()?),
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) struct UndoProgressState {
    pub(crate) description: String,
    pub(crate) completed_items: usize,
    pub(crate) total_items: usize,
    pub(crate) processed_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) current_job_id: Option<String>,
    pub(crate) cancellation: Arc<AtomicBool>,
    pub(crate) cancelling: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum UndoProgressUpdate {
    Started(String),
    Progress(FileOperationProgress),
    ItemCompleted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ToastNotice {
    pub(crate) message: String,
    pub(crate) kind: ToastKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ContextMenuAction {
    Open,
    Preview,
    Rename,
    BatchRename,
    Copy,
    Cut,
    Trash,
    Reveal,
    OpenWithChooser,
    ToggleOpenWith,
    OpenWithApp(AppInfo),
    /// Open With ▸ Other…: pick any application.
    OpenWithOther,
    ToggleFavorite,
    Compress,
    InspectArchive,
    ExtractArchive,
    ShowPackageContents,
    Paste,
}

impl ContextMenuAction {
    pub(crate) fn label(&self, count: usize, favorite: bool, expanded: bool) -> String {
        match self {
            Self::Open => "Open".to_string(),
            Self::Preview => "Quick Look".to_string(),
            Self::Rename => "Rename".to_string(),
            Self::BatchRename => format!("Batch Rename ({count})"),
            Self::Copy if count > 1 => format!("Copy ({count})"),
            Self::Copy => "Copy".to_string(),
            Self::Cut if count > 1 => format!("Cut ({count})"),
            Self::Cut => "Cut".to_string(),
            Self::Trash if count > 1 => format!("Delete ({count})"),
            Self::Trash => "Delete".to_string(),
            Self::Reveal if cfg!(target_os = "macos") => "Show in Finder".to_string(),
            Self::Reveal if cfg!(windows) => "Show in Explorer".to_string(),
            Self::Reveal => "Show in File Manager".to_string(),
            Self::OpenWithChooser => "Open with…".to_string(),
            Self::ToggleOpenWith if expanded => "Open With  ▾".to_string(),
            Self::ToggleOpenWith => "Open With  ›".to_string(),
            Self::OpenWithApp(app) if app.is_default => format!("    {} (default)", app.name),
            Self::OpenWithApp(app) => format!("    {}", app.name),
            Self::OpenWithOther => "    Other…".to_string(),
            Self::ToggleFavorite if favorite => "Remove from Favorites".to_string(),
            Self::ToggleFavorite => "Add to Favorites".to_string(),
            Self::Compress if count > 1 => format!("Compress ({count})"),
            Self::Compress => "Compress".to_string(),
            Self::InspectArchive => "Inspect Archive".to_string(),
            Self::ExtractArchive => "Extract Here".to_string(),
            Self::ShowPackageContents => "Show Package Contents".to_string(),
            Self::Paste if count > 1 => format!("Paste ({count})"),
            Self::Paste => "Paste".to_string(),
        }
    }

    pub(crate) fn icon_name(&self) -> &'static str {
        match self {
            Self::Open => "arrow-right",
            Self::Preview => "eye",
            Self::Rename | Self::BatchRename => "edit",
            Self::Copy => "copy",
            Self::Cut => "cut",
            Self::Trash => "trash",
            Self::Reveal => "folder",
            Self::OpenWithChooser
            | Self::ToggleOpenWith
            | Self::OpenWithApp(_)
            | Self::OpenWithOther => "arrow-right",
            Self::ToggleFavorite => "bookmark",
            Self::Compress | Self::InspectArchive => "archive",
            Self::ExtractArchive => "extract",
            Self::ShowPackageContents => "folder",
            Self::Paste => "paste",
        }
    }

    pub(crate) fn shortcut(&self, overrides: &BTreeMap<String, String>) -> Option<String> {
        let binding = match self {
            Self::Open => crate::shortcut::command_binding(overrides, "file-open"),
            Self::Preview => Some("space".to_string()),
            Self::Rename | Self::BatchRename => binding_for(overrides, "file-rename"),
            Self::Copy => binding_for(overrides, "file-copy"),
            Self::Cut => binding_for(overrides, "file-cut"),
            Self::Trash => binding_for(overrides, "file-trash"),
            Self::Paste => binding_for(overrides, "file-paste"),
            _ => None,
        }?;
        Some(display_binding(&binding))
    }

    pub(crate) fn selector(&self) -> String {
        let name = match self {
            Self::Open => "open",
            Self::Preview => "preview",
            Self::Rename => "rename",
            Self::BatchRename => "batch-rename",
            Self::Copy => "copy",
            Self::Cut => "cut",
            Self::Trash => "delete",
            Self::Reveal => "reveal",
            Self::OpenWithChooser => "open-with",
            Self::ToggleOpenWith => "open-with-toggle",
            Self::OpenWithApp(app) if app.is_default => "open-with-default-app",
            Self::OpenWithApp(_) => "open-with-app",
            Self::OpenWithOther => "open-with-other",
            Self::ToggleFavorite => "favorite",
            Self::Compress => "compress",
            Self::InspectArchive => "inspect-archive",
            Self::ExtractArchive => "extract-archive",
            Self::ShowPackageContents => "show-package-contents",
            Self::Paste => "paste",
        };
        format!("context-menu-{name}")
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FileContextMenu {
    pub(crate) focus_handle: FocusHandle,
    pub(crate) position: gpui::Point<gpui::Pixels>,
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) entries: Vec<FileEntry>,
    /// The (first) target opens by navigating into it: a folder or a link
    /// to one.
    pub(crate) target_is_folder: bool,
    /// The (first) target is a macOS package, which opens like a file.
    pub(crate) target_is_package: bool,
    pub(crate) focused: usize,
    pub(crate) open_with_apps: Vec<AppInfo>,
    pub(crate) open_with_expanded: bool,
}

/// What a window rendered, for tests that measure how much of the window a
/// change redraws.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RenderStats {
    /// Renders of the window's root view.
    pub(crate) root: usize,
    /// Time spent building the root view's element tree (children that
    /// build lazily, such as listing rows, are not included).
    pub(crate) root_time: Duration,
    /// Renders of the file listing area.
    pub(crate) listing: usize,
    /// List and Grid rows built by the listing's virtual lists.
    pub(crate) listing_rows: usize,
    /// Renders of the sidebar.
    pub(crate) sidebar: usize,
}
