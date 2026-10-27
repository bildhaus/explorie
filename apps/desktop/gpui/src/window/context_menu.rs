//! `DirectoryWindow` behavior for context menu.

use crate::*;

/// The file context menu on screen and the task that fills in its
/// asynchronous entries (such as Open With applications).
#[derive(Default)]
pub(crate) struct ContextMenuUi {
    pub(crate) menu: Option<FileContextMenu>,
    pub(crate) task: Option<Task<()>>,
    pub(crate) generation: u64,
}

impl DirectoryWindow {
    pub(crate) fn context_menu_actions(&self) -> Vec<(ContextMenuAction, bool)> {
        let Some(menu) = self.context_menu.menu.as_ref() else {
            return Vec::new();
        };
        let count = menu.paths.len();
        if count == 0 {
            return self
                .paste_count()
                .map(|_| vec![(ContextMenuAction::Paste, false)])
                .unwrap_or_default();
        }

        let mut actions = Vec::new();
        if count == 1 {
            actions.push((ContextMenuAction::Open, false));
            if menu.target_is_package {
                actions.push((ContextMenuAction::ShowPackageContents, false));
            } else if !menu.target_is_folder {
                actions.push((ContextMenuAction::Preview, false));
            }
            actions.push((ContextMenuAction::Rename, false));
        } else {
            actions.push((ContextMenuAction::BatchRename, false));
        }
        actions.push((ContextMenuAction::Copy, false));
        actions.push((ContextMenuAction::Cut, false));
        actions.push((ContextMenuAction::Trash, false));

        if count == 1 {
            actions.push((ContextMenuAction::Reveal, true));
            if !menu.target_is_folder && cfg!(target_os = "macos") {
                // LaunchServices' handlers (default first), then Other….
                actions.push((ContextMenuAction::ToggleOpenWith, false));
                if menu.open_with_expanded {
                    actions.extend(
                        menu.open_with_apps
                            .iter()
                            .map(|app| (ContextMenuAction::OpenWithApp(app.clone()), false)),
                    );
                    actions.push((ContextMenuAction::OpenWithOther, false));
                }
            } else if !menu.target_is_folder && cfg!(windows) {
                actions.push((ContextMenuAction::OpenWithChooser, false));
            }
            if menu.target_is_folder {
                actions.push((ContextMenuAction::ToggleFavorite, true));
            }
        }

        actions.push((ContextMenuAction::Compress, true));
        if count == 1
            && !menu.target_is_folder
            && !menu.target_is_package
            && menu.paths.first().is_some_and(|path| {
                matches!(preview_panel::route(path, true), PreviewRoute::Archive)
            })
        {
            actions.push((ContextMenuAction::InspectArchive, false));
            actions.push((ContextMenuAction::ExtractArchive, false));
        }
        actions
    }

    pub(crate) fn open_file_context_menu(
        &mut self,
        path: PathBuf,
        _is_dir: bool,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        if !self.is_effectively_selected(&path) {
            if self.browser.view_mode() == ViewMode::Column {
                self.set_column_selection(path.clone());
                if self
                    .browser
                    .visible_entries()
                    .iter()
                    .any(|entry| entry.path == path)
                {
                    self.browser.select(path.clone());
                } else {
                    self.browser.clear_selection();
                }
            } else {
                self.browser.select(path.clone());
            }
            self.sync_pinned_preview(cx);
        }
        let entries = self.effective_selected_entries();
        if entries.is_empty() {
            return;
        }
        self.open_context_menu_for_entries(entries, position, cx);
    }

    pub(crate) fn open_context_menu_for_entries(
        &mut self,
        entries: Vec<FileEntry>,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let paths: Vec<_> = entries.iter().map(|entry| entry.path.clone()).collect();
        let Some(first) = entries.first() else {
            return;
        };
        let is_dir = is_folder_like(first);
        let is_package = first.is_package;
        let path = first.path.clone();
        self.context_menu.generation = self.context_menu.generation.wrapping_add(1);
        let generation = self.context_menu.generation;
        self.begin_overlay_focus();
        self.context_menu.menu = Some(FileContextMenu {
            focus_handle: cx.focus_handle(),
            position,
            paths: paths.clone(),
            entries,
            target_is_folder: is_dir,
            target_is_package: is_package,
            focused: 0,
            open_with_apps: Vec::new(),
            open_with_expanded: false,
        });
        self.overlay.focus_pending = true;
        self.context_menu.task = None;

        if cfg!(target_os = "macos") && paths.len() == 1 && !is_dir {
            let task = self.services.integration.apps_for_file(path.clone());
            self.context_menu.task = Some(cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |view, cx| {
                    if view.context_menu.generation != generation
                        || view
                            .context_menu
                            .menu
                            .as_ref()
                            .map(|menu| menu.paths.as_slice())
                            != Some(std::slice::from_ref(&path))
                    {
                        return;
                    }
                    if let Ok(apps) = result
                        && let Some(menu) = view.context_menu.menu.as_mut()
                    {
                        menu.open_with_apps = apps;
                    }
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }

    pub(crate) fn open_empty_context_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        // Files may have been copied in Finder or another window since the
        // clipboard was last read.
        self.refresh_system_clipboard();
        if self.paste_count().is_none() {
            self.close_context_menu(cx);
            return;
        }
        self.browser.clear_selection();
        self.context_menu.generation = self.context_menu.generation.wrapping_add(1);
        self.context_menu.task = None;
        self.begin_overlay_focus();
        self.context_menu.menu = Some(FileContextMenu {
            focus_handle: cx.focus_handle(),
            position,
            paths: Vec::new(),
            entries: Vec::new(),
            target_is_folder: false,
            target_is_package: false,
            focused: 0,
            open_with_apps: Vec::new(),
            open_with_expanded: false,
        });
        self.overlay.focus_pending = true;
        cx.notify();
    }

    pub(crate) fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.menu.take().is_some() {
            self.context_menu.generation = self.context_menu.generation.wrapping_add(1);
            self.context_menu.task = None;
            self.finish_overlay_focus_if_inactive();
            cx.notify();
        }
    }

    pub(crate) fn execute_context_menu_action(
        &mut self,
        action: ContextMenuAction,
        cx: &mut Context<Self>,
    ) {
        if action == ContextMenuAction::ToggleOpenWith {
            if let Some(menu) = self.context_menu.menu.as_mut() {
                menu.open_with_expanded = !menu.open_with_expanded;
            }
            let max_index = self.context_menu_actions().len().saturating_sub(1);
            if let Some(menu) = self.context_menu.menu.as_mut() {
                menu.focused = menu.focused.min(max_index);
            }
            cx.notify();
            return;
        }
        let Some(menu) = self.context_menu.menu.clone() else {
            return;
        };
        let target = menu.paths.first().cloned();
        self.close_context_menu(cx);
        match action {
            ContextMenuAction::Open => {
                if let Some(path) = target {
                    self.open_entry(path, menu.target_is_folder, cx);
                }
            }
            ContextMenuAction::ShowPackageContents => {
                if let Some(path) = target {
                    self.navigate_to(path, cx);
                }
            }
            ContextMenuAction::Preview => {
                if let Some(path) = target {
                    let paths = self.preview_files_for(&path);
                    self.open_quick_look(path, paths, cx);
                }
            }
            ContextMenuAction::Rename => {
                if let Some(path) = target {
                    self.prompt_rename_path(path, cx);
                }
            }
            ContextMenuAction::BatchRename => {
                self.open_batch_rename_entries(menu.entries.clone(), cx)
            }
            ContextMenuAction::Copy => {
                self.set_clipboard_paths(ClipboardKind::Copy, menu.paths.clone(), cx)
            }
            ContextMenuAction::Cut => {
                self.set_clipboard_paths(ClipboardKind::Cut, menu.paths.clone(), cx)
            }
            ContextMenuAction::Trash => self.trash_paths(menu.paths.clone(), cx),
            ContextMenuAction::Reveal => {
                if let Some(path) = target {
                    self.reveal_item(path, cx);
                }
            }
            ContextMenuAction::OpenWithChooser => {
                if let Some(path) = target {
                    self.open_item_with(path, String::new(), cx);
                }
            }
            ContextMenuAction::OpenWithApp(app) => {
                if let Some(path) = target {
                    // The path picks the exact copy LaunchServices listed.
                    let app = if app.path.as_os_str().is_empty() {
                        app.name
                    } else {
                        app.path.to_string_lossy().into_owned()
                    };
                    self.open_item_with(path, app, cx);
                }
            }
            ContextMenuAction::OpenWithOther => {
                if let Some(path) = target {
                    self.open_item_with_chosen_app(path, cx);
                }
            }
            ContextMenuAction::ToggleFavorite => {
                if let Some(path) = target {
                    self.mutate_shared_session(|session| session.toggle_favorite(path));
                    self.persist_session();
                    cx.notify();
                }
            }
            ContextMenuAction::Compress => {
                self.prompt_create_archive_from_paths(menu.paths.clone(), cx)
            }
            ContextMenuAction::InspectArchive => {
                if let Some(path) = target {
                    self.inspect_archive(path, cx);
                }
            }
            ContextMenuAction::ExtractArchive => {
                if let Some(path) = target {
                    self.prompt_extract_archive_path(path, cx);
                }
            }
            ContextMenuAction::Paste => self.paste(cx),
            ContextMenuAction::ToggleOpenWith => unreachable!("handled before menu close"),
        }
    }

    pub(crate) fn handle_context_menu_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let actions = self.context_menu_actions();
        let mut handled = true;
        match event.keystroke.key.as_str() {
            "escape" => self.close_context_menu(cx),
            "home" => {
                if let Some(menu) = self.context_menu.menu.as_mut() {
                    menu.focused = 0;
                    cx.notify();
                }
            }
            "end" => {
                if let Some(menu) = self.context_menu.menu.as_mut() {
                    menu.focused = actions.len().saturating_sub(1);
                    cx.notify();
                }
            }
            "up" => {
                let Some(menu) = self.context_menu.menu.as_mut() else {
                    return;
                };
                menu.focused = menu
                    .focused
                    .checked_sub(1)
                    .unwrap_or(actions.len().saturating_sub(1));
                cx.notify();
            }
            "tab" if event.keystroke.modifiers.shift => {
                let Some(menu) = self.context_menu.menu.as_mut() else {
                    return;
                };
                menu.focused = menu
                    .focused
                    .checked_sub(1)
                    .unwrap_or(actions.len().saturating_sub(1));
                cx.notify();
            }
            "down" | "tab" => {
                let Some(menu) = self.context_menu.menu.as_mut() else {
                    return;
                };
                menu.focused = (menu.focused + 1) % actions.len().max(1);
                cx.notify();
            }
            "right" => {
                let Some(menu) = self.context_menu.menu.as_ref() else {
                    return;
                };
                if !menu.open_with_expanded
                    && actions.get(menu.focused).map(|(action, _)| action)
                        == Some(&ContextMenuAction::ToggleOpenWith)
                {
                    self.execute_context_menu_action(ContextMenuAction::ToggleOpenWith, cx);
                }
            }
            "left" => {
                let Some(menu) = self.context_menu.menu.as_ref() else {
                    return;
                };
                if menu.open_with_expanded {
                    self.execute_context_menu_action(ContextMenuAction::ToggleOpenWith, cx);
                }
            }
            "enter" => {
                let focused = self
                    .context_menu
                    .menu
                    .as_ref()
                    .map(|menu| menu.focused)
                    .unwrap_or(0);
                if let Some((action, _)) = actions.get(focused).cloned() {
                    self.execute_context_menu_action(action, cx);
                }
            }
            key if key.chars().count() == 1
                && !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                let Some(menu) = self.context_menu.menu.as_ref() else {
                    return;
                };
                let start = (menu.focused + 1) % actions.len().max(1);
                let expanded = menu.open_with_expanded;
                let count = menu.paths.len().max(1);
                let favorite = menu
                    .paths
                    .first()
                    .is_some_and(|path| self.browser.is_favorite(path));
                let needle = key.to_lowercase();
                if let Some(index) = (0..actions.len())
                    .map(|offset| (start + offset) % actions.len())
                    .find(|index| {
                        actions[*index]
                            .0
                            .label(count, favorite, expanded)
                            .to_lowercase()
                            .starts_with(&needle)
                    })
                    && let Some(menu) = self.context_menu.menu.as_mut()
                {
                    menu.focused = index;
                    cx.notify();
                }
            }
            _ => handled = false,
        }
        if handled {
            cx.stop_propagation();
        }
    }

    pub(crate) fn render_file_context_menu(
        &mut self,
        window_size: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(menu) = self.context_menu.menu.clone() else {
            return div().into_any_element();
        };
        let actions = self.context_menu_actions();
        if actions.is_empty() {
            return div().into_any_element();
        }
        let selection_count = menu.paths.len();
        let label_count = if selection_count == 0 {
            self.paste_count().unwrap_or(0)
        } else {
            selection_count
        };
        let target = menu.paths.first();
        let favorite = target.is_some_and(|path| self.browser.is_favorite(path));
        let scale = self.palette.scale;
        let width = 252.0 * scale;
        let margin = 8.0 * scale;
        let estimated_height = actions.len() as f32 * 34.0 * scale
            + actions.iter().filter(|(_, separator)| *separator).count() as f32 * 6.0 * scale
            + 12.0 * scale;
        let window_width = f32::from(window_size.width);
        let window_height = f32::from(window_size.height);
        let max_x = (window_width - width - margin).max(margin);
        let max_y = (window_height - estimated_height.min(window_height - margin * 2.0) - margin)
            .max(margin);
        let left = f32::from(menu.position.x).clamp(margin, max_x);
        let top = f32::from(menu.position.y).clamp(margin, max_y);
        let menu_focus = menu.focus_handle.clone();

        let rows: Vec<AnyElement> = actions
            .into_iter()
            .enumerate()
            .map(|(index, (action, separator))| {
                let selected = index == menu.focused;
                let selector = action.selector();
                let debug_selector = selector.clone();
                let click_action = action.clone();
                let label = action.label(label_count, favorite, menu.open_with_expanded);
                let shortcut = action.shortcut(&self.settings.shortcut_bindings);
                div()
                    .id(ElementId::Name(selector.into()))
                    .debug_selector(move || debug_selector.clone())
                    .role(Role::MenuItem)
                    .aria_label(label.clone())
                    .aria_selected(selected)
                    .when(separator, |row| {
                        row.mt_1()
                            .pt_1()
                            .border_t_1()
                            .border_color(self.palette.border)
                    })
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_h(px(32.0 * scale))
                    .px_2()
                    .rounded_sm()
                    .bg(if selected {
                        self.palette.selected
                    } else {
                        self.palette.panel
                    })
                    .hover(|row| row.bg(self.palette.hover))
                    .cursor_pointer()
                    .child(
                        div()
                            .w(px(20.0 * scale))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(toolbar_icon(
                                action.icon_name(),
                                self.palette.icon_size.clamp(13.0, 17.0),
                                self.palette.muted,
                            )),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(label))
                    .when_some(shortcut, |row, shortcut| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(self.palette.muted)
                                .child(shortcut),
                        )
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.execute_context_menu_action(click_action.clone(), cx);
                    }))
                    .into_any_element()
            })
            .collect();

        div()
            .id("file-context-menu-backdrop")
            .debug_selector(|| "file-context-menu-backdrop".to_string())
            .absolute()
            .inset_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_context_menu(cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| this.close_context_menu(cx)),
            )
            .child(
                div()
                    .id("file-context-menu")
                    .debug_selector(|| "file-context-menu".to_string())
                    .role(Role::Menu)
                    .key_context("file-context-menu")
                    .track_focus(&menu_focus)
                    .focusable()
                    .tab_stop(true)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        this.handle_context_menu_key(event, cx)
                    }))
                    .aria_label(if selection_count == 0 {
                        "Folder actions".to_string()
                    } else if selection_count == 1 {
                        "File actions".to_string()
                    } else {
                        format!("Actions for {selection_count} selected items")
                    })
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(width))
                    .max_h(px((window_height - margin * 2.0).max(80.0 * scale)))
                    .overflow_y_scroll()
                    .p_1()
                    .rounded_md()
                    .border_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.panel)
                    .shadow_lg()
                    .children(rows),
            )
            .into_any_element()
    }
}
