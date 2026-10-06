//! `DirectoryWindow` behavior for listing.

use crate::*;

/// The main listing: whether the current folder has loaded, the listing job
/// and its generation, scroll position handling, listing warnings, and
/// type-to-select input.
pub(crate) struct ListingUi {
    pub(crate) state: ListingState,
    pub(crate) task: Option<Task<()>>,
    pub(crate) generation: u64,
    pub(crate) reset_scroll_on_result: bool,
    pub(crate) pending_scroll_restore: Option<usize>,
    pub(crate) scroll_handle: UniformListScrollHandle,
    /// Warnings from the latest listing of the folder they belong to.
    pub(crate) warning: Option<(PathBuf, String)>,
    /// Listing warnings already announced with a toast, so refreshes that
    /// meet the same problem only keep it in the status line.
    pub(crate) announced_warnings: VecDeque<(PathBuf, String)>,
    pub(crate) type_select_value: String,
    pub(crate) type_select_at: Option<Instant>,
}

/// Free and total space of the volume holding the current folder.
#[derive(Default)]
pub(crate) struct DiskInfoState {
    pub(crate) info: Option<DiskInfo>,
    pub(crate) path: Option<PathBuf>,
    pub(crate) task: Option<Task<()>>,
    pub(crate) generation: u64,
}

/// Column view: the columns shown and their listings in flight, each
/// column's scroll position and the strip's, and the selection inside the
/// columns.
pub(crate) struct ColumnViewUi {
    pub(crate) columns: ColumnState,
    pub(crate) tasks: Vec<Task<()>>,
    pub(crate) generation: u64,
    pub(crate) scroll_handles: Vec<UniformListScrollHandle>,
    pub(crate) strip_scroll: ScrollHandle,
    pub(crate) scroll_to_leaf_attempts: u8,
    /// Width the trailing preview gains so the strip can scroll to a column
    /// boundary (see [`leaf_alignment`](crate::column::leaf_alignment)).
    pub(crate) preview_fill: f32,
    pub(crate) selection: BTreeSet<PathBuf>,
    pub(crate) pending_selection: Option<ColumnSelectionTarget>,
}

impl DirectoryWindow {
    pub fn start_listing(&mut self, cx: &mut Context<Self>) {
        self.start_disk_info(cx);
        self.clear_plugin_context(cx);
        if let Some(criteria) = self.listing_search_criteria() {
            self.start_smart_search(criteria, cx);
        } else if self.browser.view_mode() == ViewMode::Column {
            self.start_column_listings(true, cx);
        } else {
            self.start_listing_with_scroll_reset(false, cx);
        }
    }

    pub(crate) fn start_listing_with_scroll_reset(
        &mut self,
        reset_scroll: bool,
        cx: &mut Context<Self>,
    ) {
        self.cancel_watcher_patch();
        self.listing.generation = self.listing.generation.wrapping_add(1);
        self.listing.reset_scroll_on_result = reset_scroll;
        let generation = self.listing.generation;
        let services = self.services.clone();
        let request = ListRequest {
            path: self.browser.path().to_path_buf(),
            calc_dir_size: self.calculate_folder_sizes,
        };
        self.listing.state = ListingState::Loading;
        self.status_message = None;

        self.listing.task = Some(cx.spawn(async move |this, cx| {
            let event = list_directory_task(services, generation, request).await;
            let _ = this.update(cx, |view, cx| {
                view.apply_event(event, cx);
                cx.notify();
            });
        }));
    }

    pub(crate) fn apply_event(&mut self, event: DirectoryEvent, cx: &mut Context<Self>) {
        match event {
            DirectoryEvent::Listed {
                generation,
                request,
                entries,
                warning,
            } if generation == self.listing.generation && request.path == self.browser.path() => {
                self.browser.replace_entries(entries);
                self.listing.state = ListingState::Ready;
                self.apply_listing_warning(request.path, warning, cx);
                self.start_plugin_scan(true, cx);
                if self.listing.reset_scroll_on_result {
                    self.listing
                        .scroll_handle
                        .scroll_to_item_strict(0, ScrollStrategy::Top);
                    self.listing.pending_scroll_restore = None;
                } else if let Some(index) = self.listing.pending_scroll_restore.take() {
                    self.listing
                        .scroll_handle
                        .scroll_to_item_strict(index, ScrollStrategy::Top);
                }
                self.listing.reset_scroll_on_result = false;
                self.finish_pending_preview_refresh(cx);
            }
            DirectoryEvent::Failed {
                generation,
                request,
                error,
            } if generation == self.listing.generation && request.path == self.browser.path() => {
                self.record_error("Directory listing failed", error.to_string());
                self.listing.state = ListingState::Failed(error.to_string());
            }
            DirectoryEvent::Listed { .. } | DirectoryEvent::Failed { .. } => {}
        }
        self.finish_watcher_refresh(cx);
    }

    /// Record the warnings of a fresh listing of `path`. Each distinct
    /// warning is announced once with a toast; refreshes that meet the same
    /// problem again only keep it in the status line.
    pub(crate) fn apply_listing_warning(
        &mut self,
        path: PathBuf,
        warning: Option<String>,
        cx: &mut Context<Self>,
    ) {
        const ANNOUNCED_LIMIT: usize = 32;
        let Some(warning) = warning else {
            // A clean listing ends the problem; a recurrence is news again.
            self.listing
                .announced_warnings
                .retain(|(announced, _)| announced != &path);
            if self
                .listing
                .warning
                .as_ref()
                .is_some_and(|(warned, _)| warned == &path)
            {
                self.listing.warning = None;
            }
            return;
        };
        let key = (path, warning);
        if !self.listing.announced_warnings.contains(&key) {
            if self.listing.announced_warnings.len() == ANNOUNCED_LIMIT {
                self.listing.announced_warnings.pop_front();
            }
            self.listing.announced_warnings.push_back(key.clone());
            self.show_toast(key.1.clone(), ToastKind::Warning, cx);
        }
        self.listing.warning = Some(key);
    }

    /// The current folder's listing warning, for the status line.
    pub(crate) fn current_listing_warning(&self) -> Option<&str> {
        self.listing
            .warning
            .as_ref()
            .filter(|(path, _)| path == self.browser.path() && !self.listing_shows_search_results())
            .map(|(_, warning)| warning.as_str())
    }

    pub fn start_system_locations(&mut self, cx: &mut Context<Self>) {
        self.system.locations_error = None;
        let task = self.services.listing.system_locations();
        self.system.locations_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(locations) => view.system.locations = Some(locations),
                    Err(error) => {
                        view.record_error("System locations failed", error.to_string());
                        view.system.locations_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn start_disk_info(&mut self, cx: &mut Context<Self>) {
        let path = self.browser.path().to_path_buf();
        self.disk.generation = self.disk.generation.wrapping_add(1);
        let generation = self.disk.generation;
        self.disk.path = Some(path.clone());
        self.disk.info = None;
        let task = self.services.listing.disk_info(path.clone());
        self.disk.task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                if generation == view.disk.generation && view.browser.path() == path {
                    view.disk.info = result.ok();
                    cx.notify();
                }
            });
        }));
    }

    pub(crate) fn start_column_listings(&mut self, reset_stack: bool, cx: &mut Context<Self>) {
        self.cancel_watcher_patch();
        self.column_view.generation = self.column_view.generation.wrapping_add(1);
        let generation = self.column_view.generation;
        if reset_stack {
            let retained = self.column_view.columns.reset(self.browser.path());
            self.column_view.scroll_handles.truncate(retained);
            self.column_view.selection.clear();
        } else {
            self.column_view.columns.begin_refresh();
        }

        let paths = self.column_view.columns.paths();
        self.column_view
            .scroll_handles
            .resize_with(paths.len(), UniformListScrollHandle::new);
        if reset_stack && !paths.is_empty() {
            self.column_view
                .strip_scroll
                .scroll_to_item(paths.len() - 1);
            self.column_view.scroll_to_leaf_attempts = 3;
        }
        self.column_view.tasks.clear();
        self.column_view.tasks.reserve(paths.len());

        for path in paths {
            let services = self.services.clone();
            let request = ListRequest {
                path,
                calc_dir_size: self.calculate_folder_sizes,
            };
            self.column_view.tasks.push(cx.spawn(async move |this, cx| {
                let event = list_directory_task(services, generation, request).await;
                let _ = this.update(cx, |view, cx| {
                    view.apply_column_event(event, cx);
                    cx.notify();
                });
            }));
        }
    }

    pub(crate) fn apply_column_event(&mut self, event: DirectoryEvent, cx: &mut Context<Self>) {
        match event {
            DirectoryEvent::Listed {
                generation,
                request,
                entries,
                warning,
            } if generation == self.column_view.generation => {
                let is_active = request.path == self.browser.path();
                // Share the entries between the column and the browser listing.
                let entries: Vec<Arc<FileEntry>> = entries.into_iter().map(Arc::new).collect();
                if self
                    .column_view
                    .columns
                    .apply_listed(&request.path, entries.clone())
                    && is_active
                {
                    self.browser.replace_entries(entries);
                    self.listing.state = ListingState::Ready;
                    self.apply_listing_warning(request.path.clone(), warning, cx);
                    if let Some(target) = self.column_view.pending_selection.take() {
                        let requested = match target {
                            ColumnSelectionTarget::First => self
                                .browser
                                .visible_entries()
                                .first()
                                .map(|entry| entry.path.clone()),
                            ColumnSelectionTarget::Path(path) => self
                                .browser
                                .visible_entries()
                                .iter()
                                .any(|entry| entry.path == path)
                                .then_some(path)
                                .or_else(|| {
                                    self.browser
                                        .visible_entries()
                                        .first()
                                        .map(|entry| entry.path.clone())
                                }),
                        };
                        if let Some(path) = requested {
                            let index = self
                                .browser
                                .visible_entries()
                                .iter()
                                .position(|entry| entry.path == path)
                                .unwrap_or(0);
                            self.browser.select(path.clone());
                            self.set_column_selection(path);
                            self.reveal_selected(index);
                        }
                    }
                    self.finish_pending_preview_refresh(cx);
                    self.start_plugin_scan(true, cx);
                }
            }
            DirectoryEvent::Failed {
                generation,
                request,
                error,
            } if generation == self.column_view.generation => {
                let is_active = request.path == self.browser.path();
                let message = error.to_string();
                self.record_error("Column listing failed", &message);
                if self
                    .column_view
                    .columns
                    .apply_failed(&request.path, message.clone())
                    && is_active
                {
                    self.column_view.pending_selection = None;
                    self.listing.state = ListingState::Failed(message);
                }
            }
            DirectoryEvent::Listed { .. } | DirectoryEvent::Failed { .. } => {}
        }
        self.finish_watcher_refresh(cx);
    }

    pub(crate) fn listing_in_flight(&self) -> bool {
        if self.listing_shows_search_results() {
            self.search.task.is_some()
        } else if self.browser.view_mode() == ViewMode::Column {
            self.column_view
                .columns
                .columns()
                .iter()
                .any(|column| column.loading())
        } else {
            matches!(self.listing.state, ListingState::Loading)
        }
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(criteria) = self.listing_search_criteria() {
            self.start_smart_search(criteria, cx);
        } else if self.browser.view_mode() == ViewMode::Column {
            self.start_column_listings(false, cx);
        } else {
            self.start_listing(cx);
        }
    }

    pub(crate) fn set_view_mode(&mut self, view_mode: ViewMode, cx: &mut Context<Self>) {
        if self.browser.view_mode() == view_mode {
            return;
        }
        self.pointer.selection_marquee = None;
        if view_mode == ViewMode::Column {
            // Column view can't show a flat list of subfolder results.
            self.end_subfolder_search(false, cx);
        }
        self.browser.set_view_mode(view_mode);
        if self.search.subfolders.is_some() {
            self.browser.show_search_results(true);
        }
        self.settings.view.view_mode = view_mode;
        self.persist_settings();
        self.persist_session();
        match view_mode {
            ViewMode::List | ViewMode::Grid => {
                self.column_view.generation = self.column_view.generation.wrapping_add(1);
                self.column_view.tasks.clear();
                self.start_listing(cx);
            }
            ViewMode::Column => {
                self.listing.generation = self.listing.generation.wrapping_add(1);
                self.listing.task = None;
                self.start_column_listings(true, cx);
            }
        }
        self.start_watching(cx);
        self.listing
            .scroll_handle
            .scroll_to_item_strict(0, ScrollStrategy::Top);
        cx.notify();
    }

    pub(crate) fn toggle_folder_sizes(&mut self, cx: &mut Context<Self>) {
        self.calculate_folder_sizes = !self.calculate_folder_sizes;
        self.settings.view.show_folder_sizes = self.calculate_folder_sizes;
        self.persist_settings();
        self.refresh(cx);
        cx.notify();
    }

    pub(crate) fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.browser.toggle_hidden();
        self.settings.view.show_hidden = self.browser.show_hidden();
        self.apply_global_browser_preferences();
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn cycle_filter(&mut self, cx: &mut Context<Self>) {
        self.browser.cycle_filter();
        self.settings.view.filter_mode = self.browser.filter();
        self.apply_global_browser_preferences();
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn set_filter(&mut self, filter: EntryFilter, cx: &mut Context<Self>) {
        self.browser.set_filter(filter);
        self.settings.view.filter_mode = filter;
        self.apply_global_browser_preferences();
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn set_sort(&mut self, key: SortKey, cx: &mut Context<Self>) {
        self.browser.set_sort(key);
        self.settings.view.sort_key = self.browser.sort_key();
        self.settings.view.sort_direction = self.browser.sort_direction();
        self.persist_settings();
        self.persist_session();
        cx.notify();
    }
}
