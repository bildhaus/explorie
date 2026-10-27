//! `DirectoryWindow` behavior for search.

use crate::*;

/// How long typing must pause before a subfolder search starts, so each
/// keystroke doesn't restart the crawl.
pub(crate) const SUBFOLDER_SEARCH_DELAY: Duration = Duration::from_millis(250);

/// The longest folder name the search scope bar shows in full.
const SCOPE_FOLDER_LABEL_CHARS: usize = 24;

/// Where the search field looks. "This Folder" filters the folder's own
/// listing as you type; "Subfolders" is Finder's folder scope: the folder
/// and everything under it, found by the same Spotlight + crawler search
/// smart folders use.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SearchScope {
    #[default]
    ThisFolder,
    Subfolders,
}

/// A search of a folder's subfolders whose results the listing shows in
/// place of the folder's own entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SubfolderSearch {
    pub(crate) root: PathBuf,
    pub(crate) query: String,
}

/// Search in this window: whether the search field is active, its scope,
/// and the smart-folder or subfolder search running with its progress.
#[derive(Default)]
pub(crate) struct SearchUi {
    pub(crate) active: bool,
    pub(crate) task: Option<Task<()>>,
    pub(crate) generation: u64,
    pub(crate) request_id: Option<String>,
    pub(crate) progress: Option<SearchProgressEvent>,
    pub(crate) scope: SearchScope,
    /// The subfolder search the listing shows, if any.
    pub(crate) subfolders: Option<SubfolderSearch>,
    /// A subfolder search waiting for typing to pause.
    pub(crate) debounce: Option<Task<()>>,
}

impl DirectoryWindow {
    /// The search whose results the listing shows instead of a folder's
    /// entries: the active smart folder's, or a subfolder search's.
    pub(crate) fn listing_search_criteria(&self) -> Option<SearchCriteria> {
        if let Some(folder) = self.browser.active_smart_folder() {
            return Some(folder.criteria().clone());
        }
        let search = self.search.subfolders.as_ref()?;
        Some(SearchCriteria {
            name_pattern: Some(search.query.clone()),
            type_filter: match self.browser.filter() {
                EntryFilter::All => SearchType::All,
                EntryFilter::Files => SearchType::Files,
                EntryFilter::Folders => SearchType::Folders,
            },
            search_paths: vec![search.root.clone()],
            recursive: true,
            ..SearchCriteria::default()
        })
    }

    /// Whether the listing holds search results rather than a folder.
    pub(crate) fn listing_shows_search_results(&self) -> bool {
        self.browser.active_smart_folder().is_some() || self.search.subfolders.is_some()
    }

    /// Whether keyboard focus is in the toolbar search field.
    pub(crate) fn search_field_focused(&self, window: &Window, cx: &App) -> bool {
        self.text_input.target == Some(TextInputTarget::Search)
            && self
                .text_input
                .entity
                .as_ref()
                .is_some_and(|input| input.focus_handle(cx).is_focused(window))
    }

    /// React to the search field's query changing: keep the selection to
    /// the items still shown, and search subfolders when that is the scope.
    pub(crate) fn search_query_did_change(&mut self, cx: &mut Context<Self>) {
        self.drop_hidden_selection(cx);
        self.update_subfolder_search(cx);
    }

    /// The browser's own selection follows its filtered rows; Column view's
    /// selection must too, so opening or acting on the selection never
    /// reaches an item the search hides.
    pub(crate) fn drop_hidden_selection(&mut self, cx: &mut Context<Self>) {
        if self.column_view.selection.is_empty() {
            return;
        }
        let shown: BTreeSet<PathBuf> = self
            .column_view
            .selection
            .iter()
            .filter(|path| {
                self.column_view
                    .columns
                    .columns()
                    .iter()
                    .any(|column| column.visible_entry(&self.browser, path).is_some())
            })
            .cloned()
            .collect();
        if shown.len() != self.column_view.selection.len() {
            self.column_view.selection = shown;
            self.sync_pinned_preview(cx);
        }
    }

    /// Start, restart (after a pause in typing) or end the subfolder search
    /// to match the scope and query.
    pub(crate) fn update_subfolder_search(&mut self, cx: &mut Context<Self>) {
        let query = self.browser.search_query().trim().to_string();
        if self.search.scope != SearchScope::Subfolders
            || query.is_empty()
            || self.browser.active_smart_folder().is_some()
        {
            self.end_subfolder_search(true, cx);
            return;
        }
        if self.search.subfolders.as_ref().is_some_and(|search| {
            search.query == query && search.root.as_path() == self.browser.path()
        }) {
            self.search.debounce = None;
            return;
        }
        let executor = cx.background_executor().clone();
        self.search.debounce = Some(cx.spawn(async move |this, cx| {
            executor.timer(SUBFOLDER_SEARCH_DELAY).await;
            let _ = this.update(cx, |view, cx| view.start_subfolder_search(cx));
        }));
    }

    /// Search the current folder's subfolders for the query now, showing the
    /// results in the listing (as a list, if the folder uses Column view).
    pub(crate) fn start_subfolder_search(&mut self, cx: &mut Context<Self>) {
        self.search.debounce = None;
        let query = self.browser.search_query().trim().to_string();
        if self.search.scope != SearchScope::Subfolders
            || query.is_empty()
            || self.browser.active_smart_folder().is_some()
        {
            return;
        }
        self.search.subfolders = Some(SubfolderSearch {
            root: self.browser.path().to_path_buf(),
            query,
        });
        self.browser.show_search_results(true);
        self.listing.generation = self.listing.generation.wrapping_add(1);
        self.listing.task = None;
        self.column_view.generation = self.column_view.generation.wrapping_add(1);
        self.column_view.tasks.clear();
        self.column_view.selection.clear();
        if let Some(criteria) = self.listing_search_criteria() {
            self.start_smart_search(criteria, cx);
        }
        self.start_watching(cx);
        self.listing
            .scroll_handle
            .scroll_to_item_strict(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Stop showing subfolder results; with `relist`, show the folder's own
    /// entries again (a folder change lists the new folder itself).
    pub(crate) fn end_subfolder_search(&mut self, relist: bool, cx: &mut Context<Self>) {
        self.search.debounce = None;
        if self.search.subfolders.take().is_none() {
            return;
        }
        self.services.search.cancel();
        self.search.generation = self.search.generation.wrapping_add(1);
        self.search.task = None;
        self.search.request_id = None;
        self.search.progress = None;
        self.browser.show_search_results(false);
        if relist {
            self.start_listing(cx);
            self.start_watching(cx);
        }
        cx.notify();
    }

    pub(crate) fn set_search_scope(&mut self, scope: SearchScope, cx: &mut Context<Self>) {
        if self.search.scope == scope {
            return;
        }
        self.search.scope = scope;
        match scope {
            SearchScope::Subfolders => self.start_subfolder_search(cx),
            SearchScope::ThisFolder => self.end_subfolder_search(true, cx),
        }
        // Keep typing in the field after picking a scope.
        if self.search.active {
            self.text_input.focus_pending = true;
        }
        cx.notify();
    }

    /// Clear the search and give the keyboard back to the file list, as
    /// Escape in the field and its clear button do.
    pub(crate) fn clear_search_and_focus_list(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browser.clear_search();
        self.search.active = false;
        self.deactivate_native_text_input();
        self.end_subfolder_search(true, cx);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Like Finder, leaving a folder ends its search.
    pub(crate) fn reset_search_for_navigation(&mut self) {
        self.browser.clear_search();
        self.search.active = false;
        if self.text_input.target == Some(TextInputTarget::Search) {
            self.deactivate_native_text_input();
        }
    }

    pub(crate) fn start_smart_search(&mut self, criteria: SearchCriteria, cx: &mut Context<Self>) {
        self.cancel_watcher_patch();
        self.services.search.cancel();
        self.search.generation = self.search.generation.wrapping_add(1);
        let generation = self.search.generation;
        let request_id = format!(
            "{}-{generation}",
            self.window_lifetime
                .as_ref()
                .map_or("window", |lifetime| lifetime.id.as_str())
        );
        let task = self
            .services
            .search
            .search_with_progress(criteria, request_id.clone());
        self.search.request_id = Some(request_id);
        self.search.progress = None;
        self.browser.replace_entries(Vec::<FileEntry>::new());
        self.listing.state = ListingState::Loading;
        self.status_message = Some("Indexing local files… • Esc to stop".to_string());
        self.search.task = Some(cx.spawn(async move |this, cx| {
            let event = match task.await {
                Ok(result) => SearchEvent::Completed { generation, result },
                Err(error) => SearchEvent::Failed { generation, error },
            };
            let _ = this.update(cx, |view, cx| {
                view.apply_search_event(event, cx);
                cx.notify();
            });
        }));
    }

    pub(crate) fn apply_search_event(&mut self, event: SearchEvent, cx: &mut Context<Self>) {
        match event {
            SearchEvent::Completed { generation, result }
                if generation == self.search.generation && self.listing_shows_search_results() =>
            {
                self.search.task = None;
                self.search.request_id = None;
                let root = self
                    .search
                    .subfolders
                    .as_ref()
                    .filter(|_| self.browser.active_smart_folder().is_none())
                    .map(|search| search.root.as_path());
                let status = search_result_status(&result, root);
                self.browser.replace_entries(result.entries);
                self.listing.state = ListingState::Ready;
                self.status_message = Some(status);
                self.search.progress = None;
                self.finish_pending_preview_refresh(cx);
            }
            SearchEvent::Failed { generation, error }
                if generation == self.search.generation && self.listing_shows_search_results() =>
            {
                self.search.task = None;
                self.search.request_id = None;
                self.search.progress = None;
                if error.code == ErrorCode::Cancelled {
                    self.listing.state = ListingState::Ready;
                    self.status_message = Some(format!(
                        "Search stopped • {} partial result(s)",
                        self.browser.visible_entries().len()
                    ));
                    self.watcher.refresh_pending = false;
                    return;
                }
                self.record_error("Search failed", error.to_string());
                self.listing.state = ListingState::Failed(error.to_string());
                self.status_message = Some(format!("Search failed: {error}"));
            }
            SearchEvent::Completed { .. } | SearchEvent::Failed { .. } => {}
        }
        self.finish_watcher_refresh(cx);
    }

    pub(crate) fn apply_search_progress(
        &mut self,
        progress: SearchProgressEvent,
        cx: &mut Context<Self>,
    ) {
        if self.search.request_id.as_deref() != Some(progress.request_id.as_str())
            || !self.listing_shows_search_results()
        {
            return;
        }
        if !progress.entries.is_empty() {
            self.browser
                .append_progressive_entries(progress.entries.clone());
            self.listing.state = ListingState::Ready;
        }
        let path = progress
            .current_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| progress.current_path.display().to_string());
        self.status_message = Some(match progress.phase.as_str() {
            "spotlight" => format!(
                "Searching Spotlight • {} result(s) • Esc to stop",
                progress.matched_entries
            ),
            "indexing" => format!(
                "Indexing {path} • {} paths • Esc to stop",
                progress.indexed_entries
            ),
            "content" => format!(
                "Reading content in {path} • {} paths • Esc to stop",
                progress.indexed_entries
            ),
            _ => format!(
                "Searching {path} • {} result(s) • Esc to stop",
                progress.matched_entries
            ),
        });
        self.search.progress = Some(progress);
        cx.notify();
    }

    pub(crate) fn cancel_smart_search(&mut self, cx: &mut Context<Self>) {
        self.watcher.refresh_pending = false;
        if self.search.task.is_none() {
            return;
        }
        self.services.search.cancel();
        self.search.generation = self.search.generation.wrapping_add(1);
        self.search.task = None;
        self.search.request_id = None;
        self.search.progress = None;
        self.listing.state = ListingState::Ready;
        self.status_message = Some(format!(
            "Search stopped • {} partial result(s)",
            self.browser.visible_entries().len()
        ));
        cx.notify();
    }

    /// Finder's search scope bar: while the search field is in use, choose
    /// between this folder's items and the folder with all its subfolders.
    /// Smart folders carry their own scope, so they don't show it.
    pub(crate) fn render_search_scope_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.browser.active_smart_folder().is_some()
            || (!self.search.active && self.browser.search_query().is_empty())
        {
            return div().into_any_element();
        }
        let palette = self.palette;
        let mut folder = path_label(
            self.search
                .subfolders
                .as_ref()
                .map_or(self.browser.path(), |search| search.root.as_path()),
        );
        // Keep both choices on screen however long the folder's name is.
        if folder.chars().count() > SCOPE_FOLDER_LABEL_CHARS {
            folder = folder
                .chars()
                .take(SCOPE_FOLDER_LABEL_CHARS - 1)
                .chain(['…'])
                .collect();
        }
        let scope = self.search.scope;
        let button = |id: &'static str, label: String, value: SearchScope| {
            toolbar_button(id, &label, selected_control_color(scope == value, palette))
                .debug_selector(move || id.to_string())
                .role(Role::RadioButton)
                .aria_selected(scope == value)
                .on_click(cx.listener(move |this, _, _, cx| this.set_search_scope(value, cx)))
        };
        div()
            .id("search-scope-bar")
            .debug_selector(|| "search-scope-bar".to_string())
            .role(Role::RadioGroup)
            .aria_label("Search scope")
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(palette.border)
            .bg(palette.topbar)
            .text_xs()
            .child(div().text_color(palette.muted).child("Search:"))
            .child(button(
                "search-scope-this-folder",
                format!("“{folder}”"),
                SearchScope::ThisFolder,
            ))
            .child(button(
                "search-scope-subfolders",
                "Include subfolders".to_string(),
                SearchScope::Subfolders,
            ))
            .into_any_element()
    }

    pub(crate) fn rebuild_search_index(&mut self, cx: &mut Context<Self>) {
        let active_criteria = self.listing_search_criteria();
        if self.search.task.is_some() {
            self.cancel_smart_search(cx);
        }
        self.services.search.clear();
        if let Some(criteria) = active_criteria {
            self.start_smart_search(criteria, cx);
            self.show_toast("Rebuilding the search index", ToastKind::Success, cx);
        } else {
            self.show_toast(
                "Search index cleared; the next search will rebuild it",
                ToastKind::Success,
                cx,
            );
        }
    }

    pub(crate) fn handle_search_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.text_input.target.is_some()
            && !matches!(
                event.keystroke.key.as_str(),
                "enter" | "escape" | "up" | "down" | "tab"
            )
        {
            return;
        }
        let is_plain_space = Self::is_plain_space(&event.keystroke);
        if self.quick_look.open {
            if is_plain_space {
                self.handle_quick_look_space_down(event, cx);
                return;
            }
            let is_audio = matches!(
                self.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Audio,
                    ..
                }
            );
            let is_video = matches!(
                self.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Video,
                    ..
                }
            );
            match event.keystroke.key.as_str() {
                "escape" if self.quick_look.index_open => self.toggle_quick_look_index(cx),
                "escape" => self.close_quick_look(cx),
                "enter"
                    if event.keystroke.modifiers.platform && self.quick_look.paths.len() > 1 =>
                {
                    self.toggle_quick_look_index(cx)
                }
                "enter" if !event.is_held => {
                    if let Some(path) = self.preview.state.path().map(Path::to_path_buf) {
                        self.open_entry(path, false, cx);
                        self.close_quick_look(cx);
                    }
                }
                "left" | "up" => self.navigate_preview(-1, cx),
                "right" | "down" => self.navigate_preview(1, cx),
                "k" | "j" | "l" | "m" | "home" | "end" if is_audio || is_video => {
                    let (key, is_held) = (event.keystroke.key.clone(), event.is_held);
                    self.media
                        .update(cx, |media, cx| media.handle_shortcut(&key, is_held, cx));
                }
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        if self.navigation_ui.breadcrumb_editor.is_some() {
            self.handle_breadcrumb_editor_key(event, cx);
            return;
        }
        if self.navigation_ui.go_to_folder.is_some() {
            self.handle_go_to_folder_key(event, cx);
            return;
        }
        if self.settings_ui.shortcut_editor.is_some() {
            self.handle_shortcut_editor_key(event, cx);
            return;
        }
        if self.settings_ui.named_theme_editor.is_some() {
            self.handle_named_theme_key(event, cx);
            return;
        }
        if self.settings_ui.appearance_value_editor.is_some() {
            self.handle_appearance_value_key(event, cx);
            return;
        }
        if self.text_input.target == Some(TextInputTarget::FinderTag) {
            match event.keystroke.key.as_str() {
                "enter" => self.submit_finder_tag(cx),
                "escape" => self.cancel_add_finder_tag(cx),
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        if self.text_input.target == Some(TextInputTarget::PreviewFind) {
            match event.keystroke.key.as_str() {
                "enter" => self.advance_text_preview_match(1, cx),
                "escape" => {
                    self.preview.text.find.clear();
                    self.deactivate_native_text_input();
                    cx.notify();
                }
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        if self.operation_ui.undo_progress.is_some() && event.keystroke.key == "escape" {
            self.cancel_undo(cx);
            cx.stop_propagation();
            return;
        }
        if !self.operation_ui.conflict_prompts.is_empty() {
            match event.keystroke.key.as_str() {
                "escape" => self.cancel_all_file_conflicts(cx),
                "enter" | "s" if !event.is_held => {
                    self.resolve_file_conflict(FileConflictChoice::Skip, cx)
                }
                "r" if !event.is_held => {
                    self.resolve_file_conflict(FileConflictChoice::Replace, cx)
                }
                "k" if !event.is_held => {
                    self.resolve_file_conflict(FileConflictChoice::KeepBoth, cx)
                }
                "a" if !event.is_held => self.toggle_file_conflict_apply_to_all(cx),
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        if self.settings_ui.panel_open {
            match event.keystroke.key.as_str() {
                "escape" => self.close_settings_panel(cx),
                "tab" if event.keystroke.modifiers.shift => window.focus_prev(cx),
                "tab" => window.focus_next(cx),
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        if self.overlay.toolbar_menu != ToolbarMenu::Closed {
            match event.keystroke.key.as_str() {
                "escape" => self.close_toolbar_menu(cx),
                "down" => window.focus_next(cx),
                "up" => window.focus_prev(cx),
                "tab" if event.keystroke.modifiers.shift => window.focus_prev(cx),
                "tab" => window.focus_next(cx),
                "home" => {
                    if let Some(trigger) = self.overlay.return_focus.as_ref() {
                        window.focus(trigger, cx);
                        window.focus_next(cx);
                    }
                }
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        if self.overlay.surface != ControlSurface::Closed {
            self.handle_control_surface_key(event, cx);
            return;
        }
        if self.mutation.prompt.is_some() {
            self.handle_mutation_prompt_key(event, cx);
            return;
        }
        if self.preview.tab == PreviewTab::CustomFields
            && self
                .preview
                .custom_fields_editor
                .as_ref()
                .is_some_and(|editor| editor.draft.is_some())
        {
            self.handle_custom_field_key(event, cx);
            return;
        }
        if self.context_menu.menu.is_some() {
            self.handle_context_menu_key(event, cx);
            return;
        }
        if is_plain_space {
            self.handle_quick_look_space_down(event, cx);
            return;
        }
        if !self.search.active {
            self.handle_type_select_key(event, cx);
            return;
        }

        match event.keystroke.key.as_str() {
            "backspace" => {
                self.browser.pop_search_character();
                self.search_query_did_change(cx);
                cx.stop_propagation();
                cx.notify();
            }
            "escape" => {
                self.clear_search_and_focus_list(window, cx);
                cx.stop_propagation();
            }
            "enter" => {
                self.search.active = false;
                self.deactivate_native_text_input();
                window.focus(&self.focus_handle, cx);
                cx.stop_propagation();
                cx.notify();
            }
            _ if !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(text) = event.keystroke.key_char.as_deref()
                    && !text.chars().any(char::is_control)
                {
                    self.browser.push_search_text(text);
                    self.search_query_did_change(cx);
                    cx.stop_propagation();
                    cx.notify();
                }
            }
            _ => {}
        }
    }
}

/// The status line for a finished smart-folder search, or a search of the
/// subfolders of `root`: how many results, whether they are partial, and
/// what answered the search.
pub(crate) fn search_result_status(result: &SearchResult, root: Option<&Path>) -> String {
    let count = result.entries.len();
    let crawler_status = || {
        if result.reused_index {
            "cached index".to_string()
        } else if result.content_reads > 0 {
            format!("{} content files indexed", result.content_reads)
        } else {
            format!("{} paths indexed", result.indexed_entries)
        }
    };
    let source = match result.source {
        SearchSource::Spotlight => "Spotlight".to_string(),
        SearchSource::Mixed => format!("Spotlight + {}", crawler_status()),
        SearchSource::Crawler => crawler_status(),
    };
    let plural = if count == 1 { "" } else { "s" };
    let results = match root {
        Some(root) => format!("{count} result{plural} in {}", path_label(root)),
        None => format!("{count} smart-folder result{plural}"),
    };
    if result.truncated {
        format!("{results} (partial results) • {source}")
    } else {
        format!("{results} • {source}")
    }
}
