//! `DirectoryWindow` behavior for navigation.

use crate::*;

/// Ways of typing or picking a folder to go to: the Go to Folder sheet and
/// its completion task, the breadcrumb path editor, and the folder picker
/// shown when the current folder cannot be listed.
#[derive(Default)]
pub(crate) struct NavigationUi {
    pub(crate) go_to_folder: Option<GoToFolderState>,
    pub(crate) go_to_folder_task: Option<Task<()>>,
    pub(crate) breadcrumb_editor: Option<BreadcrumbEditor>,
    pub(crate) folder_picker_active: bool,
    pub(crate) folder_picker_error: Option<String>,
}

impl DirectoryWindow {
    pub(crate) fn prepare_column_selection(&mut self, origin: &Path, target: &Path) {
        if self.browser.view_mode() != ViewMode::Column {
            return;
        }
        let selection = if target.starts_with(origin) {
            ColumnSelectionTarget::First
        } else if origin.starts_with(target) {
            origin
                .ancestors()
                .find(|ancestor| ancestor.parent() == Some(target))
                .map(Path::to_path_buf)
                .map(ColumnSelectionTarget::Path)
                .unwrap_or(ColumnSelectionTarget::First)
        } else {
            ColumnSelectionTarget::First
        };
        self.column_view.pending_selection = Some(selection);
    }

    pub(crate) fn navigate_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.sync_active_tab_view_state();
        let origin = self.browser.path().to_path_buf();
        if self.browser.navigate(path) {
            let target = self.browser.path().to_path_buf();
            self.prepare_column_selection(&origin, &target);
            self.folder_did_change(cx);
        }
    }

    /// The current folder changed by navigating (not by switching tabs):
    /// like Finder, its search ends.
    pub(crate) fn folder_did_change(&mut self, cx: &mut Context<Self>) {
        self.reset_search_for_navigation();
        self.path_did_change(cx);
    }

    pub(crate) fn activate_column(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(column) = self.column_view.columns.columns().get(index) else {
            return;
        };
        if column.path() == self.browser.path() {
            return;
        }
        let path = column.path().to_path_buf();
        let entries = column.entries().to_vec();
        let ready = !column.loading() && column.error().is_none();
        self.sync_active_tab_view_state();
        self.browser.navigate(path);
        // An interaction inside the column strip must keep that view active,
        // even if this folder has a different saved view mode.
        self.browser.set_view_mode(ViewMode::Column);
        self.folder_did_change(cx);
        self.column_view.pending_selection = None;
        // Use the displayed listing immediately so this click and subsequent
        // keyboard input work while the fresh directory request is pending.
        self.browser.replace_entries(entries);
        self.browser.clear_selection();
        if ready {
            self.listing.state = ListingState::Ready;
        }
    }

    pub(crate) fn begin_breadcrumb_edit(&mut self, cx: &mut Context<Self>) {
        self.overlay.toolbar_menu = ToolbarMenu::Closed;
        self.search.active = false;
        self.navigation_ui.breadcrumb_editor = Some(BreadcrumbEditor {
            input: self.browser.path().to_string_lossy().into_owned(),
            replace_on_type: true,
        });
        cx.notify();
    }

    pub(crate) fn cancel_breadcrumb_edit(&mut self, cx: &mut Context<Self>) {
        if self.navigation_ui.breadcrumb_editor.take().is_some() {
            self.deactivate_native_text_input();
            cx.notify();
        }
    }

    pub(crate) fn submit_breadcrumb_edit(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self
            .navigation_ui
            .breadcrumb_editor
            .take()
            .map(|editor| editor.input.trim().to_string())
        else {
            return;
        };
        self.deactivate_native_text_input();
        if !input.is_empty() && Path::new(&input) != self.browser.path() {
            self.navigate_to(PathBuf::from(input), cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn open_folder_picker(&mut self, cx: &mut Context<Self>) {
        if self.navigation_ui.folder_picker_active {
            return;
        }
        self.navigation_ui.folder_picker_active = true;
        self.navigation_ui.folder_picker_error = None;
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose folder".into()),
        });
        cx.spawn(async move |this, cx| {
            let selection = match receiver.await {
                Ok(Ok(Some(mut paths))) => Ok(paths.pop()),
                Ok(Ok(None)) => Ok(None),
                Ok(Err(error)) => Err(error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let _ = this.update(cx, |view, cx| {
                view.complete_folder_picker(selection, cx);
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn complete_folder_picker(
        &mut self,
        selection: Result<Option<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        self.navigation_ui.folder_picker_active = false;
        match selection {
            Ok(Some(path)) => {
                self.navigation_ui.folder_picker_error = None;
                self.navigate_to(path, cx);
            }
            Ok(None) => {}
            Err(error) => self.navigation_ui.folder_picker_error = Some(error),
        }
        cx.notify();
    }

    pub(crate) fn open_go_to_folder(&mut self, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        self.overlay.toolbar_menu = ToolbarMenu::Closed;
        self.overlay.surface = ControlSurface::Closed;
        self.settings_ui.panel_open = false;
        self.navigation_ui.go_to_folder_task = None;
        self.navigation_ui.go_to_folder = Some(GoToFolderState {
            input: self.browser.path().display().to_string(),
            replace_on_type: true,
            error: None,
            suggestions: Vec::new(),
            selected: None,
            validating: false,
            generation: 0,
        });
        self.schedule_go_to_folder_suggestions(cx);
        cx.notify();
    }

    pub(crate) fn close_go_to_folder(&mut self, cx: &mut Context<Self>) {
        self.navigation_ui.go_to_folder = None;
        self.navigation_ui.go_to_folder_task = None;
        cx.notify();
    }

    pub(crate) fn schedule_go_to_folder_suggestions(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.navigation_ui.go_to_folder.as_mut() else {
            return;
        };
        dialog.generation = dialog.generation.wrapping_add(1);
        let generation = dialog.generation;
        let input = dialog.input.trim().to_string();
        dialog.selected = None;
        self.navigation_ui.go_to_folder_task = None;

        if input.is_empty() {
            dialog.suggestions = self
                .browser
                .go_to_folder_recents()
                .iter()
                .cloned()
                .map(|path| FolderSuggestion { path, recent: true })
                .collect();
            cx.notify();
            return;
        }

        let current_path = self.browser.path().to_path_buf();
        let services = self.services.clone();
        let executor = cx.background_executor().clone();
        self.navigation_ui.go_to_folder_task = Some(cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(150)).await;
            let suggestions = match expand_go_to_folder_path(&services, &input).await {
                Ok(expanded) => {
                    let (parent, prefix) = go_to_folder_parent_prefix(&expanded, &current_path);
                    services
                        .listing
                        .list(ListRequest {
                            path: parent,
                            calc_dir_size: false,
                        })
                        .await
                        .map(|entries| {
                            entries
                                .into_iter()
                                .filter(|entry| entry.is_dir)
                                .filter(|entry| {
                                    file_name(entry)
                                        .to_lowercase()
                                        .starts_with(&prefix.to_lowercase())
                                })
                                .take(10)
                                .map(|entry| FolderSuggestion {
                                    path: entry.path,
                                    recent: false,
                                })
                                .collect::<Vec<_>>()
                        })
                }
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |view, cx| {
                let Some(dialog) = view.navigation_ui.go_to_folder.as_mut() else {
                    return;
                };
                if dialog.generation != generation {
                    return;
                }
                view.navigation_ui.go_to_folder_task = None;
                dialog.suggestions = suggestions.unwrap_or_default();
                dialog.selected = None;
                cx.notify();
            });
        }));
    }

    pub(crate) fn select_go_to_folder_suggestion(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(dialog) = self.navigation_ui.go_to_folder.as_mut() else {
            return;
        };
        let Some(suggestion) = dialog.suggestions.get(index) else {
            return;
        };
        dialog.input = suggestion.path.display().to_string();
        dialog.replace_on_type = false;
        dialog.error = None;
        dialog.suggestions.clear();
        dialog.selected = None;
        cx.notify();
    }

    pub(crate) fn move_go_to_folder_suggestion(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(dialog) = self.navigation_ui.go_to_folder.as_mut() else {
            return;
        };
        let count = dialog.suggestions.len();
        if count == 0 {
            return;
        }
        dialog.selected = Some(dialog.selected.map_or_else(
            || if delta > 0 { 0 } else { count - 1 },
            |index| (index as isize + delta).rem_euclid(count as isize) as usize,
        ));
        cx.notify();
    }

    pub(crate) fn submit_go_to_folder(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.navigation_ui.go_to_folder.as_mut() else {
            return;
        };
        let input = dialog.input.trim().to_string();
        if input.is_empty() {
            dialog.error = Some("Please enter a path".to_string());
            cx.notify();
            return;
        }
        dialog.generation = dialog.generation.wrapping_add(1);
        let generation = dialog.generation;
        dialog.validating = true;
        dialog.error = None;
        dialog.suggestions.clear();
        dialog.selected = None;
        let services = self.services.clone();
        self.navigation_ui.go_to_folder_task = Some(cx.spawn(async move |this, cx| {
            let result = match expand_go_to_folder_path(&services, &input).await {
                Ok(path) => services
                    .listing
                    .list(ListRequest {
                        path: path.clone(),
                        calc_dir_size: false,
                    })
                    .await
                    .map(|_| path),
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |view, cx| {
                if view
                    .navigation_ui
                    .go_to_folder
                    .as_ref()
                    .is_none_or(|dialog| dialog.generation != generation)
                {
                    return;
                }
                view.navigation_ui.go_to_folder_task = None;
                match result {
                    Ok(path) => {
                        view.mutate_shared_session(|session| {
                            session.record_go_to_folder(path.clone())
                        });
                        view.persist_session();
                        view.navigation_ui.go_to_folder = None;
                        view.navigate_to(path, cx);
                        cx.notify();
                    }
                    Err(error) => {
                        if let Some(dialog) = view.navigation_ui.go_to_folder.as_mut() {
                            dialog.validating = false;
                            dialog.error = Some(go_to_folder_error(&error));
                        }
                        cx.notify();
                    }
                }
            });
        }));
        cx.notify();
    }

    pub(crate) fn go_back(&mut self, cx: &mut Context<Self>) {
        self.close_toolbar_menu(cx);
        self.sync_active_tab_view_state();
        let origin = self.browser.path().to_path_buf();
        if self.browser.go_back() {
            let target = self.browser.path().to_path_buf();
            self.prepare_column_selection(&origin, &target);
            self.folder_did_change(cx);
        }
    }

    pub(crate) fn go_forward(&mut self, cx: &mut Context<Self>) {
        self.close_toolbar_menu(cx);
        self.sync_active_tab_view_state();
        let origin = self.browser.path().to_path_buf();
        if self.browser.go_forward() {
            let target = self.browser.path().to_path_buf();
            self.prepare_column_selection(&origin, &target);
            self.folder_did_change(cx);
        }
    }

    pub(crate) fn go_to_back_history(&mut self, index: usize, cx: &mut Context<Self>) {
        self.close_toolbar_menu(cx);
        self.sync_active_tab_view_state();
        let origin = self.browser.path().to_path_buf();
        if self.browser.go_to_back_history(index) {
            let target = self.browser.path().to_path_buf();
            self.prepare_column_selection(&origin, &target);
            self.folder_did_change(cx);
        }
    }

    pub(crate) fn go_to_forward_history(&mut self, index: usize, cx: &mut Context<Self>) {
        self.close_toolbar_menu(cx);
        self.sync_active_tab_view_state();
        let origin = self.browser.path().to_path_buf();
        if self.browser.go_to_forward_history(index) {
            let target = self.browser.path().to_path_buf();
            self.prepare_column_selection(&origin, &target);
            self.folder_did_change(cx);
        }
    }

    pub(crate) fn clear_navigation_history(&mut self, cx: &mut Context<Self>) {
        self.browser.clear_navigation_history();
        self.persist_session();
        self.show_toast("Navigation history cleared", ToastKind::Success, cx);
    }

    pub(crate) fn go_up(&mut self, cx: &mut Context<Self>) {
        self.close_toolbar_menu(cx);
        self.sync_active_tab_view_state();
        let origin = self.browser.path().to_path_buf();
        if self.browser.go_up() {
            let target = self.browser.path().to_path_buf();
            self.prepare_column_selection(&origin, &target);
            self.folder_did_change(cx);
        }
    }

    pub(crate) fn new_tab(&mut self, cx: &mut Context<Self>) {
        self.sync_active_tab_view_state();
        self.browser.new_tab();
        self.search.active = false;
        self.path_did_change(cx);
    }

    pub(crate) fn activate_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        self.sync_active_tab_view_state();
        if self.browser.activate(id) {
            self.search.active = false;
            self.path_did_change(cx);
        }
    }

    pub(crate) fn close_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        let was_active = self.browser.active_tab_id() == id;
        if was_active {
            self.sync_active_tab_view_state();
        }
        if self.browser.close(id) {
            if was_active {
                self.search.active = false;
                self.path_did_change(cx);
            } else {
                self.persist_session();
                cx.notify();
            }
        }
    }

    pub(crate) fn close_active_tab(&mut self, cx: &mut Context<Self>) {
        let id = self.browser.active_tab_id();
        self.close_tab(id, cx);
    }

    pub(crate) fn close_active_tab_or_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.browser.tabs().len() > 1 {
            self.close_active_tab(cx);
            return;
        }

        if self.request_platform_window_close(cx) {
            window.remove_window();
        }
    }

    pub(crate) fn activate_tab_offset(&mut self, offset: isize, cx: &mut Context<Self>) {
        self.sync_active_tab_view_state();
        if self.browser.activate_offset(offset) {
            self.search.active = false;
            self.path_did_change(cx);
        }
    }

    pub(crate) fn move_active_tab(&mut self, offset: isize, cx: &mut Context<Self>) {
        if self.browser.move_active(offset) {
            self.persist_session();
            cx.notify();
        }
    }

    pub(crate) fn toggle_current_favorite(&mut self, cx: &mut Context<Self>) {
        let path = self.browser.path().to_path_buf();
        self.mutate_shared_session(|session| session.toggle_favorite(path));
        self.persist_session();
        cx.notify();
    }

    /// Navigate into `path` when `navigate` (a folder or a link to one),
    /// otherwise ask the system to open it.
    pub(crate) fn open_entry(&mut self, path: PathBuf, navigate: bool, cx: &mut Context<Self>) {
        if navigate {
            self.navigate_to(path, cx);
            return;
        }

        let task = self.services.integration.open(path.clone());
        let generation = self.begin_integration_action(format!("Opening {}…", path.display()));
        let task = cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                let status = match result {
                    Ok(()) => None,
                    Err(error) => {
                        view.record_error("Open failed", error.to_string());
                        Some(format!("Unable to open {}: {error}", path.display()))
                    }
                };
                if view.system.integration_generation == generation {
                    view.status_message = status;
                }
                cx.notify();
            });
        });
        self.push_integration_task(task);
    }

    /// Open the selection: folders and links to folders are browsed (a link
    /// keeps its own path), everything else, packages included, is opened
    /// by the system.
    pub(crate) fn open_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.effective_selected_entry() {
            let navigate = is_folder_like(&entry);
            self.open_entry(entry.path, navigate, cx);
        }
    }

    pub(crate) fn reveal_selected(&self, entry_index: usize) {
        match self.browser.view_mode() {
            ViewMode::List => self
                .listing
                .scroll_handle
                .scroll_to_item(entry_index, ScrollStrategy::Center),
            ViewMode::Grid => self.listing.scroll_handle.scroll_to_item(
                entry_index / self.layout.grid_columns.max(1),
                ScrollStrategy::Center,
            ),
            ViewMode::Column => {
                if let Some(handle) = self.column_view.scroll_handles.last() {
                    handle.scroll_to_item(entry_index, ScrollStrategy::Center);
                }
            }
        }
    }

    pub(crate) fn column_left(&mut self, cx: &mut Context<Self>) {
        if self.quick_look.open
            || (self.browser.view_mode() != ViewMode::Column
                && !matches!(self.preview.state, PreviewState::Closed))
        {
            self.navigate_preview(-1, cx);
            return;
        }
        match self.browser.view_mode() {
            ViewMode::Grid => self.select_by_offset(-1, false, cx),
            ViewMode::Column => self.go_up(cx),
            ViewMode::List => {}
        }
    }

    pub(crate) fn column_right(&mut self, cx: &mut Context<Self>) {
        if self.quick_look.open
            || (self.browser.view_mode() != ViewMode::Column
                && !matches!(self.preview.state, PreviewState::Closed))
        {
            self.navigate_preview(1, cx);
            return;
        }
        match self.browser.view_mode() {
            ViewMode::Grid => self.select_by_offset(1, false, cx),
            ViewMode::Column => {
                if let Some(entry) = self.effective_selected_entry()
                    && is_folder_like(&entry)
                {
                    self.navigate_to(entry.path, cx);
                }
            }
            ViewMode::List => {}
        }
    }

    pub(crate) fn handle_breadcrumb_editor_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => self.cancel_breadcrumb_edit(cx),
            "enter" => self.submit_breadcrumb_edit(cx),
            "backspace" => {
                if let Some(editor) = self.navigation_ui.breadcrumb_editor.as_mut() {
                    if editor.replace_on_type {
                        editor.input.clear();
                        editor.replace_on_type = false;
                    } else {
                        editor.input.pop();
                    }
                }
                cx.notify();
            }
            "a" if event.keystroke.modifiers.control || event.keystroke.modifiers.platform => {
                if let Some(editor) = self.navigation_ui.breadcrumb_editor.as_mut() {
                    editor.replace_on_type = true;
                }
                cx.notify();
            }
            _ if !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(text) = event.keystroke.key_char.as_deref()
                    && !text.chars().any(char::is_control)
                    && let Some(editor) = self.navigation_ui.breadcrumb_editor.as_mut()
                {
                    if editor.replace_on_type {
                        editor.input.clear();
                        editor.replace_on_type = false;
                    }
                    editor.input.push_str(text);
                    cx.notify();
                }
            }
            _ => {}
        }
        cx.stop_propagation();
    }

    pub(crate) fn handle_go_to_folder_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let suggestions_visible = self
            .navigation_ui
            .go_to_folder
            .as_ref()
            .is_some_and(|dialog| !dialog.suggestions.is_empty());
        match event.keystroke.key.as_str() {
            "escape" if suggestions_visible => {
                if let Some(dialog) = self.navigation_ui.go_to_folder.as_mut() {
                    dialog.suggestions.clear();
                    dialog.selected = None;
                }
                cx.notify();
            }
            "escape" => self.close_go_to_folder(cx),
            "down" if suggestions_visible => self.move_go_to_folder_suggestion(1, cx),
            "up" if suggestions_visible => self.move_go_to_folder_suggestion(-1, cx),
            "tab" if suggestions_visible => {
                let index = self
                    .navigation_ui
                    .go_to_folder
                    .as_ref()
                    .and_then(|dialog| dialog.selected)
                    .unwrap_or(0);
                self.select_go_to_folder_suggestion(index, cx);
            }
            "enter"
                if self
                    .navigation_ui
                    .go_to_folder
                    .as_ref()
                    .and_then(|dialog| dialog.selected)
                    .is_some() =>
            {
                let index = self
                    .navigation_ui
                    .go_to_folder
                    .as_ref()
                    .and_then(|dialog| dialog.selected)
                    .unwrap();
                self.select_go_to_folder_suggestion(index, cx);
            }
            "enter" => self.submit_go_to_folder(cx),
            "backspace" => {
                if let Some(dialog) = self.navigation_ui.go_to_folder.as_mut() {
                    dialog.error = None;
                    if dialog.replace_on_type {
                        dialog.input.clear();
                        dialog.replace_on_type = false;
                    } else {
                        dialog.input.pop();
                    }
                }
                self.schedule_go_to_folder_suggestions(cx);
                cx.notify();
            }
            "a" if event.keystroke.modifiers.control || event.keystroke.modifiers.platform => {
                if let Some(dialog) = self.navigation_ui.go_to_folder.as_mut() {
                    dialog.replace_on_type = true;
                }
                cx.notify();
            }
            _ if !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(text) = event.keystroke.key_char.as_deref()
                    && !text.chars().any(char::is_control)
                    && let Some(dialog) = self.navigation_ui.go_to_folder.as_mut()
                {
                    dialog.error = None;
                    if dialog.replace_on_type {
                        dialog.input.clear();
                        dialog.replace_on_type = false;
                    }
                    dialog.input.push_str(text);
                    self.schedule_go_to_folder_suggestions(cx);
                    cx.notify();
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(crate) fn render_go_to_folder(
        &mut self,
        window_height: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(dialog) = self.navigation_ui.go_to_folder.clone() else {
            return div().into_any_element();
        };
        let can_submit = !dialog.validating && !dialog.input.trim().is_empty();
        let palette = self.palette;
        let suggestions = dialog
            .suggestions
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, suggestion)| {
                let selected = dialog.selected == Some(index);
                let path = suggestion.path.display().to_string();
                div()
                    .id(("go-to-folder-suggestion", index))
                    .debug_selector(move || format!("go-to-folder-suggestion-{index}"))
                    .role(Role::ListBoxOption)
                    .aria_label(path.clone())
                    .aria_selected(selected)
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_h(px(34.0))
                    .px_3()
                    .bg(if selected {
                        palette.hover
                    } else {
                        palette.panel
                    })
                    .hover(move |row| row.bg(palette.hover))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_go_to_folder_suggestion(index, cx)
                    }))
                    .child(toolbar_icon(
                        if suggestion.recent {
                            "reload"
                        } else {
                            "folder"
                        },
                        13.0,
                        palette.muted,
                    ))
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(path))
                    .when(suggestion.recent, |row| {
                        row.child(
                            div()
                                .px_2()
                                .py_1()
                                .border_1()
                                .border_color(palette.border)
                                .text_xs()
                                .text_color(palette.tertiary)
                                .child("Recent"),
                        )
                    })
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        div()
            .id("go-to-folder-backdrop")
            .debug_selector(|| "go-to-folder-backdrop".to_string())
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .pt(px((window_height * 0.15).max(12.0)))
            .px_3()
            .pb_3()
            .bg(with_alpha(rgb(0x000000), 0.58))
            .on_click(cx.listener(|this, _, _, cx| this.close_go_to_folder(cx)))
            .child(
                div()
                    .id("go-to-folder-dialog")
                    .debug_selector(|| "go-to-folder-dialog".to_string())
                    .role(Role::Dialog)
                    .aria_label("Go to Folder")
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w_full()
                    .max_w(px(560.0 * self.palette.scale))
                    .max_h(px(
                        (window_height - 24.0 * self.palette.scale).max(1.0 * self.palette.scale)
                    ))
                    .overflow_y_scroll()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.panel)
                    .shadow_lg()
                    .occlude()
                    .on_click(cx.listener(|_, _, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .text_size(px(16.0))
                            .font_weight(FontWeight::MEDIUM)
                            .child("Go to Folder"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(self.palette.muted)
                                    .child("Folder path"),
                            )
                            .child(
                                div()
                                    .id("go-to-folder-input")
                                    .debug_selector(|| "go-to-folder-input".to_string())
                                    .role(Role::EditableComboBox)
                                    .aria_label("Folder path")
                                    .font_family(monospace_font_family())
                                    .child(
                                        self.native_text_input_element(TextInputTarget::GoToFolder)
                                            .unwrap_or_else(|| {
                                                div().child(dialog.input.clone()).into_any_element()
                                            }),
                                    ),
                            )
                            .when_some(dialog.error, |input, error| {
                                input.child(
                                    div()
                                        .id("go-to-folder-error")
                                        .debug_selector(|| "go-to-folder-error".to_string())
                                        .role(Role::Alert)
                                        .text_xs()
                                        .text_color(rgb(0xff6b6b))
                                        .child(error),
                                )
                            })
                            .when(!suggestions.is_empty(), |input| {
                                input.child(
                                    div()
                                        .id("go-to-folder-suggestions")
                                        .debug_selector(|| "go-to-folder-suggestions".to_string())
                                        .role(Role::ListBox)
                                        .aria_label("Folder suggestions")
                                        .flex()
                                        .flex_col()
                                        .max_h(px(240.0))
                                        .overflow_y_scroll()
                                        .border_1()
                                        .border_color(self.palette.border)
                                        .bg(self.palette.panel)
                                        .children(suggestions),
                                )
                            }),
                    )
                    .child(div().text_xs().text_color(self.palette.tertiary).child(
                        "Enter navigates • Tab autocompletes • Esc cancels • ~ expands home",
                    ))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                toolbar_button(
                                    "go-to-folder-cancel",
                                    "Cancel",
                                    self.palette.control,
                                )
                                .debug_selector(|| "go-to-folder-cancel".to_string())
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.close_go_to_folder(cx)),
                                ),
                            )
                            .child(
                                toolbar_button_enabled(
                                    "go-to-folder-submit",
                                    if dialog.validating {
                                        "Validating…"
                                    } else {
                                        "Go"
                                    },
                                    if can_submit {
                                        self.palette.accent
                                    } else {
                                        self.palette.disabled
                                    },
                                    can_submit,
                                )
                                .debug_selector(|| "go-to-folder-submit".to_string())
                                .role(Role::DefaultButton)
                                .aria_label(if dialog.validating {
                                    "Validating folder"
                                } else if dialog.input.trim().is_empty() {
                                    "Enter a folder path before navigating"
                                } else {
                                    "Go to folder"
                                })
                                .px_4()
                                .py_2()
                                .text_color(if !can_submit {
                                    self.palette.muted
                                } else if self.palette.dark {
                                    rgb(0x000000)
                                } else {
                                    self.palette.text
                                })
                                .when(can_submit, |button| {
                                    button.on_click(
                                        cx.listener(|this, _, _, cx| this.submit_go_to_folder(cx)),
                                    )
                                }),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The toolbar's path. In the single-row toolbar the strip asks for its
    /// full width and gives way only after the search field has shrunk; it
    /// never shrinks below the current folder's whole label (up to its cap),
    /// and ancestors collapse root-first to make room.
    pub(crate) fn render_breadcrumbs(
        &mut self,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = self.palette;
        if let Some(editor) = self.navigation_ui.breadcrumb_editor.as_ref() {
            return div()
                .id("breadcrumbs")
                .debug_selector(|| "breadcrumbs".to_string())
                .role(Role::Group)
                .aria_label("Edit folder path")
                .flex()
                .flex_1()
                .min_w(px(96.0))
                .items_center()
                .px_2()
                .py_1()
                .rounded_sm()
                .border_1()
                .border_color(palette.accent)
                .bg(palette.window)
                .cursor_text()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.cancel_breadcrumb_edit(cx);
                }))
                .child(
                    div()
                        .id("breadcrumb-path-input")
                        .debug_selector(|| "breadcrumb-path-input".to_string())
                        .w_full()
                        .font_family(monospace_font_family())
                        .child(
                            self.native_text_input_element(TextInputTarget::Breadcrumb)
                                .unwrap_or_else(|| {
                                    div().child(editor.input.clone()).into_any_element()
                                }),
                        ),
                )
                .into_any_element();
        }
        let current_path = self.browser.path().to_path_buf();
        let stack = build_path_stack(&current_path);
        let current_index = stack.len().saturating_sub(1);
        let current_label = stack
            .last()
            .map(|path| {
                path.file_name()
                    .unwrap_or_else(|| path.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_default();
        let current_max_width = BREADCRUMB_CURRENT_MAX_WIDTH * palette.scale;
        // The current crumb's label plus its horizontal padding.
        let current_width = (self.breadcrumb_label_width(&current_label, cx) + 8.0 * palette.scale)
            .min(current_max_width);
        let mut crumbs = Vec::with_capacity(stack.len());
        for (index, path) in stack.into_iter().enumerate() {
            let is_current = index == current_index;
            let label = path
                .file_name()
                .unwrap_or_else(|| path.as_os_str())
                .to_string_lossy()
                .into_owned();
            let tooltip_label = label.clone();
            let crumb = div()
                .id(("breadcrumb", index))
                .debug_selector(move || format!("breadcrumb-{index}"))
                .role(Role::Button)
                .aria_label(if is_current {
                    format!("Edit current folder path, {label}")
                } else {
                    format!("Navigate to {label}")
                })
                .focusable()
                .tab_stop(true)
                .px_1()
                .rounded_sm()
                .focus(move |crumb| crumb.bg(palette.hover))
                .hover(move |crumb| crumb.bg(palette.hover))
                .when(is_current, |crumb| crumb.cursor_text())
                .when(!is_current, |crumb| crumb.cursor_pointer())
                .whitespace_nowrap()
                .max_w(px(if is_current {
                    current_max_width
                } else {
                    120.0 * palette.scale
                }))
                .truncate()
                .text_sm()
                .text_color(palette.text)
                .when(is_current, |crumb| {
                    crumb
                        .flex_shrink_0()
                        .min_w(px(32.0 * palette.scale))
                        .font_weight(FontWeight::SEMIBOLD)
                })
                .tooltip(move |_, cx| app_tooltip(tooltip_label.clone(), palette, cx))
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if is_current {
                        this.begin_breadcrumb_edit(cx);
                        window.focus(&this.focus_handle, cx);
                    } else {
                        this.navigate_to(path.clone(), cx);
                    }
                }));
            if is_current {
                crumbs.push(crumb.into_any_element());
                continue;
            }
            // Each ancestor collapses together with its separator, root-most
            // first, so deep paths keep the current folder and its nearest
            // parents readable instead of squeezing every label to nothing.
            let collapse_weight = breadcrumb_collapse_weight(current_index - index);
            crumbs.push(
                div()
                    .flex()
                    .items_center()
                    .min_w_0()
                    .overflow_hidden()
                    .map(|mut group| {
                        group.style().flex_shrink = Some(collapse_weight);
                        group
                    })
                    .child(crumb)
                    .child(
                        div()
                            .flex_shrink_0()
                            .mx_1()
                            .text_color(palette.tertiary)
                            .child(toolbar_icon(
                                "arrow-right",
                                palette.icon_size.clamp(10.0, 13.0),
                                palette.tertiary,
                            )),
                    )
                    .into_any_element(),
            );
        }

        // Room for the whole current crumb and the edit hit area beside it.
        let min_width = (current_width + BREADCRUMB_HIT_AREA_MIN_WIDTH + 1.0).max(96.0);
        div()
            .id("breadcrumbs")
            .debug_selector(|| "breadcrumbs".to_string())
            .flex()
            .map(|strip| {
                if compact {
                    // A content-sized basis would wrap the actions after it
                    // onto the second toolbar row.
                    strip.flex_1()
                } else {
                    // The trailing controls shrink far faster (see
                    // `render_toolbar`), so the path gives way last.
                    strip.flex_auto()
                }
            })
            .min_w(px(min_width))
            .items_center()
            .justify_start()
            .overflow_x_scroll()
            .children(crumbs)
            .child(
                div()
                    .id("breadcrumb-edit-hit-area")
                    .debug_selector(|| "breadcrumb-edit-hit-area".to_string())
                    .flex_1()
                    .min_w(px(BREADCRUMB_HIT_AREA_MIN_WIDTH))
                    .h_full()
                    .cursor_text(),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.begin_breadcrumb_edit(cx);
                window.focus(&this.focus_handle, cx);
            }))
            .into_any_element()
    }
}

const BREADCRUMB_CURRENT_MAX_WIDTH: f32 = 180.0;
const BREADCRUMB_HIT_AREA_MIN_WIDTH: f32 = 8.0;

impl DirectoryWindow {
    /// Width of a breadcrumb label in the current crumb's font.
    fn breadcrumb_label_width(&self, label: &str, cx: &App) -> f32 {
        let text_system = cx.text_system();
        let mut font = gpui::font(font_family(&self.settings));
        font.weight = FontWeight::SEMIBOLD;
        let font_id = text_system.resolve_font(&font);
        // `text_sm` is 0.875 rem, and the rem follows the UI scale.
        let font_size = px(14.0 * self.palette.scale);
        let width: f32 = label
            .chars()
            .map(|character| f32::from(text_system.layout_width(font_id, font_size, character)))
            .sum();
        // Shaping can differ slightly from summed advances.
        width.ceil() + 2.0
    }
}

/// Flex-shrink weight for a breadcrumb ancestor `distance` segments above the
/// current folder. Weights grow steeply so farther ancestors absorb nearly all
/// of the overflow before nearer ones begin to shrink; the cap keeps the
/// weighted sums well inside `f32` range.
fn breadcrumb_collapse_weight(distance: usize) -> f32 {
    64f32.powi(distance.clamp(1, 16) as i32)
}
