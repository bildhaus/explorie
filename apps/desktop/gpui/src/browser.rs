use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use explorie_core::FileEntry;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::Value;

const MAX_NAVIGATION_HISTORY: usize = 50;
const MAX_FOLDER_VIEW_STATES: usize = 512;
const MAX_STORED_SELECTION: usize = 1_000;
const MAX_COLUMN_WIDTHS: usize = 64;
const MAX_COLUMN_VIEW_WIDTHS: usize = 512;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderViewState {
    pub view_mode: ViewMode,
    pub sort_key: SortKey,
    pub sort_direction: SortDirection,
    #[serde(default)]
    pub selected: Vec<PathBuf>,
    #[serde(default)]
    pub scroll_index: usize,
    #[serde(default = "default_grid_min_width")]
    pub grid_min_width: u16,
    #[serde(default)]
    pub show_preview_panel: bool,
    #[serde(default)]
    pub column_widths: HashMap<String, u16>,
}

fn default_grid_min_width() -> u16 {
    180
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    List,
    Grid,
    Column,
}

impl ViewMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::List => "List",
            Self::Grid => "Grid",
            Self::Column => "Column",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryFilter {
    All,
    Folders,
    Files,
}

impl EntryFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Folders => "Folders",
            Self::Files => "Files",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::All => Self::Folders,
            Self::Folders => Self::Files,
            Self::Files => Self::All,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Custom(String),
}

impl SortKey {
    pub fn label(&self) -> &str {
        match self {
            Self::Name => "Name",
            Self::Size => "Size",
            Self::Modified => "Modified",
            Self::Custom(key) => key,
        }
    }

    pub fn custom(key: impl Into<String>) -> Result<Self, String> {
        let key = key.into();
        if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err("custom sort keys must contain 1 to 128 visible characters".to_string());
        }
        if matches!(key.as_str(), "name" | "size" | "modified") {
            return Err("built-in sort keys cannot be represented as custom keys".to_string());
        }
        Ok(Self::Custom(key))
    }

    fn from_storage_key(key: String) -> Result<Self, String> {
        match key.as_str() {
            "name" => Ok(Self::Name),
            "size" => Ok(Self::Size),
            "modified" => Ok(Self::Modified),
            _ => Self::custom(key),
        }
    }

    fn storage_key(&self) -> &str {
        match self {
            Self::Name => "name",
            Self::Size => "size",
            Self::Modified => "modified",
            Self::Custom(key) => key,
        }
    }
}

impl Serialize for SortKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.storage_key())
    }
}

impl<'de> Deserialize<'de> for SortKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::from_storage_key(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDirection {
    Ascending,
    Descending,
}

impl SortDirection {
    pub fn indicator(self) -> &'static str {
        match self {
            Self::Ascending => "↑",
            Self::Descending => "↓",
        }
    }

    fn reversed(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BrowserState {
    path: PathBuf,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    listing: EntryListing,
    order: SortedOrder,
    visible_indices: Vec<usize>,
    visible_entries: Vec<Arc<FileEntry>>,
    /// The lowercased query behind `visible_indices` while they are a filtered
    /// view of `order`, so a longer query can narrow them without a rescan.
    narrowable_query: Option<String>,
    custom_columns: OnceLock<Vec<String>>,
    selected_index: OnceLock<Option<usize>>,
    selected: Arc<BTreeSet<PathBuf>>,
    selection_cursor: Option<PathBuf>,
    selection_anchor: Option<PathBuf>,
    show_hidden: bool,
    show_system_files: bool,
    filter: EntryFilter,
    sort_key: SortKey,
    sort_direction: SortDirection,
    view_mode: ViewMode,
    /// The view shown instead of `view_mode` while the listing holds a
    /// flat list of search results Column view can't show.
    search_results_view: Option<ViewMode>,
    search_query: String,
    folder_view_states: HashMap<PathBuf, FolderViewState>,
    pending_selection: Vec<PathBuf>,
    scroll_index: usize,
    grid_min_width: u16,
    show_preview_panel: bool,
    column_widths: HashMap<String, u16>,
    column_view_widths: HashMap<PathBuf, u16>,
}

impl BrowserState {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            back: Vec::new(),
            forward: Vec::new(),
            listing: EntryListing::default(),
            order: SortedOrder::default(),
            visible_indices: Vec::new(),
            visible_entries: Vec::new(),
            narrowable_query: None,
            custom_columns: OnceLock::new(),
            selected_index: OnceLock::new(),
            selected: Arc::default(),
            selection_cursor: None,
            selection_anchor: None,
            show_hidden: false,
            show_system_files: false,
            filter: EntryFilter::All,
            sort_key: SortKey::Name,
            sort_direction: SortDirection::Ascending,
            view_mode: ViewMode::List,
            search_results_view: None,
            search_query: String::new(),
            folder_view_states: HashMap::new(),
            pending_selection: Vec::new(),
            scroll_index: 0,
            grid_min_width: default_grid_min_width(),
            show_preview_panel: false,
            column_widths: HashMap::new(),
            column_view_widths: HashMap::new(),
        }
    }

    pub(super) fn fork_for_new_tab(&self) -> Self {
        let mut browser = self.clone();
        browser.back.clear();
        browser.forward.clear();
        browser.clear_listing();
        browser.restore_current_folder_view();
        browser
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub fn back_history(&self) -> &[PathBuf] {
        &self.back
    }

    pub fn forward_history(&self) -> &[PathBuf] {
        &self.forward
    }

    pub fn restore_navigation_history(
        &mut self,
        mut back: Vec<PathBuf>,
        mut forward: Vec<PathBuf>,
    ) {
        keep_newest_history(&mut back);
        keep_newest_history(&mut forward);
        self.back = back;
        self.forward = forward;
    }

    pub fn can_go_up(&self) -> bool {
        self.path.parent().is_some()
    }

    pub fn visible_entries(&self) -> &[Arc<FileEntry>] {
        &self.visible_entries
    }

    pub fn entries(&self) -> &[Arc<FileEntry>] {
        self.listing.entries()
    }

    /// Folder, file, size and hidden totals for the whole listing, kept up to
    /// date as entries change so rendering never rescans them.
    pub fn entry_stats(&self) -> EntryStats {
        self.listing.stats()
    }

    pub fn custom_columns(&self) -> Vec<String> {
        self.custom_columns
            .get_or_init(|| {
                let query = self.search_query.to_lowercase();
                let visibility = self.visibility(&query);
                collect_custom_columns(
                    (0..self.listing.len())
                        .filter(|&index| self.listing.is_visible(index, &visibility))
                        .map(|index| self.listing.entries[index].as_ref()),
                )
            })
            .clone()
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.selection_cursor
            .as_deref()
            .filter(|path| self.selected.contains(*path))
            .or_else(|| self.selected.first().map(PathBuf::as_path))
    }

    pub fn selection_count(&self) -> usize {
        self.selected.len()
    }

    pub(super) fn selection_snapshot(&self) -> Arc<BTreeSet<PathBuf>> {
        Arc::clone(&self.selected)
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.visible_entries
            .iter()
            .filter(|entry| self.selected.contains(&entry.path))
            .map(|entry| entry.path.clone())
            .collect()
    }

    pub fn selected_entries(&self) -> Vec<FileEntry> {
        self.visible_entries
            .iter()
            .filter(|entry| self.selected.contains(&entry.path))
            .map(|entry| entry.as_ref().clone())
            .collect()
    }

    pub fn is_selected(&self, path: &Path) -> bool {
        self.selected.contains(path)
    }

    pub fn selected_entry(&self) -> Option<&FileEntry> {
        self.selected_index()
            .and_then(|index| self.visible_entries.get(index))
            .map(Arc::as_ref)
    }

    /// Position of [`Self::selected_path`] in the visible entries, resolved at
    /// most once per selection or listing change.
    fn selected_index(&self) -> Option<usize> {
        *self.selected_index.get_or_init(|| {
            let selected = self.selected_path()?;
            self.visible_entries
                .iter()
                .position(|entry| entry.path == selected)
        })
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    pub fn show_system_files(&self) -> bool {
        self.show_system_files
    }

    pub fn filter(&self) -> EntryFilter {
        self.filter
    }

    pub fn sort_key(&self) -> SortKey {
        self.sort_key.clone()
    }

    pub fn sort_direction(&self) -> SortDirection {
        self.sort_direction
    }

    pub fn view_mode(&self) -> ViewMode {
        self.search_results_view.unwrap_or(self.view_mode)
    }

    pub fn set_view_mode(&mut self, view_mode: ViewMode) {
        self.view_mode = view_mode;
        self.search_results_view = None;
    }

    /// Show the listing's search results as a list while it holds them if
    /// the folder's own view is Column view, without changing the view the
    /// folder remembers; `false` returns to that view.
    pub(crate) fn show_search_results(&mut self, showing: bool) {
        self.search_results_view =
            (showing && self.view_mode == ViewMode::Column).then_some(ViewMode::List);
    }

    /// The filter, sort and search settings other listings (Column view
    /// columns) apply to show the same view of their entries.
    pub(crate) fn view_spec(&self) -> ViewSpec {
        ViewSpec {
            show_hidden: self.show_hidden,
            show_system_files: self.show_system_files,
            filter: self.filter,
            sort_key: self.sort_key.clone(),
            sort_direction: self.sort_direction,
            search_query: self.search_query.clone(),
        }
    }

    /// Whether `spec` still describes this browser's view, without allocating.
    pub(crate) fn view_matches(&self, spec: &ViewSpec) -> bool {
        spec.show_hidden == self.show_hidden
            && spec.show_system_files == self.show_system_files
            && spec.filter == self.filter
            && spec.sort_key == self.sort_key
            && spec.sort_direction == self.sort_direction
            && spec.search_query == self.search_query
    }

    pub fn apply_common_preferences(
        &mut self,
        show_hidden: bool,
        show_system_files: bool,
        filter: EntryFilter,
    ) {
        self.show_hidden = show_hidden;
        self.show_system_files = show_system_files;
        self.filter = filter;
        self.rebuild_visible_entries();
    }

    pub fn folder_view_states(&self) -> &HashMap<PathBuf, FolderViewState> {
        &self.folder_view_states
    }

    pub fn has_current_folder_view_state(&self) -> bool {
        self.folder_view_states.contains_key(&self.path)
    }

    pub fn restore_folder_view_states(&mut self, states: HashMap<PathBuf, FolderViewState>) {
        self.folder_view_states = states
            .into_iter()
            .take(MAX_FOLDER_VIEW_STATES)
            .map(|(path, mut state)| {
                state.selected.truncate(MAX_STORED_SELECTION);
                if state.column_widths.len() > MAX_COLUMN_WIDTHS {
                    state.column_widths = state
                        .column_widths
                        .into_iter()
                        .take(MAX_COLUMN_WIDTHS)
                        .collect();
                }
                (path, state)
            })
            .collect();
        self.restore_current_folder_view();
    }

    pub fn sync_folder_ui_state(
        &mut self,
        scroll_index: usize,
        grid_min_width: u16,
        show_preview_panel: bool,
    ) {
        self.scroll_index = scroll_index;
        self.grid_min_width = grid_min_width;
        self.show_preview_panel = show_preview_panel;
        self.capture_current_folder_view();
    }

    pub fn folder_ui_state(&self) -> (usize, u16, bool) {
        (
            self.scroll_index,
            self.grid_min_width,
            self.show_preview_panel,
        )
    }

    pub fn column_width(&self, key: &str) -> Option<u16> {
        self.column_widths.get(key).copied()
    }

    pub fn set_column_width(&mut self, key: String, width: u16) {
        self.column_widths.insert(key, width);
    }

    pub fn column_view_widths(&self) -> &HashMap<PathBuf, u16> {
        &self.column_view_widths
    }

    pub fn restore_column_view_widths(&mut self, widths: HashMap<PathBuf, u16>) {
        self.column_view_widths = widths.into_iter().take(MAX_COLUMN_VIEW_WIDTHS).collect();
    }

    pub fn column_view_width(&self, path: &Path) -> Option<u16> {
        self.column_view_widths.get(path).copied()
    }

    pub fn set_column_view_width(&mut self, path: PathBuf, width: u16) {
        if !self.column_view_widths.contains_key(&path)
            && self.column_view_widths.len() >= MAX_COLUMN_VIEW_WIDTHS
            && let Some(stale) = self.column_view_widths.keys().next().cloned()
        {
            self.column_view_widths.remove(&stale);
        }
        self.column_view_widths.insert(path, width);
    }

    pub fn apply_preferences(
        &mut self,
        show_hidden: bool,
        show_system_files: bool,
        filter: EntryFilter,
        sort_key: SortKey,
        sort_direction: SortDirection,
        view_mode: ViewMode,
    ) {
        self.show_hidden = show_hidden;
        self.show_system_files = show_system_files;
        self.filter = filter;
        self.sort_key = sort_key;
        self.sort_direction = sort_direction;
        self.view_mode = view_mode;
        self.rebuild_visible_entries();
    }

    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    pub fn push_search_text(&mut self, text: &str) {
        self.search_query.push_str(text);
        self.search_query_changed();
    }

    pub fn set_search_query(&mut self, query: String) {
        self.search_query = query;
        self.search_query_changed();
    }

    pub fn pop_search_character(&mut self) {
        self.search_query.pop();
        self.search_query_changed();
    }

    pub fn clear_search(&mut self) {
        self.search_query.clear();
        self.search_query_changed();
    }

    pub fn navigate(&mut self, path: PathBuf) -> bool {
        if path == self.path {
            return false;
        }
        self.capture_current_folder_view();
        self.back.push(std::mem::replace(&mut self.path, path));
        keep_newest_history(&mut self.back);
        self.forward.clear();
        self.clear_listing();
        self.restore_current_folder_view();
        true
    }

    pub fn go_back(&mut self) -> bool {
        let Some(path) = self.back.pop() else {
            return false;
        };
        self.capture_current_folder_view();
        self.forward.push(std::mem::replace(&mut self.path, path));
        keep_newest_history(&mut self.forward);
        self.clear_listing();
        self.restore_current_folder_view();
        true
    }

    pub fn go_forward(&mut self) -> bool {
        let Some(path) = self.forward.pop() else {
            return false;
        };
        self.capture_current_folder_view();
        self.back.push(std::mem::replace(&mut self.path, path));
        keep_newest_history(&mut self.back);
        self.clear_listing();
        self.restore_current_folder_view();
        true
    }

    pub fn go_to_back_history(&mut self, index: usize) -> bool {
        let Some(actual_index) = self.back.len().checked_sub(index + 1) else {
            return false;
        };
        self.capture_current_folder_view();
        let mut skipped = self.back.split_off(actual_index);
        let target = skipped.remove(0);
        self.forward.push(std::mem::replace(&mut self.path, target));
        self.forward.extend(skipped.into_iter().rev());
        keep_newest_history(&mut self.forward);
        self.clear_listing();
        self.restore_current_folder_view();
        true
    }

    pub fn go_to_forward_history(&mut self, index: usize) -> bool {
        let Some(actual_index) = self.forward.len().checked_sub(index + 1) else {
            return false;
        };
        self.capture_current_folder_view();
        let target = self.forward.remove(actual_index);
        let skipped = self.forward.split_off(actual_index);
        self.back.push(std::mem::replace(&mut self.path, target));
        self.back.extend(skipped.into_iter().rev());
        keep_newest_history(&mut self.back);
        self.clear_listing();
        self.restore_current_folder_view();
        true
    }

    pub fn clear_navigation_history(&mut self) {
        self.back.clear();
        self.forward.clear();
    }

    pub fn go_up(&mut self) -> bool {
        let Some(parent) = self.path.parent().map(Path::to_path_buf) else {
            return false;
        };
        self.navigate(parent)
    }

    /// Replace the whole listing. Accepts owned entries or entries already
    /// shared with another view (Column view keeps the same `Arc`s).
    pub fn replace_entries<E: Into<Arc<FileEntry>>>(&mut self, entries: Vec<E>) {
        self.listing = EntryListing::new(entries.into_iter().map(Into::into).collect());
        self.order.invalidate();
        self.rebuild_visible_entries();
        if !self.pending_selection.is_empty() {
            let selection = std::mem::take(&mut self.pending_selection);
            self.replace_selection(selection);
        }
    }

    /// Append a progressive result batch without re-sorting every result seen
    /// so far. The authoritative completion replaces and sorts the full set.
    pub fn append_progressive_entries(&mut self, entries: Vec<FileEntry>) {
        if entries.is_empty() {
            return;
        }
        let query = self.search_query.to_lowercase();
        self.visible_indices.reserve(entries.len());
        self.visible_entries.reserve(entries.len());
        for entry in entries {
            let entry = Arc::new(entry);
            let index = self.listing.push(Arc::clone(&entry));
            if self.listing.is_visible(index, &self.visibility(&query)) {
                self.visible_indices.push(index);
                self.visible_entries.push(entry);
            }
        }
        // The appended rows are unsorted, so later queries must rescan.
        self.order.invalidate();
        self.narrowable_query = None;
        self.custom_columns = OnceLock::new();
    }

    /// Patch the listing with freshly read entries (`None` for a path that no
    /// longer exists) instead of replacing it. Untouched entries keep their
    /// sorted order and selection; returns whether anything changed.
    pub fn apply_entry_changes(&mut self, changes: Vec<(PathBuf, Option<FileEntry>)>) -> bool {
        let Some(patch) = self.listing.apply_changes(changes) else {
            return false;
        };
        self.order.apply_patch(&self.listing, &patch);
        if self.narrowable_query.is_some()
            && self.order.is_sorted_by(&self.sort_key, self.sort_direction)
        {
            self.patch_visible_entries(&patch);
        } else {
            self.rebuild_visible_entries();
        }
        true
    }

    /// Carry the visible rows through a patch: untouched rows keep their
    /// place (moved, not re-filtered) and changed rows that are still shown
    /// are merged in, so only the changed entries are compared.
    fn patch_visible_entries(&mut self, patch: &ListingPatch) {
        let query = self.search_query.to_lowercase();
        let visibility = self.visibility(&query);
        let kept: Vec<_> = std::mem::take(&mut self.visible_indices)
            .into_iter()
            .zip(std::mem::take(&mut self.visible_entries))
            .filter_map(|(index, entry)| patch.kept[index].map(|index| (index, entry)))
            .collect();
        let incoming: Vec<_> = patch
            .reinserted
            .iter()
            .filter(|&&index| self.listing.is_visible(index, &visibility))
            .map(|&index| (index, Arc::clone(&self.listing.entries[index])))
            .collect();
        let merged = merge_sorted(kept, incoming, |left, right| {
            self.listing
                .compare(left, right, &self.sort_key, self.sort_direction)
        });
        (self.visible_indices, self.visible_entries) = merged.into_iter().unzip();
        self.narrowable_query = Some(query);
        self.visible_entries_changed();
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.rebuild_visible_entries();
    }

    pub fn toggle_system_files(&mut self) {
        self.show_system_files = !self.show_system_files;
        self.rebuild_visible_entries();
    }

    pub fn cycle_filter(&mut self) {
        self.filter = self.filter.next();
        self.rebuild_visible_entries();
    }

    pub fn set_filter(&mut self, filter: EntryFilter) {
        self.filter = filter;
        self.rebuild_visible_entries();
    }

    pub fn set_sort(&mut self, key: SortKey) {
        if self.sort_key == key {
            self.sort_direction = self.sort_direction.reversed();
        } else {
            self.sort_key = key;
            self.sort_direction = SortDirection::Ascending;
        }
        self.resort_visible_entries();
    }

    #[cfg(test)]
    pub fn cycle_sort_key(&mut self) {
        self.sort_key = match self.sort_key {
            SortKey::Name => SortKey::Size,
            SortKey::Size => SortKey::Modified,
            SortKey::Modified | SortKey::Custom(_) => SortKey::Name,
        };
        self.sort_direction = SortDirection::Ascending;
        self.resort_visible_entries();
    }

    pub fn select(&mut self, path: PathBuf) {
        if let Some(index) = self
            .visible_entries
            .iter()
            .position(|entry| entry.path == path)
        {
            self.select_index(index);
        }
    }

    fn select_index(&mut self, index: usize) {
        let path = self.visible_entries[index].path.clone();
        self.selected = Arc::new(BTreeSet::from([path.clone()]));
        self.selection_cursor = Some(path.clone());
        self.selection_anchor = Some(path);
        self.selected_index = OnceLock::from(Some(index));
    }

    pub fn toggle_selection(&mut self, path: PathBuf) {
        if !self.visible_entries.iter().any(|entry| entry.path == path) {
            return;
        }
        let selected = Arc::make_mut(&mut self.selected);
        if !selected.remove(&path) {
            selected.insert(path.clone());
        }
        self.selection_cursor = self.selected.contains(&path).then(|| path.clone());
        if self.selection_cursor.is_none() {
            self.selection_cursor = self.selected.first().cloned();
        }
        self.selection_anchor = Some(path);
        self.selected_index = OnceLock::new();
    }

    pub fn select_range_to(&mut self, path: PathBuf) {
        let Some(target) = self
            .visible_entries
            .iter()
            .position(|entry| entry.path == path)
        else {
            return;
        };
        let anchor_path = self
            .selection_anchor
            .as_ref()
            .or(self.selection_cursor.as_ref())
            .unwrap_or(&path);
        let anchor = self
            .visible_entries
            .iter()
            .position(|entry| &entry.path == anchor_path)
            .unwrap_or(target);
        let (start, end) = if anchor <= target {
            (anchor, target)
        } else {
            (target, anchor)
        };
        self.selected = Arc::new(
            self.visible_entries[start..=end]
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
        );
        self.selection_cursor = Some(path);
        if self.selection_anchor.is_none() {
            self.selection_anchor = self
                .visible_entries
                .get(anchor)
                .map(|entry| entry.path.clone());
        }
        self.selected_index = OnceLock::from(Some(target));
    }

    pub fn select_all(&mut self) {
        self.selected = Arc::new(
            self.visible_entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
        );
        self.selection_cursor = self.visible_entries.first().map(|entry| entry.path.clone());
        self.selection_anchor = self.selection_cursor.clone();
        self.selected_index = OnceLock::from((!self.visible_entries.is_empty()).then_some(0));
    }

    pub fn replace_selection<I>(&mut self, paths: I)
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let requested: BTreeSet<_> = paths.into_iter().collect();
        self.selected = Arc::new(
            self.visible_entries
                .iter()
                .filter(|entry| requested.contains(&entry.path))
                .map(|entry| entry.path.clone())
                .collect(),
        );
        self.selection_cursor = self.selected.first().cloned();
        self.selection_anchor = self.selection_cursor.clone();
        self.selected_index = OnceLock::new();
    }

    pub fn select_prefix(&mut self, prefix: &str) -> Option<usize> {
        let prefix = prefix.to_lowercase();
        let index = self
            .visible_indices
            .iter()
            .position(|&entry| self.listing.keys[entry].lower.starts_with(&prefix))?;
        self.select_index(index);
        Some(index)
    }

    pub fn clear_selection(&mut self) {
        self.pending_selection.clear();
        self.selected = Arc::default();
        self.selection_cursor = None;
        self.selection_anchor = None;
        self.selected_index = OnceLock::from(None);
    }

    pub fn select_next(&mut self) -> Option<usize> {
        self.select_offset(1)
    }

    pub fn select_previous(&mut self) -> Option<usize> {
        self.select_offset(-1)
    }

    pub fn select_offset(&mut self, offset: isize) -> Option<usize> {
        if self.visible_entries.is_empty() {
            self.clear_selection();
            return None;
        }
        let next = self.selected_index().map_or(0, |index| {
            index
                .saturating_add_signed(offset)
                .min(self.visible_entries.len() - 1)
        });
        self.select_index(next);
        Some(next)
    }

    pub fn select_next_range(&mut self) -> Option<usize> {
        self.select_range_offset(1)
    }

    pub fn select_previous_range(&mut self) -> Option<usize> {
        self.select_range_offset(-1)
    }

    pub fn select_range_offset(&mut self, offset: isize) -> Option<usize> {
        if self.visible_entries.is_empty() {
            self.clear_selection();
            return None;
        }
        let current = self.selected_index().unwrap_or_else(|| {
            if offset > 0 {
                0
            } else {
                self.visible_entries.len() - 1
            }
        });
        if self.selection_anchor.is_none() {
            self.selection_anchor = self
                .visible_entries
                .get(current)
                .map(|entry| entry.path.clone());
        }
        let next = current
            .saturating_add_signed(offset)
            .min(self.visible_entries.len() - 1);
        self.select_range_to(self.visible_entries[next].path.clone());
        Some(next)
    }

    fn clear_listing(&mut self) {
        self.listing = EntryListing::default();
        self.order.invalidate();
        self.visible_indices.clear();
        self.visible_entries.clear();
        self.narrowable_query = None;
        self.custom_columns = OnceLock::new();
        self.clear_selection();
    }

    fn capture_current_folder_view(&mut self) {
        if !self.folder_view_states.contains_key(&self.path)
            && self.folder_view_states.len() >= MAX_FOLDER_VIEW_STATES
            && let Some(stale) = self
                .folder_view_states
                .keys()
                .find(|path| path.as_path() != self.path)
                .cloned()
        {
            self.folder_view_states.remove(&stale);
        }
        let mut selected = if self.listing.is_empty() && !self.pending_selection.is_empty() {
            self.pending_selection.clone()
        } else {
            self.selected.iter().cloned().collect()
        };
        selected.truncate(MAX_STORED_SELECTION);
        let column_widths = self
            .column_widths
            .iter()
            .take(MAX_COLUMN_WIDTHS)
            .map(|(key, width)| (key.clone(), *width))
            .collect();
        self.folder_view_states.insert(
            self.path.clone(),
            FolderViewState {
                view_mode: self.view_mode,
                sort_key: self.sort_key.clone(),
                sort_direction: self.sort_direction,
                selected,
                scroll_index: self.scroll_index,
                grid_min_width: self.grid_min_width,
                show_preview_panel: self.show_preview_panel,
                column_widths,
            },
        );
    }

    fn restore_current_folder_view(&mut self) {
        let Some(state) = self.folder_view_states.get(&self.path).cloned() else {
            self.pending_selection.clear();
            self.scroll_index = 0;
            return;
        };
        self.view_mode = state.view_mode;
        self.sort_key = state.sort_key;
        self.sort_direction = state.sort_direction;
        self.pending_selection = state.selected;
        self.scroll_index = state.scroll_index;
        self.grid_min_width = state.grid_min_width;
        self.show_preview_panel = state.show_preview_panel;
        self.column_widths = state.column_widths;
    }

    fn visibility<'a>(&self, query: &'a str) -> Visibility<'a> {
        Visibility {
            show_hidden: self.show_hidden,
            show_system_files: self.show_system_files,
            filter: self.filter,
            query,
        }
    }

    /// Filter the sorted base order. Sorting only happens when the sort key,
    /// direction or listing changed; filter and search changes never re-sort.
    fn rebuild_visible_entries(&mut self) {
        self.filter_sorted_order();
        self.visible_entries_changed();
    }

    /// A new sort shows the same entries in another order, so the selection
    /// (always a subset of the visible entries) needs no reconciling.
    fn resort_visible_entries(&mut self) {
        self.filter_sorted_order();
        self.selected_index = OnceLock::new();
    }

    fn filter_sorted_order(&mut self) {
        let query = self.search_query.to_lowercase();
        let visibility = self.visibility(&query);
        let order = self
            .order
            .ensure(&self.listing, &self.sort_key, self.sort_direction);
        self.visible_indices = self.listing.visible(order, &visibility);
        self.visible_entries = self
            .visible_indices
            .iter()
            .map(|&index| Arc::clone(&self.listing.entries[index]))
            .collect();
        self.narrowable_query = Some(query);
    }

    /// A query that extends the previous one can only match a subset of the
    /// previous results, so narrow those instead of rescanning the listing.
    fn search_query_changed(&mut self) {
        let query = self.search_query.to_lowercase();
        let Some(previous) = self.narrowable_query.as_deref() else {
            return self.rebuild_visible_entries();
        };
        if query == previous {
            return;
        }
        if !query.starts_with(previous) {
            return self.rebuild_visible_entries();
        }
        let keys = &self.listing.keys;
        (self.visible_indices, self.visible_entries) = std::mem::take(&mut self.visible_indices)
            .into_iter()
            .zip(std::mem::take(&mut self.visible_entries))
            .filter(|(index, _)| keys[*index].lower.contains(query.as_str()))
            .unzip();
        self.narrowable_query = Some(query);
        self.visible_entries_changed();
    }

    fn visible_entries_changed(&mut self) {
        self.custom_columns = OnceLock::new();
        self.selected_index = OnceLock::new();

        // Walk the listing once instead of scanning it for every selected
        // path, and keep the selection as is when every path is still shown.
        if !self.selected.is_empty() {
            let still_shown: Vec<&PathBuf> = self
                .visible_entries
                .iter()
                .map(|entry| &entry.path)
                .filter(|path| self.selected.contains(*path))
                .collect();
            if still_shown.len() != self.selected.len() {
                self.selected = Arc::new(still_shown.into_iter().cloned().collect());
            }
        }
        if self
            .selection_cursor
            .as_ref()
            .is_some_and(|path| !self.selected.contains(path))
        {
            self.selection_cursor = self.selected.first().cloned();
        }
        if self
            .selection_anchor
            .as_ref()
            .is_some_and(|path| !self.selected.contains(path))
        {
            self.selection_anchor = self.selection_cursor.clone();
        }
    }
}

fn keep_newest_history(history: &mut Vec<PathBuf>) {
    if history.len() > MAX_NAVIGATION_HISTORY {
        history.drain(..history.len() - MAX_NAVIGATION_HISTORY);
    }
}

/// The settings that decide which entries a listing shows and in what order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewSpec {
    pub(crate) show_hidden: bool,
    pub(crate) show_system_files: bool,
    pub(crate) filter: EntryFilter,
    pub(crate) sort_key: SortKey,
    pub(crate) sort_direction: SortDirection,
    pub(crate) search_query: String,
}

struct Visibility<'a> {
    show_hidden: bool,
    show_system_files: bool,
    filter: EntryFilter,
    /// Already lowercased.
    query: &'a str,
}

/// Folder, file, size and hidden totals for one listing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EntryStats {
    pub folders: usize,
    pub files: usize,
    pub file_bytes: u64,
    pub hidden: usize,
}

impl EntryStats {
    fn add(&mut self, entry: &FileEntry) {
        if is_folder_like(entry) {
            self.folders += 1;
        } else {
            self.files += 1;
            self.file_bytes = self.file_bytes.saturating_add(entry.size);
        }
        self.hidden += usize::from(entry.hidden);
    }

    fn remove(&mut self, entry: &FileEntry) {
        if is_folder_like(entry) {
            self.folders -= 1;
        } else {
            self.files -= 1;
            self.file_bytes = self.file_bytes.saturating_sub(entry.size);
        }
        self.hidden -= usize::from(entry.hidden);
    }
}

/// Sort and filter data derived once from an entry's name, so sorting and
/// searching never allocate per entry.
#[derive(Clone, Debug)]
struct EntryKey {
    /// The [`natural_sort_key`] of a name with digits. Without digits the
    /// lowercased name already is its natural key, so none is stored.
    natural: Option<Box<[u8]>>,
    /// The lowercased name, for case-insensitive search.
    lower: Box<str>,
    is_system: bool,
}

impl EntryKey {
    fn new(entry: &FileEntry) -> Self {
        let name = entry_name(entry).to_string_lossy();
        let lower = name.to_lowercase();
        Self {
            natural: lower
                .bytes()
                .any(|byte| byte.is_ascii_digit())
                .then(|| natural_sort_key(&lower)),
            is_system: is_system_name(&name),
            lower: lower.into_boxed_str(),
        }
    }

    fn natural(&self) -> &[u8] {
        self.natural.as_deref().unwrap_or(self.lower.as_bytes())
    }
}

/// The fields a sort compares, copied out of the entries so sorting large
/// listings compares compact items instead of chasing entry pointers.
#[derive(Clone, Copy)]
struct SortItem<'a> {
    folder: bool,
    size: u64,
    modified: SystemTime,
    natural: &'a [u8],
    index: usize,
}

/// Entries in listing order with their precomputed keys and totals.
#[derive(Clone, Debug, Default)]
pub(crate) struct EntryListing {
    entries: Vec<Arc<FileEntry>>,
    keys: Vec<EntryKey>,
    stats: EntryStats,
}

impl EntryListing {
    pub(crate) fn new(entries: Vec<Arc<FileEntry>>) -> Self {
        let mut stats = EntryStats::default();
        let keys = entries
            .iter()
            .map(|entry| {
                stats.add(entry);
                EntryKey::new(entry)
            })
            .collect();
        Self {
            entries,
            keys,
            stats,
        }
    }

    pub(crate) fn entries(&self) -> &[Arc<FileEntry>] {
        &self.entries
    }

    pub(crate) fn stats(&self) -> EntryStats {
        self.stats
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn push(&mut self, entry: Arc<FileEntry>) -> usize {
        self.stats.add(&entry);
        self.keys.push(EntryKey::new(&entry));
        self.entries.push(entry);
        self.entries.len() - 1
    }

    fn is_visible(&self, index: usize, visibility: &Visibility<'_>) -> bool {
        let entry = &self.entries[index];
        let key = &self.keys[index];
        (visibility.show_hidden || !entry.hidden)
            && (visibility.show_system_files || !key.is_system)
            && match visibility.filter {
                EntryFilter::All => true,
                // Like the search service's type filter: anything that is a
                // directory on disk (packages included) counts as a folder.
                EntryFilter::Folders => is_directory_entry(entry),
                EntryFilter::Files => !is_directory_entry(entry),
            }
            && (visibility.query.is_empty() || key.lower.contains(visibility.query))
    }

    /// The visible subset of `order`, keeping its order.
    fn visible(&self, order: &[usize], visibility: &Visibility<'_>) -> Vec<usize> {
        order
            .iter()
            .copied()
            .filter(|&index| self.is_visible(index, visibility))
            .collect()
    }

    /// The entries `spec` shows, in its order, given `order` sorted for it.
    pub(crate) fn visible_for(&self, order: &[usize], spec: &ViewSpec) -> Vec<Arc<FileEntry>> {
        let query = spec.search_query.to_lowercase();
        let visibility = Visibility {
            show_hidden: spec.show_hidden,
            show_system_files: spec.show_system_files,
            filter: spec.filter,
            query: &query,
        };
        order
            .iter()
            .filter(|&&index| self.is_visible(index, &visibility))
            .map(|&index| Arc::clone(&self.entries[index]))
            .collect()
    }

    fn sorted(&self, sort_key: &SortKey, sort_direction: SortDirection) -> Vec<usize> {
        let mut items: Vec<_> = (0..self.entries.len())
            .map(|index| self.sort_item(index))
            .collect();
        items.sort_unstable_by(|left, right| {
            self.compare_items(left, right, sort_key, sort_direction)
        });
        items.into_iter().map(|item| item.index).collect()
    }

    fn sort_item(&self, index: usize) -> SortItem<'_> {
        let entry = &self.entries[index];
        SortItem {
            folder: is_folder_like(entry),
            size: entry.size,
            modified: entry.modified,
            natural: self.keys[index].natural(),
            index,
        }
    }

    fn compare(
        &self,
        left: usize,
        right: usize,
        sort_key: &SortKey,
        sort_direction: SortDirection,
    ) -> Ordering {
        self.compare_items(
            &self.sort_item(left),
            &self.sort_item(right),
            sort_key,
            sort_direction,
        )
    }

    /// Folders first, then the sort key in `sort_direction`, then the natural
    /// name order as a tiebreak. Name sorts reverse the whole name order.
    fn compare_items(
        &self,
        left: &SortItem<'_>,
        right: &SortItem<'_>,
        sort_key: &SortKey,
        sort_direction: SortDirection,
    ) -> Ordering {
        right.folder.cmp(&left.folder).then_with(|| {
            let directed = |order: Ordering| match sort_direction {
                SortDirection::Ascending => order,
                SortDirection::Descending => order.reverse(),
            };
            let names = || {
                left.natural
                    .cmp(right.natural)
                    .then_with(|| self.name_tiebreak(left.index, right.index))
            };
            match sort_key {
                SortKey::Name => directed(names()),
                SortKey::Size => directed(left.size.cmp(&right.size)).then_with(names),
                SortKey::Modified => directed(left.modified.cmp(&right.modified)).then_with(names),
                SortKey::Custom(key) => directed(compare_custom_fields(
                    &self.entries[left.index],
                    &self.entries[right.index],
                    key,
                ))
                .then_with(names),
            }
        })
    }

    /// Deterministic order for names with equal natural keys: the exact name,
    /// then the full path, then listing order.
    fn name_tiebreak(&self, left: usize, right: usize) -> Ordering {
        let (left_entry, right_entry) = (&self.entries[left], &self.entries[right]);
        entry_name(left_entry)
            .cmp(entry_name(right_entry))
            .then_with(|| left_entry.path.cmp(&right_entry.path))
            .then_with(|| left.cmp(&right))
    }

    /// Apply re-read entries: `Some` inserts or updates the entry at that
    /// path, `None` removes it. Returns how indices moved, or `None` when the
    /// listing did not change.
    pub(crate) fn apply_changes(
        &mut self,
        changes: Vec<(PathBuf, Option<FileEntry>)>,
    ) -> Option<ListingPatch> {
        if changes.is_empty() {
            return None;
        }
        let mut pending: HashMap<PathBuf, Option<FileEntry>> = changes.into_iter().collect();
        // Only entries whose path length matches a change need a hash lookup.
        let mut lengths = vec![
            false;
            pending
                .keys()
                .map(|path| path.as_os_str().len())
                .max()
                .unwrap_or(0)
                + 1
        ];
        for path in pending.keys() {
            lengths[path.as_os_str().len()] = true;
        }

        let previous_entries = std::mem::take(&mut self.entries);
        let previous_keys = std::mem::take(&mut self.keys);
        let mut kept = Vec::with_capacity(previous_entries.len());
        let mut reinserted = Vec::new();
        let mut changed = false;
        self.entries.reserve(previous_entries.len() + pending.len());
        self.keys.reserve(previous_entries.len() + pending.len());
        for (entry, key) in previous_entries.into_iter().zip(previous_keys) {
            let change = if lengths
                .get(entry.path.as_os_str().len())
                .copied()
                .unwrap_or(false)
            {
                pending.remove(entry.path.as_path())
            } else {
                None
            };
            match change {
                Some(Some(updated)) if !same_listing_metadata(&entry, &updated) => {
                    self.stats.remove(&entry);
                    self.stats.add(&updated);
                    kept.push(None);
                    reinserted.push(self.entries.len());
                    // The key only depends on the name, which a path keeps.
                    self.entries.push(Arc::new(updated));
                    self.keys.push(key);
                    changed = true;
                }
                Some(None) => {
                    self.stats.remove(&entry);
                    kept.push(None);
                    changed = true;
                }
                Some(Some(_)) | None => {
                    kept.push(Some(self.entries.len()));
                    self.entries.push(entry);
                    self.keys.push(key);
                }
            }
        }
        let mut inserted: Vec<_> = pending.into_values().flatten().collect();
        inserted.sort_by(|left, right| left.path.cmp(&right.path));
        for entry in inserted {
            reinserted.push(self.push(Arc::new(entry)));
            changed = true;
        }
        changed.then_some(ListingPatch { kept, reinserted })
    }
}

/// How [`EntryListing::apply_changes`] moved listing indices.
pub(crate) struct ListingPatch {
    /// For each previous index, its new index when the entry kept its place
    /// in any sorted order.
    kept: Vec<Option<usize>>,
    /// New indices of inserted or updated entries that must be sorted in.
    reinserted: Vec<usize>,
}

/// Listing indices sorted for one sort key and direction.
#[derive(Clone, Debug, Default)]
pub(crate) struct SortedOrder {
    indices: Vec<usize>,
    sorted_by: Option<(SortKey, SortDirection)>,
}

impl SortedOrder {
    /// The listing sorted by `sort_key`, re-sorting only if it changed.
    pub(crate) fn ensure(
        &mut self,
        listing: &EntryListing,
        sort_key: &SortKey,
        sort_direction: SortDirection,
    ) -> &[usize] {
        if !self
            .sorted_by
            .as_ref()
            .is_some_and(|(key, direction)| key == sort_key && *direction == sort_direction)
        {
            self.indices = listing.sorted(sort_key, sort_direction);
            self.sorted_by = Some((sort_key.clone(), sort_direction));
        }
        &self.indices
    }

    pub(crate) fn invalidate(&mut self) {
        self.indices.clear();
        self.sorted_by = None;
    }

    /// Merge a patch's inserted and updated entries into the existing order
    /// instead of re-sorting the whole listing.
    pub(crate) fn apply_patch(&mut self, listing: &EntryListing, patch: &ListingPatch) {
        let Some((sort_key, sort_direction)) = &self.sorted_by else {
            return;
        };
        let kept = self
            .indices
            .iter()
            .filter_map(|&previous| patch.kept[previous])
            .map(|index| (index, ()))
            .collect();
        let incoming = patch.reinserted.iter().map(|&index| (index, ())).collect();
        self.indices = merge_sorted(kept, incoming, |left, right| {
            listing.compare(left, right, sort_key, *sort_direction)
        })
        .into_iter()
        .map(|(index, ())| index)
        .collect();
    }

    fn is_sorted_by(&self, sort_key: &SortKey, sort_direction: SortDirection) -> bool {
        self.sorted_by
            .as_ref()
            .is_some_and(|(key, direction)| key == sort_key && *direction == sort_direction)
    }
}

/// Merge unsorted `incoming` items into `kept`, which is sorted by
/// `compare`. Each incoming item finds its place by binary search, so only
/// O(k log n) comparisons touch entries while the rest is moved in bulk.
fn merge_sorted<T>(
    kept: Vec<(usize, T)>,
    mut incoming: Vec<(usize, T)>,
    compare: impl Fn(usize, usize) -> Ordering,
) -> Vec<(usize, T)> {
    if incoming.is_empty() {
        return kept;
    }
    incoming.sort_by(|left, right| compare(left.0, right.0));
    let mut start = 0;
    let positions: Vec<usize> = incoming
        .iter()
        .map(|(index, _)| {
            start += kept[start..]
                .partition_point(|(existing, _)| compare(*existing, *index) == Ordering::Less);
            start
        })
        .collect();
    let mut merged = Vec::with_capacity(kept.len() + incoming.len());
    let mut kept = kept.into_iter();
    let mut taken = 0;
    for (item, position) in incoming.into_iter().zip(positions) {
        merged.extend(kept.by_ref().take(position - taken));
        taken = position;
        merged.push(item);
    }
    merged.extend(kept);
    merged
}

/// Whether the browser treats `entry` as a folder: real folders other than
/// packages, and links (symlinks, junctions) that resolve to a folder. These
/// open by navigating (a link keeps its own path), get a child column, sort
/// and count with the folders, and accept drops. Packages behave like files.
pub(crate) fn is_folder_like(entry: &FileEntry) -> bool {
    (entry.is_dir || entry.link_target_is_dir) && !entry.is_package
}

/// Whether `entry` is a directory on disk, directly or through a link:
/// folders, packages and links to either. Such items have no file contents
/// to preview.
pub(crate) fn is_directory_entry(entry: &FileEntry) -> bool {
    entry.is_dir || entry.link_target_is_dir
}

/// Whether a re-read entry would render and sort exactly like the listed one.
fn same_listing_metadata(listed: &FileEntry, fresh: &FileEntry) -> bool {
    listed.size == fresh.size
        && listed.modified == fresh.modified
        && listed.hidden == fresh.hidden
        && listed.is_dir == fresh.is_dir
        && listed.is_symlink == fresh.is_symlink
        && listed.is_junction == fresh.is_junction
        && listed.has_xattrs == fresh.has_xattrs
        && listed.link_target == fresh.link_target
        && listed.is_package == fresh.is_package
        && listed.link_target_is_dir == fresh.link_target_is_dir
        && listed.is_cloud_placeholder == fresh.is_cloud_placeholder
        && listed.tags == fresh.tags
        && listed.custom == fresh.custom
}

/// A case-insensitive natural sort key for an already lowercased name: each
/// run of ASCII digits compares by numeric value (leading zeros ignored, any
/// length), everything else by code point, so "file2" sorts before "file10".
///
/// A digit run becomes `'0'`, its significant-digit count and the digits.
/// The `'0'` marker keeps runs ordered against other characters exactly as
/// the digits they replace, and a longer count means a larger number. Counts
/// below 255 take one byte; longer runs use `0xFF` and a big-endian `u64`.
fn natural_sort_key(lower: &str) -> Box<[u8]> {
    let bytes = lower.as_bytes();
    let mut key = Vec::with_capacity(bytes.len() + 2);
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            key.push(bytes[index]);
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let digits = &bytes[start..index];
        let significant = &digits[digits
            .iter()
            .position(|digit| *digit != b'0')
            .unwrap_or(digits.len())..];
        key.push(b'0');
        match u8::try_from(significant.len()) {
            Ok(count) if count < u8::MAX => key.push(count),
            _ => {
                key.push(u8::MAX);
                key.extend_from_slice(&(significant.len() as u64).to_be_bytes());
            }
        }
        key.extend_from_slice(significant);
    }
    key.into_boxed_slice()
}

const CUSTOM_COLUMN_SCAN_LIMIT: usize = 500;
const CUSTOM_COLUMN_CANDIDATES: [&str; 4] = ["status", "type", "category", "priority"];

#[cfg(test)]
pub fn custom_columns(entries: &[FileEntry]) -> Vec<String> {
    collect_custom_columns(entries.iter())
}

fn collect_custom_columns<'a>(entries: impl Iterator<Item = &'a FileEntry>) -> Vec<String> {
    entries
        .take(CUSTOM_COLUMN_SCAN_LIMIT)
        .flat_map(|entry| entry.custom.iter())
        .filter(|(key, value)| is_custom_column_candidate(key, value))
        .map(|(key, _)| key.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn is_custom_column_candidate(key: &str, value: &Value) -> bool {
    if !CUSTOM_COLUMN_CANDIDATES
        .iter()
        .any(|candidate| key.eq_ignore_ascii_case(candidate))
    {
        return false;
    }
    match value {
        Value::Bool(_) | Value::Number(_) => true,
        Value::String(value) => value.chars().count() <= 50,
        Value::Null | Value::Array(_) | Value::Object(_) => false,
    }
}

fn compare_custom_fields(left: &FileEntry, right: &FileEntry, key: &str) -> Ordering {
    match (left.custom.get(key), right.custom.get(key)) {
        (Some(Value::String(left)), Some(Value::String(right))) => left.cmp(right),
        (Some(Value::Number(left)), Some(Value::Number(right))) => left
            .as_f64()
            .partial_cmp(&right.as_f64())
            .unwrap_or(Ordering::Equal),
        (Some(Value::Bool(left)), Some(Value::Bool(right))) => left.cmp(right),
        (Some(Value::Null), Some(Value::Null)) => Ordering::Equal,
        (Some(Value::Null), Some(_)) => Ordering::Less,
        (Some(_), Some(Value::Null)) => Ordering::Greater,
        (Some(_), Some(_)) => Ordering::Equal,
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

const SYSTEM_FILE_NAMES: [&str; 14] = [
    ".ds_store",
    ".spotlight-v100",
    ".trashes",
    ".fseventsd",
    ".temporaryitems",
    ".documentrevisions-v100",
    ".volumeicon.icns",
    "desktop.ini",
    "thumbs.db",
    "$recycle.bin",
    "system volume information",
    ".git",
    ".svn",
    ".hg",
];

fn is_system_name(name: &str) -> bool {
    name.starts_with("._")
        || SYSTEM_FILE_NAMES
            .iter()
            .any(|system| name.eq_ignore_ascii_case(system))
}

fn entry_name(entry: &FileEntry) -> &OsStr {
    entry
        .path
        .file_name()
        .unwrap_or_else(|| entry.path.as_os_str())
}

pub fn file_name(entry: &FileEntry) -> String {
    entry_name(entry).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, UNIX_EPOCH};

    use uuid::Uuid;

    use super::*;

    fn entry(name: &str, is_dir: bool, size: u64, hidden: bool, modified: u64) -> FileEntry {
        FileEntry {
            id: Uuid::new_v4(),
            path: PathBuf::from("root").join(name),
            size,
            modified: UNIX_EPOCH + Duration::from_secs(modified),
            hidden,
            is_dir,
            custom: HashMap::new(),
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

    #[test]
    fn navigation_tracks_back_forward_and_discards_forward_branches() {
        let mut state = BrowserState::new(PathBuf::from("one"));
        assert!(state.navigate(PathBuf::from("two")));
        assert!(state.navigate(PathBuf::from("three")));
        assert!(state.go_back());
        assert_eq!(state.path(), Path::new("two"));
        assert!(state.can_go_forward());

        assert!(state.navigate(PathBuf::from("branch")));
        assert_eq!(state.path(), Path::new("branch"));
        assert!(!state.can_go_forward());
    }

    #[test]
    fn navigation_history_jumps_caps_and_clears_like_the_retained_browser() {
        let mut state = BrowserState::new(PathBuf::from("root/0"));
        for index in 1..=55 {
            assert!(state.navigate(PathBuf::from(format!("root/{index}"))));
        }
        assert_eq!(state.back_history().len(), MAX_NAVIGATION_HISTORY);
        assert_eq!(state.back_history()[0], PathBuf::from("root/5"));
        assert_eq!(state.back_history()[49], PathBuf::from("root/54"));

        assert!(state.go_to_back_history(2));
        assert_eq!(state.path(), Path::new("root/52"));
        assert_eq!(
            state
                .forward_history()
                .iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>(),
            [
                PathBuf::from("root/53"),
                PathBuf::from("root/54"),
                PathBuf::from("root/55")
            ]
        );
        assert!(state.go_to_forward_history(2));
        assert_eq!(state.path(), Path::new("root/55"));
        assert!(!state.can_go_forward());

        state.clear_navigation_history();
        assert!(!state.can_go_back());
        assert!(!state.can_go_forward());
        assert!(!state.go_to_back_history(0));
        assert!(!state.go_to_forward_history(0));
    }

    #[test]
    fn folders_stay_first_while_sort_direction_changes() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![
            entry("small.txt", false, 1, false, 10),
            entry("folder", true, 0, false, 5),
            entry("large.txt", false, 100, false, 20),
        ]);
        state.set_sort(SortKey::Size);
        assert_eq!(file_name(&state.visible_entries()[0]), "folder");
        assert_eq!(file_name(&state.visible_entries()[1]), "small.txt");

        state.set_sort(SortKey::Size);
        assert_eq!(file_name(&state.visible_entries()[0]), "folder");
        assert_eq!(file_name(&state.visible_entries()[1]), "large.txt");
    }

    #[test]
    fn packages_sort_and_count_as_files_and_folder_links_as_folders() {
        let mut package = entry("Numbers.app", true, 0, false, 0);
        package.is_package = true;
        let mut folder_link = entry("projects-link", false, 30, false, 0);
        folder_link.is_symlink = true;
        folder_link.link_target_is_dir = true;
        let mut package_link = entry("Tool-link", false, 30, false, 0);
        package_link.is_symlink = true;
        package_link.link_target_is_dir = true;
        package_link.is_package = true;
        let mut file_link = entry("notes-link", false, 30, false, 0);
        file_link.is_symlink = true;

        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![
            entry("zeta.txt", false, 5, false, 0),
            package,
            folder_link,
            package_link,
            file_link,
            entry("alpha", true, 0, false, 0),
        ]);
        assert_eq!(
            visible_names(&state),
            [
                "alpha",
                "projects-link",
                "notes-link",
                "Numbers.app",
                "Tool-link",
                "zeta.txt"
            ]
        );
        assert_eq!(
            state.entry_stats(),
            EntryStats {
                folders: 2,
                files: 4,
                file_bytes: 65,
                hidden: 0,
            }
        );

        // The type filter matches smart folders' search semantics, where a
        // package is a directory.
        state.set_filter(EntryFilter::Folders);
        assert_eq!(
            visible_names(&state),
            ["alpha", "projects-link", "Numbers.app", "Tool-link"]
        );
        state.set_filter(EntryFilter::Files);
        assert_eq!(visible_names(&state), ["notes-link", "zeta.txt"]);
    }

    #[test]
    fn custom_columns_match_the_bounded_legacy_candidate_contract() {
        let mut entries = vec![entry("first.txt", false, 1, false, 0)];
        entries[0]
            .custom
            .insert("status".to_string(), Value::from("Done"));
        entries[0]
            .custom
            .insert("priority".to_string(), Value::from(3));
        entries[0]
            .custom
            .insert("tags".to_string(), serde_json::json!(["work"]));
        entries[0]
            .custom
            .insert("notes".to_string(), Value::from("short but not a column"));
        entries[0]
            .custom
            .insert("category".to_string(), Value::from("x".repeat(51)));
        for index in 1..=500 {
            entries.push(entry(&format!("file-{index}.txt"), false, index, false, 0));
        }
        entries[500]
            .custom
            .insert("type".to_string(), Value::from("Document"));

        assert_eq!(
            custom_columns(&entries),
            vec!["priority".to_string(), "status".to_string()]
        );
    }

    #[test]
    fn custom_sort_preserves_present_missing_numeric_and_direction_behavior() {
        let mut low = entry("low.txt", false, 1, false, 0);
        low.custom.insert("priority".to_string(), Value::from(1));
        let mut high = entry("high.txt", false, 1, false, 0);
        high.custom.insert("priority".to_string(), Value::from(10));
        let missing = entry("missing.txt", false, 1, false, 0);
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![missing, high, low]);

        state.set_sort(SortKey::custom("priority").unwrap());
        assert_eq!(
            state
                .visible_entries()
                .iter()
                .map(|entry| file_name(entry))
                .collect::<Vec<_>>(),
            ["low.txt", "high.txt", "missing.txt"]
        );
        state.set_sort(SortKey::custom("priority").unwrap());
        assert_eq!(
            state
                .visible_entries()
                .iter()
                .map(|entry| file_name(entry))
                .collect::<Vec<_>>(),
            ["missing.txt", "high.txt", "low.txt"]
        );
    }

    #[test]
    fn custom_columns_do_not_disappear_when_sorting_moves_missing_rows_first() {
        let mut tagged = entry("tagged.txt", false, 1, false, 0);
        tagged
            .custom
            .insert("status".to_string(), Value::from("Done"));
        let mut entries = vec![tagged];
        entries
            .extend((0..500).map(|index| entry(&format!("plain-{index}.txt"), false, 1, false, 0)));
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(entries);
        state.set_sort(SortKey::custom("status").unwrap());
        state.set_sort(SortKey::custom("status").unwrap());

        assert_eq!(file_name(&state.visible_entries()[0]), "plain-0.txt");
        assert_eq!(state.custom_columns(), vec!["status".to_string()]);
    }

    #[test]
    fn sort_keys_round_trip_as_legacy_compatible_strings() {
        assert_eq!(serde_json::to_string(&SortKey::Name).unwrap(), "\"name\"");
        let custom = SortKey::custom("status").unwrap();
        assert_eq!(serde_json::to_string(&custom).unwrap(), "\"status\"");
        assert_eq!(
            serde_json::from_str::<SortKey>("\"status\"").unwrap(),
            custom
        );
        assert!(serde_json::from_str::<SortKey>("\"\"").is_err());
    }

    #[test]
    fn sort_key_cycle_is_available_outside_the_list_header() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        assert_eq!(state.sort_key(), SortKey::Name);
        state.cycle_sort_key();
        assert_eq!(state.sort_key(), SortKey::Size);
        state.cycle_sort_key();
        assert_eq!(state.sort_key(), SortKey::Modified);
        state.cycle_sort_key();
        assert_eq!(state.sort_key(), SortKey::Name);
    }

    #[test]
    fn hidden_and_type_filters_reconcile_selection() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![
            entry("folder", true, 0, false, 0),
            entry("visible.txt", false, 1, false, 0),
            entry(".secret", false, 2, true, 0),
        ]);
        assert_eq!(state.visible_entries().len(), 2);

        state.toggle_hidden();
        assert_eq!(state.visible_entries().len(), 3);
        state.select(PathBuf::from("root/.secret"));
        state.cycle_filter();
        assert_eq!(state.filter(), EntryFilter::Folders);
        assert!(state.selected_path().is_none());
        assert_eq!(state.visible_entries().len(), 1);
    }

    #[test]
    fn system_files_are_hidden_independently_from_dotfiles() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![
            entry("visible.txt", false, 1, false, 0),
            entry(".notes", false, 1, true, 0),
            entry(".DS_Store", false, 1, false, 0),
            entry("desktop.ini", false, 1, false, 0),
            entry(".git", true, 0, true, 0),
        ]);

        assert_eq!(state.visible_entries().len(), 1);
        state.toggle_hidden();
        assert_eq!(state.visible_entries().len(), 2);
        assert!(
            state
                .visible_entries()
                .iter()
                .any(|entry| file_name(entry) == ".notes")
        );

        state.toggle_system_files();
        assert_eq!(state.visible_entries().len(), 5);
    }

    #[test]
    fn name_search_is_case_insensitive_and_reconciles_selection() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![
            entry("Alpha.txt", false, 0, false, 0),
            entry("beta.txt", false, 0, false, 0),
        ]);
        state.select(PathBuf::from("root/beta.txt"));

        state.push_search_text("ALP");
        assert_eq!(state.visible_entries().len(), 1);
        assert_eq!(file_name(&state.visible_entries()[0]), "Alpha.txt");
        assert!(state.selected_path().is_none());

        state.pop_search_character();
        state.clear_search();
        assert_eq!(state.visible_entries().len(), 2);
    }

    #[test]
    fn keyboard_selection_clamps_at_both_ends() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![
            entry("a", false, 0, false, 0),
            entry("b", false, 0, false, 0),
        ]);

        assert_eq!(state.select_previous(), Some(0));
        assert_eq!(state.select_previous(), Some(0));
        assert_eq!(state.select_next(), Some(1));
        assert_eq!(state.select_next(), Some(1));
    }

    #[test]
    fn keyboard_selection_supports_grid_sized_offsets_and_ranges() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(
            (0..10)
                .map(|index| entry(&format!("{index:02}"), false, 0, false, 0))
                .collect(),
        );

        assert_eq!(state.select_offset(4), Some(0));
        assert_eq!(state.select_offset(4), Some(4));
        assert_eq!(state.select_range_offset(4), Some(8));
        assert_eq!(state.selection_count(), 5);
        assert_eq!(state.select_offset(-4), Some(4));
        assert_eq!(state.select_offset(-8), Some(0));
        assert_eq!(state.select_offset(20), Some(9));
    }

    #[test]
    fn view_mode_changes_without_losing_selection_or_filters() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![entry("a", false, 0, false, 0)]);
        state.cycle_filter();
        state.cycle_filter();
        state.select(PathBuf::from("root/a"));

        state.set_view_mode(ViewMode::Grid);

        assert_eq!(state.view_mode(), ViewMode::Grid);
        assert_eq!(state.filter(), EntryFilter::Files);
        assert_eq!(state.selected_path(), Some(Path::new("root/a")));
    }

    #[test]
    fn refresh_keeps_selection_when_the_entry_still_exists() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![entry("a", false, 1, false, 1)]);
        state.select(PathBuf::from("root/a"));

        state.replace_entries(vec![
            entry("a", false, 2, false, 2),
            entry("b", false, 1, false, 1),
        ]);

        assert_eq!(state.selected_path(), Some(Path::new("root/a")));
        assert_eq!(state.selected_entry().unwrap().size, 2);
    }

    #[test]
    fn bulk_selection_survives_sort_and_refresh_but_drops_filtered_and_removed_paths() {
        let entries = (0..1_000)
            .map(|index| entry(&format!("file-{index:04}.txt"), false, index, false, 0))
            .collect::<Vec<_>>();
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(entries.clone());
        state.select_all();
        state.set_sort(SortKey::Size);
        state.set_sort(SortKey::Size);
        assert_eq!(state.selection_count(), 1_000);
        assert_eq!(state.selected_path(), Some(Path::new("root/file-0000.txt")));

        state.replace_entries(entries.into_iter().skip(1).collect::<Vec<_>>());
        assert_eq!(state.selection_count(), 999);
        assert_eq!(state.selected_path(), Some(Path::new("root/file-0001.txt")));
        assert_eq!(state.selection_anchor, state.selection_cursor);

        state.set_search_query("file-09".to_string());
        assert_eq!(state.selection_count(), 100);
        assert_eq!(state.selected_path(), Some(Path::new("root/file-0900.txt")));
        assert_eq!(state.selection_anchor, state.selection_cursor);
        assert!(state.selected_paths().iter().all(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("file-09")
        }));

        state.clear_search();
        assert_eq!(state.selection_count(), 100);
        state.set_filter(EntryFilter::Folders);
        assert_eq!(state.selection_count(), 0);
        assert!(state.selection_cursor.is_none());
        assert!(state.selection_anchor.is_none());
    }

    /// Mixed-case names with unpadded digit runs, one folder in ten, and only
    /// a thousand distinct sizes so size sorts exercise their name tiebreak.
    fn benchmark_entries(count: usize) -> Vec<FileEntry> {
        const PREFIXES: [&str; 4] = ["Report", "photo", "IMG", "notes"];
        (0..count)
            .rev()
            .map(|index| {
                let prefix = PREFIXES[index % PREFIXES.len()];
                let is_dir = index % 10 == 0;
                let name = if is_dir {
                    format!("{prefix} folder {index}")
                } else {
                    format!("{prefix}-{index}.txt")
                };
                entry(&name, is_dir, (index % 1_000) as u64, false, 0)
            })
            .collect()
    }

    /// Run with `cargo test -p explorie-gpui records_large_folder_interaction_baselines
    /// --release -- --ignored --nocapture`. Set EXPLORIE_BENCH_COUNTS to a
    /// comma-separated list to compare the same fixture sizes across revisions.
    #[test]
    #[ignore = "explicit large-folder interaction benchmark"]
    fn records_large_folder_interaction_baselines() {
        use std::hint::black_box;
        use std::time::Instant;

        let counts =
            std::env::var("EXPLORIE_BENCH_COUNTS").unwrap_or_else(|_| "10000,100000".to_string());
        for count in counts
            .split(',')
            .map(|count| count.parse::<usize>().unwrap())
        {
            assert!(count > 0);
            let entries = benchmark_entries(count);
            let mut state = BrowserState::new(PathBuf::from("root"));
            let refresh_entries = entries.clone();
            let started = Instant::now();
            state.replace_entries(entries);
            let listing = started.elapsed();

            let started = Instant::now();
            state.set_sort(SortKey::Size);
            let sort = started.elapsed();

            state.select_all();
            let started = Instant::now();
            state.set_sort(SortKey::Name);
            let selected_sort = started.elapsed();
            assert_eq!(state.selection_count(), count);

            let started = Instant::now();
            state.replace_entries(refresh_entries.clone());
            let selected_refresh = started.elapsed();
            assert_eq!(state.selection_count(), count);

            let started = Instant::now();
            state.set_search_query(".txt".to_string());
            let selected_filter = started.elapsed();
            assert_eq!(state.selection_count(), count - count.div_ceil(10));

            state.clear_search();
            state.clear_selection();
            let query = format!("photo-{}", count - 3);
            let started = Instant::now();
            state.set_search_query(query.clone());
            let narrow_search = started.elapsed();
            assert!(!state.visible_entries().is_empty());
            state.clear_search();

            let started = Instant::now();
            for character in query.chars() {
                state.push_search_text(&character.to_string());
            }
            let typed_search = started.elapsed();
            assert!(!state.visible_entries().is_empty());
            state.clear_search();

            // A watcher burst that touched a handful of files, applied the way
            // a full refresh does: every entry is replaced and re-sorted.
            let mut churned = refresh_entries;
            for changed in churned.iter_mut().step_by((count / 10).max(1)) {
                changed.size += 1;
            }
            // The same burst as a watcher patch: only those entries re-sort.
            let changes: Vec<_> = churned
                .iter()
                .step_by((count / 10).max(1))
                .map(|entry| {
                    let mut entry = entry.clone();
                    entry.size += 1;
                    (entry.path.clone(), Some(entry))
                })
                .collect();
            let started = Instant::now();
            state.replace_entries(churned);
            let churn_refresh = started.elapsed();

            let started = Instant::now();
            assert!(state.apply_entry_changes(changes));
            let churn_patch = started.elapsed();
            black_box(&state);
            eprintln!(
                "{count} entries | listing {listing:.2?} | sort {sort:.2?} | selected sort {selected_sort:.2?} | selected refresh {selected_refresh:.2?} | selected filter {selected_filter:.2?} | narrow search {narrow_search:.2?} | typed search {typed_search:.2?} | churn full refresh {churn_refresh:.2?} | churn patch {churn_patch:.2?}"
            );
        }
    }

    #[test]
    fn clearing_selection_cancels_saved_selection_before_listing_arrives() {
        let root = PathBuf::from("root");
        let mut state = BrowserState::new(root.clone());
        state.replace_entries(vec![entry("a", false, 0, false, 0)]);
        state.select(root.join("a"));
        state.navigate(root.join("child"));
        state.go_back();
        state.clear_selection();
        state.replace_entries(vec![entry("a", false, 0, false, 0)]);
        assert_eq!(state.selection_count(), 0);
    }

    #[test]
    fn selection_supports_replace_toggle_range_and_select_all() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![
            entry("a", false, 0, false, 0),
            entry("b", false, 0, false, 0),
            entry("c", false, 0, false, 0),
            entry("d", false, 0, false, 0),
        ]);

        state.select(PathBuf::from("root/a"));
        state.toggle_selection(PathBuf::from("root/c"));
        assert_eq!(state.selection_count(), 2);
        assert!(state.is_selected(Path::new("root/a")));
        assert!(state.is_selected(Path::new("root/c")));

        state.select_range_to(PathBuf::from("root/d"));
        assert_eq!(state.selection_count(), 2);
        assert!(state.is_selected(Path::new("root/c")));
        assert!(state.is_selected(Path::new("root/d")));

        state.select_all();
        assert_eq!(state.selection_count(), 4);
        state.replace_selection([
            PathBuf::from("root/b"),
            PathBuf::from("root/d"),
            PathBuf::from("root/missing"),
        ]);
        assert_eq!(state.selection_count(), 2);
        assert!(state.is_selected(Path::new("root/b")));
        assert!(state.is_selected(Path::new("root/d")));
        assert_eq!(state.selected_path(), Some(Path::new("root/b")));
        state.clear_selection();
        assert_eq!(state.selection_count(), 0);
    }

    #[test]
    fn drag_selection_snapshot_survives_toggle_replace_and_clear() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![
            entry("a", false, 0, false, 0),
            entry("b", false, 0, false, 0),
        ]);
        state.select_all();
        let snapshot = state.selection_snapshot();
        state.toggle_selection(PathBuf::from("root/a"));
        assert_eq!(state.selection_count(), 1);
        assert_eq!(snapshot.len(), 2);
        state.select(PathBuf::from("root/a"));
        state.clear_selection();
        assert_eq!(state.selection_count(), 0);
        assert_eq!(
            *snapshot,
            BTreeSet::from([PathBuf::from("root/a"), PathBuf::from("root/b")])
        );
    }

    #[test]
    fn range_keyboard_selection_keeps_anchor_and_refresh_reconciles_each_path() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![
            entry("a", false, 0, false, 0),
            entry("b", false, 0, false, 0),
            entry("c", false, 0, false, 0),
        ]);
        state.select(PathBuf::from("root/a"));

        assert_eq!(state.select_next_range(), Some(1));
        assert_eq!(state.select_next_range(), Some(2));
        assert_eq!(state.selection_count(), 3);
        assert_eq!(state.select_previous_range(), Some(1));
        assert_eq!(state.selection_count(), 2);

        state.replace_entries(vec![
            entry("b", false, 1, false, 1),
            entry("c", false, 1, false, 1),
        ]);
        assert_eq!(state.selection_count(), 1);
        assert!(state.is_selected(Path::new("root/b")));
    }

    #[test]
    fn type_to_select_matches_a_case_insensitive_prefix() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![
            entry("Alpha.txt", false, 0, false, 0),
            entry("beta.txt", false, 0, false, 0),
        ]);

        assert_eq!(state.select_prefix("BE"), Some(1));
        assert_eq!(state.selected_path(), Some(Path::new("root/beta.txt")));
        assert_eq!(state.select_prefix("missing"), None);
        assert_eq!(state.selected_path(), Some(Path::new("root/beta.txt")));
    }

    fn visible_names(state: &BrowserState) -> Vec<String> {
        state
            .visible_entries()
            .iter()
            .map(|entry| file_name(entry))
            .collect()
    }

    fn sorted_names(names: &[&str]) -> Vec<String> {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(
            names
                .iter()
                .map(|name| entry(name, false, 0, false, 0))
                .collect::<Vec<_>>(),
        );
        visible_names(&state)
    }

    #[test]
    fn name_sort_compares_digit_runs_numerically_and_case_insensitively() {
        assert_eq!(
            sorted_names(&["file10.txt", "File2.txt", "file1.txt", "file1a.txt"]),
            ["file1.txt", "file1a.txt", "File2.txt", "file10.txt"]
        );
        // Digits keep their place among other characters: "a 1" < "a-1" < "a1" < "a_1".
        assert_eq!(
            sorted_names(&["a_1", "a1", "a-1", "a 1"]),
            ["a 1", "a-1", "a1", "a_1"]
        );
        assert_eq!(
            sorted_names(&["v1.10.0", "v1.9.2", "v1.9.10", "v1.9"]),
            ["v1.9", "v1.9.2", "v1.9.10", "v1.10.0"]
        );
    }

    #[test]
    fn natural_ties_fall_back_to_the_exact_name() {
        // Equal numbers with different zero padding, and names that differ only
        // by case, still sort deterministically.
        assert_eq!(
            sorted_names(&["file7", "file007", "file07", "file0", "file00", "file"]),
            ["file", "file0", "file00", "file007", "file07", "file7"]
        );
        assert_eq!(
            sorted_names(&["readme", "README", "ReadMe"]),
            ["README", "ReadMe", "readme"]
        );
    }

    #[test]
    fn very_long_digit_runs_compare_by_length_without_overflow() {
        let huge = format!("n{}", "9".repeat(300));
        let larger = format!("n1{}", "0".repeat(300));
        let padded = format!("n{}5", "0".repeat(400));
        let names = [larger.as_str(), huge.as_str(), padded.as_str(), "n42"];
        assert_eq!(
            sorted_names(&names),
            [
                padded.clone(),
                "n42".to_string(),
                huge.clone(),
                larger.clone()
            ]
        );
        assert!(
            natural_sort_key(&"9".repeat(300)) < natural_sort_key(&format!("1{}", "0".repeat(300)))
        );
        assert!(natural_sort_key(&"9".repeat(254)) < natural_sort_key(&"1".repeat(255)));
    }

    #[test]
    fn natural_order_handles_unicode_names() {
        assert_eq!(
            sorted_names(&[
                "Ölfass 10",
                "ölfass 9",
                "Zebra",
                "apple",
                "Äpfel 2",
                "äpfel 10"
            ]),
            [
                "apple",
                "Zebra",
                "Äpfel 2",
                "äpfel 10",
                "ölfass 9",
                "Ölfass 10"
            ]
        );
        // Only ASCII digits form numbers; other scripts' digits sort as text.
        assert_eq!(
            sorted_names(&["track ١٠", "track 9", "track 10"]),
            ["track 9", "track 10", "track ١٠"]
        );
        assert_eq!(
            sorted_names(&["写真12", "写真3", "写真"]),
            ["写真", "写真3", "写真12"]
        );
    }

    #[test]
    fn natural_order_applies_to_descending_names_and_every_name_tiebreak() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![
            entry("item10", false, 5, false, 1),
            entry("item9", false, 5, false, 1),
            entry("Item100", false, 1, false, 2),
            entry("dir2", true, 0, false, 3),
            entry("dir10", true, 0, false, 3),
        ]);
        assert_eq!(
            visible_names(&state),
            ["dir2", "dir10", "item9", "item10", "Item100"]
        );
        state.set_sort(SortKey::Name);
        assert_eq!(
            visible_names(&state),
            ["dir10", "dir2", "Item100", "item10", "item9"]
        );

        state.set_sort(SortKey::Size);
        assert_eq!(
            visible_names(&state),
            ["dir2", "dir10", "Item100", "item9", "item10"]
        );
        state.set_sort(SortKey::Size);
        assert_eq!(
            visible_names(&state),
            ["dir2", "dir10", "item9", "item10", "Item100"],
            "descending sizes keep ascending natural name ties"
        );
        state.set_sort(SortKey::Modified);
        assert_eq!(
            visible_names(&state),
            ["dir2", "dir10", "item9", "item10", "Item100"]
        );
        state.set_sort(SortKey::custom("status").unwrap());
        assert_eq!(
            visible_names(&state),
            ["dir2", "dir10", "item9", "item10", "Item100"]
        );
    }

    #[test]
    fn typed_searches_narrow_previous_results_like_a_full_rescan() {
        let entries: Vec<_> = (0..200)
            .map(|index| {
                entry(
                    &format!("Report {index}.txt"),
                    index % 7 == 0,
                    (index % 3) as u64,
                    index % 5 == 0,
                    0,
                )
            })
            .chain([entry(".DS_Store", false, 0, false, 0)])
            .collect();
        let mut typed = BrowserState::new(PathBuf::from("root"));
        typed.replace_entries(entries.clone());
        typed.set_sort(SortKey::Size);
        typed.select(PathBuf::from("root/Report 12.txt"));
        for character in "REPORT 1".chars() {
            typed.push_search_text(&character.to_string());
        }

        let mut rescanned = BrowserState::new(PathBuf::from("root"));
        rescanned.replace_entries(entries);
        rescanned.set_sort(SortKey::Size);
        rescanned.set_search_query("REPORT 1".to_string());

        assert_eq!(visible_names(&typed), visible_names(&rescanned));
        assert!(!visible_names(&typed).is_empty());
        assert!(
            visible_names(&typed)
                .iter()
                .all(|name| name.to_lowercase().contains("report 1"))
        );
        assert_eq!(typed.selected_path(), Some(Path::new("root/Report 12.txt")));

        // Narrowing never reintroduces entries the other filters hide.
        typed.push_search_text("5");
        rescanned.set_search_query("REPORT 15".to_string());
        assert_eq!(visible_names(&typed), visible_names(&rescanned));
        assert!(!visible_names(&typed).contains(&"Report 15.txt".to_string()));
        assert_eq!(typed.selection_count(), 0);
        typed.pop_search_character();
        typed.pop_search_character();
        rescanned.set_search_query("REPORT ".to_string());
        assert_eq!(visible_names(&typed), visible_names(&rescanned));
        typed.clear_search();
        assert_eq!(typed.visible_entries().len(), 160);
    }

    #[test]
    fn progressive_results_are_sorted_again_when_the_query_changes() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![entry("b10", false, 0, false, 0)]);
        state.append_progressive_entries(vec![
            entry("b2", false, 0, false, 0),
            entry("a1", false, 0, false, 0),
        ]);
        assert_eq!(visible_names(&state), ["b10", "b2", "a1"]);
        state.push_search_text("b");
        assert_eq!(visible_names(&state), ["b2", "b10"]);
    }

    #[test]
    fn listing_totals_track_replaced_and_appended_entries() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![
            entry("folder", true, 4_096, false, 0),
            entry("a.txt", false, 10, false, 0),
            entry(".hidden", false, 5, true, 0),
        ]);
        assert_eq!(
            state.entry_stats(),
            EntryStats {
                folders: 1,
                files: 2,
                file_bytes: 15,
                hidden: 1,
            }
        );
        state.append_progressive_entries(vec![entry("b.txt", false, 7, false, 0)]);
        assert_eq!(state.entry_stats().files, 3);
        assert_eq!(state.entry_stats().file_bytes, 22);
        state.navigate(PathBuf::from("elsewhere"));
        assert_eq!(state.entry_stats(), EntryStats::default());
    }

    #[test]
    fn cached_selected_entry_and_custom_columns_follow_changes() {
        let mut tagged = entry("tagged.txt", false, 1, false, 0);
        tagged
            .custom
            .insert("status".to_string(), Value::from("Done"));
        let mut hidden = entry(".secret", false, 1, true, 0);
        hidden.custom.insert("priority".to_string(), Value::from(2));
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![tagged, hidden, entry("plain.txt", false, 3, false, 0)]);
        assert_eq!(state.custom_columns(), ["status"]);
        state.toggle_hidden();
        assert_eq!(state.custom_columns(), ["priority", "status"]);
        state.set_search_query("plain".to_string());
        assert!(state.custom_columns().is_empty());
        state.clear_search();

        assert!(state.selected_entry().is_none());
        state.select(PathBuf::from("root/plain.txt"));
        assert_eq!(state.selected_entry().unwrap().size, 3);
        state.select_next();
        assert_eq!(file_name(state.selected_entry().unwrap()), "tagged.txt");
        state.toggle_selection(PathBuf::from("root/.secret"));
        assert_eq!(file_name(state.selected_entry().unwrap()), ".secret");
        state.select_all();
        assert_eq!(file_name(state.selected_entry().unwrap()), ".secret");
        state.set_sort(SortKey::Size);
        state.set_sort(SortKey::Size);
        assert_eq!(file_name(state.selected_entry().unwrap()), ".secret");
        state.replace_entries(vec![entry("plain.txt", false, 9, false, 0)]);
        assert_eq!(state.selected_entry().unwrap().size, 9);
        state.clear_selection();
        assert!(state.selected_entry().is_none());
    }

    fn change(name: &str, entry: Option<FileEntry>) -> (PathBuf, Option<FileEntry>) {
        (PathBuf::from("root").join(name), entry)
    }

    #[test]
    fn entry_patches_create_delete_modify_and_rename_in_sorted_order() {
        let mut state = BrowserState::new(PathBuf::from("root"));
        state.replace_entries(vec![
            entry("file1", false, 10, false, 0),
            entry("file3", false, 30, false, 0),
            entry("file10", false, 5, false, 0),
            entry("dir", true, 0, false, 0),
        ]);
        state.select(PathBuf::from("root/file3"));

        assert!(state.apply_entry_changes(vec![
            change("file2", Some(entry("file2", false, 20, false, 0))),
            change("file10", None),
        ]));
        assert_eq!(visible_names(&state), ["dir", "file1", "file2", "file3"]);
        assert_eq!(state.selected_path(), Some(Path::new("root/file3")));
        assert_eq!(state.selected_entry().unwrap().size, 30);

        state.set_sort(SortKey::Size);
        assert!(state.apply_entry_changes(vec![change(
            "file3",
            Some(entry("file3", false, 1, false, 0))
        )]));
        assert_eq!(visible_names(&state), ["dir", "file3", "file1", "file2"]);
        assert_eq!(state.selected_path(), Some(Path::new("root/file3")));
        assert_eq!(state.selected_entry().unwrap().size, 1);
        assert_eq!(
            state.entry_stats(),
            EntryStats {
                folders: 1,
                files: 3,
                file_bytes: 31,
                hidden: 0,
            }
        );

        // A rename arrives as the old path gone and the new path present.
        assert!(state.apply_entry_changes(vec![
            change("file3", None),
            change("renamed", Some(entry("renamed", false, 1, false, 0))),
        ]));
        assert_eq!(visible_names(&state), ["dir", "renamed", "file1", "file2"]);
        assert_eq!(state.selection_count(), 0);

        // Hidden entries are listed and counted but stay filtered out.
        assert!(state.apply_entry_changes(vec![change(
            ".cache",
            Some(entry(".cache", false, 4, true, 0))
        )]));
        assert_eq!(visible_names(&state), ["dir", "renamed", "file1", "file2"]);
        assert_eq!(state.entries().len(), 5);
        assert_eq!(state.entry_stats().hidden, 1);

        // Re-reading unchanged or unknown paths is not a change.
        assert!(!state.apply_entry_changes(vec![
            change("file2", Some(entry("file2", false, 20, false, 0))),
            change("missing", None),
        ]));

        // A Finder tag added on disk changes the row even if nothing else did.
        let mut tagged = entry("file2", false, 20, false, 0);
        tagged.tags = vec![explorie_core::FinderTag {
            name: "Urgent".to_string(),
            color: 6,
        }];
        assert!(state.apply_entry_changes(vec![change("file2", Some(tagged))]));
        let file2 = state
            .visible_entries()
            .iter()
            .find(|entry| entry.path == Path::new("root/file2"))
            .unwrap();
        assert_eq!(file2.tags.len(), 1);
    }

    #[test]
    fn entry_patches_match_a_full_refresh_under_every_sort() {
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move |bound: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % bound
        };
        let make = |index: u64, size: u64, modified: u64| {
            entry(
                &format!("Item{index}"),
                index.is_multiple_of(9),
                size,
                index.is_multiple_of(11),
                modified,
            )
        };
        for (sort_key, descending, show_hidden, query) in [
            (SortKey::Name, false, true, ""),
            (SortKey::Name, true, false, ""),
            (SortKey::Size, false, true, "item1"),
            (SortKey::Size, true, false, ""),
            (SortKey::Modified, true, false, "7"),
        ] {
            let configure = |state: &mut BrowserState| {
                state.set_sort(sort_key.clone());
                if descending {
                    state.set_sort(sort_key.clone());
                }
                if show_hidden {
                    state.toggle_hidden();
                }
                state.set_search_query(query.to_string());
            };
            let mut model: HashMap<PathBuf, FileEntry> = (0..150)
                .map(|index| make(index, index % 7, index % 5))
                .map(|entry| (entry.path.clone(), entry))
                .collect();
            let mut patched = BrowserState::new(PathBuf::from("root"));
            patched.replace_entries(model.values().cloned().collect::<Vec<_>>());
            configure(&mut patched);
            patched.select_all();
            for _ in 0..40 {
                let changes: Vec<_> = (0..1 + next(12))
                    .map(|_| {
                        let index = next(220);
                        let path = PathBuf::from("root").join(format!("Item{index}"));
                        let fresh = (next(4) != 0).then(|| make(index, next(7), next(5)));
                        match &fresh {
                            Some(entry) => model.insert(path.clone(), entry.clone()),
                            None => model.remove(&path),
                        };
                        (path, fresh)
                    })
                    .collect();
                // Later duplicates win, like the model.
                let mut deduped: Vec<(PathBuf, Option<FileEntry>)> = Vec::new();
                for (path, fresh) in changes {
                    deduped.retain(|(existing, _)| existing != &path);
                    deduped.push((path, fresh));
                }
                patched.apply_entry_changes(deduped);

                let mut refreshed = BrowserState::new(PathBuf::from("root"));
                refreshed.replace_entries(model.values().cloned().collect::<Vec<_>>());
                configure(&mut refreshed);
                assert_eq!(visible_names(&patched), visible_names(&refreshed));
                assert_eq!(patched.entry_stats(), refreshed.entry_stats());
                // Selected entries that vanished or stopped matching drop out.
                assert!(
                    patched
                        .selected_paths()
                        .iter()
                        .all(|path| model.contains_key(path))
                );
                assert!(patched.selection_count() <= patched.visible_entries().len());
            }
        }
    }
}
