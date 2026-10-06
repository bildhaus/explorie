//! `DirectoryWindow` behavior for commands.

use crate::*;

/// Control surfaces (command palette, shortcuts, diagnostics and the other
/// managers) and toolbar menus: which one is open, the palette query and
/// selection, and where focus returns when they close.
pub(crate) struct OverlayUi {
    pub(crate) surface: ControlSurface,
    pub(crate) toolbar_menu: ToolbarMenu,
    pub(crate) query: String,
    pub(crate) selected: usize,
    pub(crate) return_focus: Option<FocusHandle>,
    pub(crate) focus_pending: bool,
    pub(crate) restore_pending: bool,
}

impl Default for OverlayUi {
    fn default() -> Self {
        Self {
            surface: ControlSurface::Closed,
            toolbar_menu: ToolbarMenu::Closed,
            query: String::new(),
            selected: 0,
            return_focus: None,
            focus_pending: false,
            restore_pending: false,
        }
    }
}

impl DirectoryWindow {
    pub(crate) fn command_context(&self) -> CommandContext {
        CommandContext {
            show_hidden: self.browser.show_hidden(),
            show_preview: self.settings.view.show_preview_panel,
            show_status: self.settings.view.show_status_bar,
            sidebar_collapsed: self.layout.sidebar_collapsed,
            favorite: self.browser.is_favorite(self.browser.path()),
        }
    }

    /// What the macOS menu bar reflects while this window is active.
    pub(crate) fn menu_state(&self) -> crate::app_menu::MenuState {
        crate::app_menu::MenuState {
            shortcut_overrides: self.settings.shortcut_bindings.clone(),
            view_mode: Some(self.browser.view_mode()),
            show_hidden: self.browser.show_hidden(),
            show_preview: self.settings.view.show_preview_panel,
            show_status: self.settings.view.show_status_bar,
            show_sidebar: !self.layout.sidebar_collapsed,
            favorite: self.browser.is_favorite(self.browser.path()),
            multiple_tabs: self.browser.tabs().len() > 1,
        }
    }

    pub(crate) fn commands(&self) -> Vec<CommandSpec> {
        all_commands_with_shortcuts(self.command_context(), &self.settings.shortcut_bindings)
    }

    pub(crate) fn visible_commands(&self) -> Vec<CommandSpec> {
        let commands = self.commands();
        if !self.overlay.query.trim().is_empty() {
            return filtered_commands(&self.overlay.query, &commands);
        }
        let mut ordered = Vec::with_capacity(commands.len());
        for recent in &self.settings.recent_commands {
            if let Some(id) = CommandId::from_str(recent)
                && let Some(command) = commands.iter().find(|command| command.id == id)
            {
                ordered.push(command.clone());
            }
        }
        for command in commands {
            if !ordered.iter().any(|recent| recent.id == command.id) {
                ordered.push(command);
            }
        }
        ordered
    }

    pub(crate) fn overlay_is_active(&self) -> bool {
        self.settings_ui.panel_open
            || self.overlay.surface != ControlSurface::Closed
            || self.overlay.toolbar_menu != ToolbarMenu::Closed
            || self.context_menu.menu.is_some()
            || self.settings_ui.confirmation.is_some()
            || self.navigation_ui.go_to_folder.is_some()
            || self.mutation.prompt.is_some()
            || self.quick_look.open
            || !self.operation_ui.conflict_prompts.is_empty()
            || self.mutation.exit_waiting
    }

    pub(crate) fn begin_overlay_focus(&mut self) {
        if !self.overlay_is_active() {
            self.overlay.focus_pending = true;
            self.overlay.restore_pending = false;
        }
    }

    pub(crate) fn finish_overlay_focus_if_inactive(&mut self) {
        if !self.overlay_is_active() && self.overlay.return_focus.is_some() {
            self.overlay.restore_pending = true;
            self.overlay.focus_pending = false;
        }
    }

    pub(crate) fn capture_overlay_focus(&mut self, window: &Window, cx: &App) {
        if self.overlay_is_active() && self.overlay.return_focus.is_none() {
            self.overlay.return_focus = window.focused(cx);
            self.overlay.focus_pending = true;
            self.overlay.restore_pending = false;
        }
    }

    pub(crate) fn sync_overlay_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay_is_active() && self.overlay.focus_pending {
            if self.overlay.return_focus.is_none() {
                self.overlay.return_focus = window.focused(cx);
            }
            self.overlay.focus_pending = false;
            if let Some(menu) = self.context_menu.menu.as_ref() {
                let focus = menu.focus_handle.clone();
                window.defer(cx, move |window, cx| window.focus(&focus, cx));
            } else if self.text_input.target.is_none() {
                window.defer(cx, |window, cx| window.focus_next(cx));
            }
        } else if !self.overlay_is_active()
            && (self.overlay.restore_pending || self.overlay.return_focus.is_some())
        {
            self.overlay.restore_pending = false;
            let focus = self
                .overlay
                .return_focus
                .take()
                .unwrap_or_else(|| self.focus_handle.clone());
            window.defer(cx, move |window, cx| window.focus(&focus, cx));
        }
    }

    pub(crate) fn open_control_surface(&mut self, surface: ControlSurface, cx: &mut Context<Self>) {
        if self.mutation.prompt.is_some() {
            self.show_toast(
                "Finish or dismiss the active file prompt first",
                ToastKind::Warning,
                cx,
            );
            return;
        }
        if (self.overlay.surface != surface && self.has_control_draft())
            || (self.settings_ui.panel_open && self.has_settings_draft())
        {
            self.request_settings_confirmation(
                SettingsConfirmation::DiscardDraft(PendingDraftAction::OpenControlSurface(surface)),
                cx,
            );
            return;
        }
        self.open_control_surface_unchecked(surface, cx);
    }

    pub(crate) fn open_control_surface_unchecked(
        &mut self,
        surface: ControlSurface,
        cx: &mut Context<Self>,
    ) {
        self.begin_overlay_focus();
        self.overlay.surface = surface;
        self.overlay.toolbar_menu = ToolbarMenu::Closed;
        self.overlay.query.clear();
        self.overlay.selected = 0;
        if surface != ControlSurface::Workspaces {
            self.workspace_ui.editing = None;
            self.workspace_ui.delete_confirmation = None;
        }
        if surface != ControlSurface::SmartFolders {
            self.smart_folder_editor = None;
        }
        if surface != ControlSurface::RemoteDrives {
            self.remote.editor = None;
            self.remote.delete_confirmation = None;
            self.remote.blocked_disconnect = None;
        }
        if surface != ControlSurface::BatchRename {
            self.batch_rename = None;
        }
        self.settings_ui.appearance_value_editor = None;
        self.settings_ui.named_theme_editor = None;
        self.settings_ui.shortcut_editor = None;
        self.settings_ui.selector = None;
        self.settings_ui.panel_open = false;
        self.search.active = false;
        cx.notify();
    }

    pub(crate) fn toggle_control_surface(
        &mut self,
        surface: ControlSurface,
        cx: &mut Context<Self>,
    ) {
        if self.overlay.surface == surface {
            self.close_control_surface(cx);
        } else {
            self.open_control_surface(surface, cx);
        }
    }

    pub(crate) fn close_control_surface(&mut self, cx: &mut Context<Self>) {
        if self.has_control_draft() {
            self.request_settings_confirmation(
                SettingsConfirmation::DiscardDraft(PendingDraftAction::CloseControlSurface),
                cx,
            );
            return;
        }
        self.close_control_surface_unchecked(cx);
    }

    pub(crate) fn close_control_surface_unchecked(&mut self, cx: &mut Context<Self>) {
        if self.overlay.surface != ControlSurface::Closed {
            self.overlay.surface = ControlSurface::Closed;
            self.overlay.query.clear();
            self.overlay.selected = 0;
            self.workspace_ui.editing = None;
            self.workspace_ui.delete_confirmation = None;
            self.smart_folder_editor = None;
            self.remote.editor = None;
            self.remote.delete_confirmation = None;
            self.remote.blocked_disconnect = None;
            self.batch_rename = None;
            self.deactivate_native_text_input();
            self.finish_overlay_focus_if_inactive();
            cx.notify();
        }
    }

    pub(crate) fn toggle_toolbar_menu(&mut self, menu: ToolbarMenu, cx: &mut Context<Self>) {
        if self.mutation.prompt.is_some() {
            self.show_toast(
                "Finish or dismiss the active file prompt first",
                ToastKind::Warning,
                cx,
            );
            return;
        }
        if self.overlay.surface != ControlSurface::Closed && self.has_control_draft() {
            self.request_settings_confirmation(
                SettingsConfirmation::DiscardDraft(PendingDraftAction::ToggleToolbarMenu(menu)),
                cx,
            );
            return;
        }
        self.toggle_toolbar_menu_unchecked(menu, cx);
    }

    pub(crate) fn toggle_toolbar_menu_unchecked(
        &mut self,
        menu: ToolbarMenu,
        cx: &mut Context<Self>,
    ) {
        self.begin_overlay_focus();
        self.overlay.toolbar_menu = if self.overlay.toolbar_menu == menu {
            ToolbarMenu::Closed
        } else {
            menu
        };
        if self.overlay.toolbar_menu != ToolbarMenu::Closed {
            self.overlay.surface = ControlSurface::Closed;
            self.overlay.query.clear();
            self.overlay.selected = 0;
            self.settings_ui.panel_open = false;
            self.search.active = false;
            self.context_menu.menu = None;
        }
        self.finish_overlay_focus_if_inactive();
        cx.notify();
    }

    pub(crate) fn close_toolbar_menu(&mut self, cx: &mut Context<Self>) {
        if self.overlay.toolbar_menu != ToolbarMenu::Closed {
            self.overlay.toolbar_menu = ToolbarMenu::Closed;
            self.finish_overlay_focus_if_inactive();
            cx.notify();
        }
    }

    pub(crate) fn remember_command(&mut self, command: CommandId) {
        let id = command.as_str().to_string();
        self.settings.recent_commands.retain(|recent| recent != &id);
        self.settings.recent_commands.insert(0, id);
        self.settings.recent_commands.truncate(5);
        self.persist_settings();
    }

    pub(crate) fn execute_selected_command(&mut self, cx: &mut Context<Self>) {
        let commands = self.visible_commands();
        let Some(command) = commands
            .get(self.overlay.selected)
            .map(|command| command.id)
        else {
            return;
        };
        self.execute_command(command, cx);
    }

    /// Runs a command chosen in the command palette and records it as recent.
    pub(crate) fn execute_command(&mut self, command: CommandId, cx: &mut Context<Self>) {
        self.remember_command(command);
        self.overlay.surface = ControlSurface::Closed;
        self.overlay.query.clear();
        self.overlay.selected = 0;
        self.run_command(command, cx);
    }

    /// The shared execution path for the command palette and the menu bar.
    pub(crate) fn run_command(&mut self, command: CommandId, cx: &mut Context<Self>) {
        match command {
            CommandId::GoBack => self.go_back(cx),
            CommandId::GoForward => self.go_forward(cx),
            CommandId::GoUp => self.go_up(cx),
            CommandId::GoToFolder => self.open_go_to_folder(cx),
            CommandId::ClearHistory => self.clear_navigation_history(cx),
            CommandId::Refresh => self.refresh(cx),
            CommandId::ListView => self.set_view_mode(ViewMode::List, cx),
            CommandId::GridView => self.set_view_mode(ViewMode::Grid, cx),
            CommandId::ColumnView => self.set_view_mode(ViewMode::Column, cx),
            CommandId::ToggleHidden => self.toggle_hidden(cx),
            CommandId::TogglePreview => self.toggle_preview_panel(cx),
            CommandId::ToggleStatus => self.toggle_status_bar(cx),
            CommandId::NewWindow => self.open_new_window(false, cx),
            CommandId::MoveTabToNewWindow => self.open_new_window(true, cx),
            CommandId::NewTab => self.new_tab(cx),
            CommandId::CloseTab => self.close_active_tab(cx),
            CommandId::NewFolder => self.prompt_new_folder(cx),
            CommandId::Rename => self.prompt_rename_selected(cx),
            CommandId::Copy => self.copy_selected(cx),
            CommandId::Cut => self.cut_selected(cx),
            CommandId::Paste => self.paste(cx),
            CommandId::Trash => self.trash_selected(cx),
            CommandId::Undo => self.undo(cx),
            CommandId::Redo => self.redo(cx),
            CommandId::OpenSettings => {
                self.begin_overlay_focus();
                self.settings_ui.panel_open = true;
                cx.notify();
            }
            CommandId::ManageWorkspaces => self.open_workspace_manager(cx),
            CommandId::SaveWorkspace => self.open_workspace_manager(cx),
            CommandId::ManageRemoteDrives => self.open_remote_drive_manager(cx),
            CommandId::ThemeDark => self.set_theme(ThemeMode::Dark, cx),
            CommandId::ThemeLight => self.set_theme(ThemeMode::Light, cx),
            CommandId::ThemeSystem => self.set_theme(ThemeMode::System, cx),
            CommandId::ToggleFavorite => self.toggle_current_favorite(cx),
            CommandId::SaveSmartFolder => self.save_current_search(cx),
            CommandId::ShowShortcuts => self.open_control_surface(ControlSurface::Shortcuts, cx),
            CommandId::ShowDiagnostics => {
                self.open_control_surface(ControlSurface::Diagnostics, cx)
            }
            CommandId::OpenSelected => self.open_selected(cx),
            CommandId::Compress => self.prompt_create_archive(cx),
            CommandId::DeletePermanently => self.prompt_permanent_delete_selected(cx),
            CommandId::Find => {
                self.search.active = true;
                cx.notify();
            }
            CommandId::GoHome => self.go_to_system_location(SystemLocation::Home, cx),
            CommandId::GoDesktop => self.go_to_system_location(SystemLocation::Desktop, cx),
            CommandId::GoDocuments => self.go_to_system_location(SystemLocation::Documents, cx),
            CommandId::GoDownloads => self.go_to_system_location(SystemLocation::Downloads, cx),
            CommandId::ToggleSidebar => {
                self.layout.sidebar_collapsed = !self.layout.sidebar_collapsed;
                cx.notify();
            }
            CommandId::NextTab => self.activate_tab_offset(1, cx),
            CommandId::PreviousTab => self.activate_tab_offset(-1, cx),
            CommandId::About => {
                self.begin_overlay_focus();
                self.settings_ui.tab = SettingsTab::About;
                self.settings_ui.panel_open = true;
                cx.notify();
            }
        }
    }

    pub(crate) fn go_to_system_location(
        &mut self,
        location: SystemLocation,
        cx: &mut Context<Self>,
    ) {
        let known = self.system.locations.as_ref().and_then(|locations| {
            match location {
                SystemLocation::Home => locations.home.as_ref(),
                SystemLocation::Desktop => locations.desktop.as_ref(),
                SystemLocation::Documents => locations.documents.as_ref(),
                SystemLocation::Downloads => locations.downloads.as_ref(),
            }
            .map(PathBuf::from)
        });
        let path = known.or_else(|| match location {
            SystemLocation::Home => dirs::home_dir(),
            SystemLocation::Desktop => dirs::desktop_dir(),
            SystemLocation::Documents => dirs::document_dir(),
            SystemLocation::Downloads => dirs::download_dir(),
        });
        match path {
            Some(path) => self.navigate_to(path, cx),
            None => self.show_toast(
                format!("{} folder is unavailable", location.label()),
                ToastKind::Warning,
                cx,
            ),
        }
    }

    pub(crate) fn move_control_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = match self.overlay.surface {
            ControlSurface::CommandPalette => self.visible_commands().len(),
            _ => 0,
        };
        if count == 0 {
            self.overlay.selected = 0;
            return;
        }
        self.overlay.selected = self
            .overlay
            .selected
            .saturating_add_signed(delta)
            .min(count - 1);
        cx.notify();
    }

    pub(crate) fn has_control_draft(&self) -> bool {
        self.workspace_ui.editing.is_some()
            || self.smart_folder_editor.is_some()
            || self.remote.editor.is_some()
            || self.batch_rename.is_some()
    }

    pub(crate) fn selection_marquee_overlay_rect(&self, layout: MarqueeLayout) -> Option<GridRect> {
        let marquee = self
            .pointer
            .selection_marquee
            .as_ref()
            .filter(|marquee| marquee.activated && marquee.layout == layout)?;
        let (viewport, scroll_top, _) = self.marquee_viewport(layout)?;
        let content = marquee.content_rect();
        let viewport_rect = GridRect {
            left: content.left.clamp(0.0, viewport.width()),
            top: (content.top - scroll_top).clamp(0.0, viewport.height()),
            right: content.right.clamp(0.0, viewport.width()),
            bottom: (content.bottom - scroll_top).clamp(0.0, viewport.height()),
        };
        (viewport_rect.width() > 0.0 && viewport_rect.height() > 0.0).then_some(viewport_rect)
    }

    pub(crate) fn render_selection_marquee_overlay(&self, rect: GridRect) -> AnyElement {
        div()
            .id("selection-marquee-overlay")
            .debug_selector(|| "selection-marquee-overlay".to_string())
            .absolute()
            .left(px(rect.left))
            .top(px(rect.top))
            .w(px(rect.width()))
            .h(px(rect.height()))
            .border_1()
            .border_color(self.palette.accent)
            .bg(with_alpha(self.palette.accent, 0.15))
            .into_any_element()
    }

    pub(crate) fn handle_control_surface_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape"
                if self.overlay.surface == ControlSurface::Workspaces
                    && (self.workspace_ui.editing.is_some()
                        || self.workspace_ui.delete_confirmation.is_some()) =>
            {
                self.workspace_ui.editing = None;
                self.workspace_ui.delete_confirmation = None;
                self.overlay.query.clear();
                cx.notify();
            }
            "escape"
                if self.overlay.surface == ControlSurface::RemoteDrives
                    && (self.remote.editor.is_some()
                        || self.remote.delete_confirmation.is_some()
                        || self.remote.blocked_disconnect.is_some()) =>
            {
                self.remote.editor = None;
                self.remote.delete_confirmation = None;
                self.remote.blocked_disconnect = None;
                self.overlay.query.clear();
                cx.notify();
            }
            "escape" => self.close_control_surface(cx),
            "backspace" if self.overlay.surface == ControlSurface::SmartFolders => {
                let mut value = self.overlay.query.clone();
                value.pop();
                self.update_smart_folder_query(value, cx);
            }
            "backspace" if self.overlay.surface != ControlSurface::Diagnostics => {
                self.overlay.query.pop();
                self.overlay.selected = 0;
                cx.notify();
            }
            "down" if self.overlay.surface == ControlSurface::CommandPalette => {
                self.move_control_selection(1, cx);
            }
            "up" if self.overlay.surface == ControlSurface::CommandPalette => {
                self.move_control_selection(-1, cx);
            }
            "enter" if self.overlay.surface == ControlSurface::CommandPalette => {
                self.execute_selected_command(cx);
            }
            "enter" if self.overlay.surface == ControlSurface::Workspaces => {
                if self.workspace_ui.editing.is_some() {
                    self.commit_workspace_rename(cx);
                } else {
                    self.save_workspace(cx);
                }
            }
            "tab" if self.overlay.surface == ControlSurface::SmartFolders => {
                self.move_smart_folder_field(
                    if event.keystroke.modifiers.shift {
                        -1
                    } else {
                        1
                    },
                    cx,
                );
            }
            "enter"
                if self.overlay.surface == ControlSurface::SmartFolders
                    && (event.keystroke.modifiers.control
                        || event.keystroke.modifiers.platform) =>
            {
                self.commit_smart_folder_editor(cx);
            }
            "enter"
                if self.overlay.surface == ControlSurface::SmartFolders
                    && event.keystroke.modifiers.shift
                    && self
                        .smart_folder_editor
                        .as_ref()
                        .is_some_and(|editor| editor.field == SmartFolderField::SearchPaths) =>
            {
                let mut value = self.overlay.query.clone();
                value.push('\n');
                self.update_smart_folder_query(value, cx);
            }
            "enter" if self.overlay.surface == ControlSurface::SmartFolders => {
                self.move_smart_folder_field(1, cx);
            }
            "enter"
                if self.overlay.surface == ControlSurface::RemoteDrives
                    && self.remote.editor.is_some() =>
            {
                self.commit_remote_editor_field(cx);
            }
            "tab" if self.overlay.surface == ControlSurface::BatchRename => {
                self.toggle_batch_rename_field(cx);
            }
            "enter" if self.overlay.surface == ControlSurface::BatchRename => {
                self.apply_batch_rename(cx);
            }
            _ if self.overlay.surface == ControlSurface::SmartFolders
                && !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(text) = event.keystroke.key_char.as_deref()
                    && !text.chars().any(char::is_control)
                {
                    let mut value = self.overlay.query.clone();
                    value.push_str(text);
                    self.update_smart_folder_query(value, cx);
                }
            }
            _ if self.overlay.surface != ControlSurface::Diagnostics
                && (self.overlay.surface != ControlSurface::RemoteDrives
                    || self.remote.editor.is_some())
                && !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(text) = event.keystroke.key_char.as_deref()
                    && !text.chars().any(char::is_control)
                {
                    self.overlay.query.push_str(text);
                    self.overlay.selected = 0;
                    cx.notify();
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(crate) fn render_control_surface(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette;
        let surface = self.overlay.surface;
        let command_palette_top = (self
            .layout
            .last_window_bounds
            .height
            .unwrap_or(DEFAULT_WINDOW_HEIGHT)
            * 0.15)
            .max(12.0);
        let panel = match surface {
            ControlSurface::Closed => return div().into_any_element(),
            ControlSurface::CommandPalette => {
                let commands = self.visible_commands();
                let control_input = self.native_text_input_element(TextInputTarget::ControlQuery);
                let rows: Vec<AnyElement> = commands
                    .into_iter()
                    .enumerate()
                    .map(|(index, command)| {
                        let id = command.id;
                        let selected = index == self.overlay.selected;
                        div()
                            .id(("command-row", index))
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_3()
                            .py_2()
                            .border_b_1()
                            .border_color(self.palette.border)
                            .bg(if selected {
                                self.palette.selected
                            } else {
                                self.palette.surface
                            })
                            .hover(move |row| row.bg(palette.hover))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.execute_command(id, cx);
                            }))
                            .child(
                                div()
                                    .w(px(100.0))
                                    .text_xs()
                                    .text_color(self.palette.muted)
                                    .child(command.category.label()),
                            )
                            .child(div().flex_1().text_sm().child(command.name))
                            .when_some(command.shortcut, |row, shortcut| {
                                row.child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(self.palette.control)
                                        .text_xs()
                                        .child(shortcut),
                                )
                            })
                            .into_any_element()
                    })
                    .collect();
                div()
                    .id("command-palette")
                    .debug_selector(|| "command-palette".to_string())
                    .role(Role::Dialog)
                    .aria_label("Command palette")
                    // The browser's arrow-key bindings still match inside the
                    // palette's text field; claim them for the result list.
                    .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                        this.move_control_selection(1, cx);
                    }))
                    .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                        this.move_control_selection(-1, cx);
                    }))
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(px(560.0 * self.palette.scale))
                    .max_h(px(520.0))
                    .border_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.panel)
                    .child(div().px_4().py_3().children(control_input))
                    .child(
                        div()
                            .id("command-results")
                            .flex_1()
                            .min_h_0()
                            .overflow_scroll()
                            .when(rows.is_empty(), |list| {
                                list.child(
                                    div()
                                        .px_4()
                                        .py_6()
                                        .text_color(self.palette.muted)
                                        .child("No native commands found"),
                                )
                            })
                            .children(rows),
                    )
                    .child(
                        div()
                            .px_4()
                            .py_2()
                            .border_t_1()
                            .border_color(self.palette.border)
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child("↑/↓ select • Enter run • Esc close"),
                    )
                    .into_any_element()
            }
            ControlSurface::Shortcuts => {
                let query = self.overlay.query.to_lowercase();
                let control_input = self.native_text_input_element(TextInputTarget::ControlQuery);
                let mut groups = Vec::<AnyElement>::new();
                for category in [
                    "Navigation",
                    "File operations",
                    "View",
                    "Tabs",
                    "Settings",
                    "Help",
                ] {
                    let rows: Vec<AnyElement> = EDITABLE_SHORTCUTS
                        .iter()
                        .filter(|shortcut| shortcut.category == category)
                        .filter_map(|shortcut| {
                            let binding =
                                binding_for(&self.settings.shortcut_bindings, shortcut.id)?;
                            let keys = display_binding(&binding);
                            (query.is_empty()
                                || category.to_lowercase().contains(&query)
                                || keys.to_lowercase().contains(&query)
                                || shortcut.label.to_lowercase().contains(&query))
                            .then(|| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .py_1()
                                    .child(div().flex_1().text_sm().child(shortcut.label))
                                    .child(
                                        div()
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .bg(self.palette.control)
                                            .text_xs()
                                            .child(keys),
                                    )
                                    .into_any_element()
                            })
                        })
                        .collect();
                    if !rows.is_empty() {
                        groups.push(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .p_3()
                                .border_1()
                                .border_color(self.palette.border)
                                .bg(self.palette.surface)
                                .child(div().text_sm().child(category))
                                .children(rows)
                                .into_any_element(),
                        );
                    }
                }
                let fixed_rows: Vec<AnyElement> = fixed_browser_bindings()
                    .iter()
                    .filter_map(|(binding, label)| {
                        let keys = display_binding(binding);
                        (query.is_empty()
                            || "native reserved".contains(&query)
                            || keys.to_lowercase().contains(&query)
                            || label.to_lowercase().contains(&query))
                        .then(|| {
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .py_1()
                                .child(div().flex_1().text_sm().child(*label))
                                .child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(self.palette.control)
                                        .text_xs()
                                        .child(keys),
                                )
                                .into_any_element()
                        })
                    })
                    .collect();
                if !fixed_rows.is_empty() {
                    groups.push(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .p_3()
                            .border_1()
                            .border_color(self.palette.border)
                            .bg(self.palette.surface)
                            .child(div().text_sm().child("Native / reserved"))
                            .children(fixed_rows)
                            .into_any_element(),
                    );
                }
                div()
                    .id("shortcuts-overlay")
                    .debug_selector(|| "shortcuts-overlay".to_string())
                    .role(Role::Dialog)
                    .aria_label("Keyboard shortcuts")
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(px(720.0 * self.palette.scale))
                    .max_h(px(540.0))
                    .border_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.panel)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(self.palette.border)
                            .child(div().text_lg().child("Keyboard shortcuts"))
                            .child(div().flex_1().children(control_input))
                            .child(
                                toolbar_button("close-shortcuts", "Close", self.palette.control)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.close_control_surface(cx)
                                        }),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("shortcut-results")
                            .flex()
                            .flex_col()
                            .gap_2()
                            .p_3()
                            .overflow_y_scroll()
                            .when(groups.is_empty(), |list| {
                                list.child(
                                    div()
                                        .py_6()
                                        .text_color(self.palette.muted)
                                        .child("No shortcuts found"),
                                )
                            })
                            .children(groups),
                    )
                    .into_any_element()
            }
            ControlSurface::Diagnostics => {
                let snapshot = self.diagnostics_snapshot();
                let reports = self
                    .error_reports
                    .reports()
                    .take(5)
                    .map(|report| {
                        (
                            report.operation().to_string(),
                            report.category().to_string(),
                            report.message().to_string(),
                        )
                    })
                    .collect::<Vec<_>>();
                div()
                    .id("diagnostics-panel")
                    .debug_selector(|| "diagnostics-panel".to_string())
                    .role(Role::Dialog)
                    .aria_label("Native diagnostics")
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(px(560.0 * self.palette.scale))
                    .max_h(px(590.0))
                    .border_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.panel)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(self.palette.border)
                            .child(div().flex_1().text_lg().child("Native diagnostics"))
                            .child(
                                toolbar_button(
                                    "copy-diagnostics",
                                    "Copy JSON",
                                    self.palette.control,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.copy_diagnostics(cx)
                                })),
                            )
                            .child(
                                toolbar_button(
                                    "copy-error-reports",
                                    "Copy errors",
                                    self.palette.control,
                                )
                                .debug_selector(|| "copy-error-reports".to_string())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.copy_error_reports(cx)
                                })),
                            )
                            .child(
                                toolbar_button(
                                    "clear-error-reports",
                                    "Clear",
                                    self.palette.control,
                                )
                                .debug_selector(|| "clear-error-reports".to_string())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.clear_error_reports(cx)
                                })),
                            )
                            .child(
                                toolbar_button(
                                    "close-diagnostics",
                                    "Close",
                                    self.palette.control,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.close_control_surface(cx)
                                })),
                            ),
                    )
                    .child(
                        div()
                            .id("diagnostics-content")
                            .flex()
                            .flex_col()
                            .gap_2()
                            .p_4()
                            .overflow_y_scroll()
                            .text_sm()
                            .child(format!(
                                "Runtime: GPUI {} • {} / {}",
                                env!("CARGO_PKG_VERSION"),
                                std::env::consts::OS,
                                std::env::consts::ARCH
                            ))
                            .child(format!(
                                "Browser: {} items • {} selected • {} tabs • {} favorites • {} smart folders",
                                snapshot.item_count,
                                snapshot.selected_count,
                                snapshot.tab_count,
                                snapshot.favorite_count,
                                snapshot.smart_folder_count
                            ))
                            .child(format!(
                                "Operations: {} total • {} active • undo {} • redo {}",
                                snapshot.operation_count,
                                snapshot.active_operation_count,
                                on_off(snapshot.undo_available),
                                on_off(snapshot.redo_available)
                            ))
                            .child(format!(
                                "Preview helpers: {} / {} available",
                                snapshot.available_helper_count, snapshot.helper_count
                            ))
                            .child(format!(
                                "Remote drives: {} profiles • {} connected • {} connecting • {} errors • {} retrying • {} exhausted",
                                snapshot.remote_profile_count,
                                snapshot.remote_connected_count,
                                snapshot.remote_connecting_count,
                                snapshot.remote_error_count,
                                snapshot.remote_retrying_count,
                                snapshot.remote_exhausted_count
                            ))
                            .child(format!(
                                "Previous session unclean: {}",
                                if snapshot.previous_session_unclean {
                                    "yes"
                                } else {
                                    "no"
                                }
                            ))
                            .child(
                                div()
                                    .mt_2()
                                    .pt_3()
                                    .border_t_1()
                                    .border_color(self.palette.border)
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child(
                                        format!(
                                            "Error reports: {} / 50 • collection {}",
                                            snapshot.error_report_count,
                                            on_off(snapshot.error_reporting_enabled)
                                        ),
                                    )),
                            )
                            .when(reports.is_empty(), |panel| {
                                panel.child(
                                    div()
                                        .id("error-reports-empty")
                                        .debug_selector(|| "error-reports-empty".to_string())
                                        .py_2()
                                        .text_color(self.palette.muted)
                                        .child(if snapshot.error_reporting_enabled {
                                            "No errors collected this session"
                                        } else {
                                            "Enable Error reporting in Settings to collect local errors"
                                        }),
                                )
                            })
                            .children(reports.into_iter().enumerate().map(
                                |(index, (operation, category, message))| {
                                    div()
                                        .id(ElementId::Name(format!("error-report-{index}").into()))
                                        .debug_selector(move || format!("error-report-{index}"))
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .p_2()
                                        .border_1()
                                        .border_color(self.palette.border)
                                        .bg(self.palette.surface)
                                        .child(
                                            div()
                                                .flex()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(operation),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(self.palette.muted)
                                                        .child(category),
                                                ),
                                        )
                                        .child(div().text_xs().child(message))
                                },
                            ))
                            .child(
                                div()
                                    .mt_2()
                                    .text_xs()
                                    .text_color(self.palette.muted)
                                    .child(
                                        "Diagnostics are generated locally, require no network, and omit path values.",
                                    ),
                            ),
                    )
                    .into_any_element()
            }
            ControlSurface::SmartFolders => self.render_smart_folder_editor(cx),
            ControlSurface::Workspaces => self.render_workspace_manager(cx),
            ControlSurface::RemoteDrives => self.render_remote_drive_manager(cx),
            ControlSurface::BatchRename => self.render_batch_rename(cx),
        };

        div()
            .id("control-surface-backdrop")
            .debug_selector(|| "control-surface-backdrop".to_string())
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .when(surface == ControlSurface::CommandPalette, |backdrop| {
                backdrop.items_start().pt(px(command_palette_top)).px_3()
            })
            .when(surface != ControlSurface::CommandPalette, |backdrop| {
                backdrop.items_center().p_3()
            })
            .bg(with_alpha(rgb(0x000000), 0.58))
            .child(panel)
            .into_any_element()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemLocation {
    Home,
    Desktop,
    Documents,
    Downloads,
}

impl SystemLocation {
    fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Desktop => "Desktop",
            Self::Documents => "Documents",
            Self::Downloads => "Downloads",
        }
    }
}
