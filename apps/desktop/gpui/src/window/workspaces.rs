//! `DirectoryWindow` behavior for workspaces.

use crate::*;

/// The workspace manager: the workspace being renamed or confirmed for
/// deletion, and a workspace load in flight.
#[derive(Default)]
pub(crate) struct WorkspaceUi {
    pub(crate) editing: Option<String>,
    pub(crate) delete_confirmation: Option<String>,
    pub(crate) load_task: Option<Task<()>>,
}

impl DirectoryWindow {
    pub(crate) fn persist_workspaces(&mut self) {
        let error = self
            .workspace_store
            .as_ref()
            .and_then(|store| store.save(&self.workspaces).err());
        if let Some(error) = error {
            self.record_error("Workspace save failed", &error);
            self.status_message = Some(format!("Unable to save workspaces: {error}"));
        }
    }

    pub(crate) fn mutate_workspaces<R>(
        &mut self,
        mutation: impl FnOnce(&mut WorkspaceState) -> R,
    ) -> R {
        let (result, state) = if let Some(runtime) = self
            .window_lifetime
            .as_ref()
            .map(|lifetime| lifetime.runtime.clone())
        {
            runtime.mutate_workspaces(&mut self.shared_state_revision, mutation)
        } else {
            let mut state = self.workspaces.clone();
            let result = mutation(&mut state);
            (result, state)
        };
        self.workspaces = state;
        result
    }

    pub(crate) fn workspace_snapshot(&self) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            tabs: self
                .browser
                .workspace_tabs()
                .into_iter()
                .map(|(id, path)| WorkspaceTab { id, path })
                .collect(),
            active_tab_id: self.browser.active_tab_id().value().to_string(),
            view_mode: self.browser.view_mode(),
            sort_key: self.browser.sort_key(),
            sort_direction: self.browser.sort_direction(),
            show_hidden: self.browser.show_hidden(),
            filter_mode: self.browser.filter(),
            show_preview_panel: self.settings.view.show_preview_panel,
            grid_min_width: self.settings.appearance.grid_min_width,
            window: self.layout.last_window_bounds,
            sidebar_width: self.layout.sidebar_width,
            preview_panel_width: self.layout.preview_panel_width,
            sidebar_collapsed: self.layout.sidebar_collapsed,
        }
    }

    pub(crate) fn open_workspace_manager(&mut self, cx: &mut Context<Self>) {
        self.workspace_ui.editing = None;
        self.workspace_ui.delete_confirmation = None;
        self.open_control_surface(ControlSurface::Workspaces, cx);
    }

    pub(crate) fn save_workspace(&mut self, cx: &mut Context<Self>) {
        let name = self.overlay.query.clone();
        let snapshot = self.workspace_snapshot();
        match self.mutate_workspaces(|workspaces| workspaces.save_current(&name, snapshot)) {
            Ok(_) => {
                self.persist_workspaces();
                self.overlay.query.clear();
                self.show_toast("Workspace saved", ToastKind::Success, cx);
            }
            Err(error) => self.show_toast(error, ToastKind::Warning, cx),
        }
    }

    pub(crate) fn begin_workspace_rename(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspaces.get(&id) else {
            return;
        };
        self.overlay.query.clone_from(&workspace.name);
        self.workspace_ui.editing = Some(id);
        self.workspace_ui.delete_confirmation = None;
        self.sync_native_text_input(self.overlay.query.clone(), cx);
        cx.notify();
    }

    pub(crate) fn commit_workspace_rename(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.workspace_ui.editing.clone() else {
            return;
        };
        let name = self.overlay.query.clone();
        match self.mutate_workspaces(|workspaces| workspaces.rename(&id, &name)) {
            Ok(()) => {
                self.workspace_ui.editing = None;
                self.overlay.query.clear();
                self.persist_workspaces();
                self.show_toast("Workspace renamed", ToastKind::Success, cx);
            }
            Err(error) => self.show_toast(error, ToastKind::Warning, cx),
        }
    }

    pub(crate) fn request_workspace_delete(&mut self, id: String, cx: &mut Context<Self>) {
        self.workspace_ui.delete_confirmation = Some(id);
        self.workspace_ui.editing = None;
        cx.notify();
    }

    pub(crate) fn confirm_workspace_delete(&mut self, id: String, cx: &mut Context<Self>) {
        if self.mutate_workspaces(|workspaces| workspaces.delete(&id)) {
            self.persist_workspaces();
            self.workspace_ui.delete_confirmation = None;
            self.show_toast("Workspace deleted", ToastKind::Success, cx);
        }
    }

    /// Load a saved workspace. Its folders are checked off the UI thread, as
    /// an offline network volume can block the check for a long time.
    pub(crate) fn load_workspace(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspaces.get(&id).cloned() else {
            self.show_toast("Workspace no longer exists", ToastKind::Warning, cx);
            return;
        };
        let paths: Vec<PathBuf> = workspace.tabs.iter().map(|tab| tab.path.clone()).collect();
        let available = cx.background_spawn(async move {
            paths.iter().map(|path| path.is_dir()).collect::<Vec<_>>()
        });
        self.workspace_ui.load_task = Some(cx.spawn(async move |this, cx| {
            let available = available.await;
            let _ = this.update(cx, |view, cx| {
                view.workspace_ui.load_task = None;
                view.apply_loaded_workspace(id, workspace, &available, cx);
            });
        }));
    }

    fn apply_loaded_workspace(
        &mut self,
        id: String,
        workspace: crate::workspace::Workspace,
        available: &[bool],
        cx: &mut Context<Self>,
    ) {
        let total = workspace.tabs.len();
        let tabs: Vec<_> = workspace
            .tabs
            .iter()
            .zip(available)
            .filter(|(_, available)| **available)
            .map(|(tab, _)| (tab.id.clone(), tab.path.clone()))
            .collect();
        let skipped = total.saturating_sub(tabs.len());
        if tabs.is_empty() {
            self.show_toast(
                "None of this workspace's folders are currently available",
                ToastKind::Warning,
                cx,
            );
            return;
        }
        if let Err(error) = self
            .browser
            .replace_workspace_tabs(tabs, &workspace.active_tab_id)
        {
            self.show_toast(error, ToastKind::Warning, cx);
            return;
        }
        self.settings.view.view_mode = workspace.view_mode;
        self.settings.view.show_hidden = workspace.show_hidden;
        self.settings.view.filter_mode = workspace.filter_mode;
        self.settings.view.sort_key = workspace.sort_key;
        self.settings.view.sort_direction = workspace.sort_direction;
        self.settings.view.show_preview_panel = workspace.show_preview_panel;
        self.settings.appearance.grid_min_width = workspace.grid_min_width;
        self.layout.sidebar_width = workspace.sidebar_width;
        self.layout.preview_panel_width = workspace.preview_panel_width;
        self.layout.sidebar_collapsed = workspace.sidebar_collapsed;
        self.settings.view.sidebar_width = self.layout.sidebar_width;
        self.settings.view.preview_panel_width = self.layout.preview_panel_width;
        self.layout.pending_workspace_bounds = Some(workspace.window);
        self.apply_global_browser_preferences();
        self.column_view.columns.reset(self.browser.path());
        self.close_preview(cx);
        self.mutate_workspaces(|workspaces| workspaces.mark_loaded(&id));
        self.persist_session();
        self.persist_settings();
        self.persist_workspaces();
        self.overlay.surface = ControlSurface::Closed;
        if self.browser.view_mode() == ViewMode::Column {
            self.start_column_listings(true, cx);
        } else {
            self.start_listing_with_scroll_reset(true, cx);
        }
        self.start_watching(cx);
        let message = if skipped == 0 {
            format!("Loaded workspace {}", workspace.name)
        } else {
            format!(
                "Loaded workspace {} • skipped {skipped} unavailable folder(s)",
                workspace.name
            )
        };
        self.status_message = Some(message);
        cx.notify();
    }

    pub(crate) fn copy_workspaces_json(&mut self, cx: &mut Context<Self>) {
        match self.workspaces.export_json() {
            Ok(json) => {
                cx.write_to_clipboard(ClipboardItem::new_string(json));
                self.show_toast("Workspace JSON copied", ToastKind::Success, cx);
            }
            Err(error) => self.show_toast(error, ToastKind::Warning, cx),
        }
    }

    pub(crate) fn import_workspaces_from_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(json) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.show_toast("Clipboard does not contain text", ToastKind::Warning, cx);
            return;
        };
        match self.mutate_workspaces(|workspaces| workspaces.import_json(&json)) {
            Ok(count) => {
                self.persist_workspaces();
                self.show_toast(
                    format!("Imported {count} workspace(s)"),
                    ToastKind::Success,
                    cx,
                );
            }
            Err(error) => self.show_toast(error, ToastKind::Warning, cx),
        }
    }

    pub(crate) fn render_workspace_manager(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let editing = self.workspace_ui.editing.clone();
        let deleting = self.workspace_ui.delete_confirmation.clone();
        let last_loaded = self.workspaces.last_workspace_id().map(str::to_string);
        let workspaces = self.workspaces.workspaces().to_vec();
        let rows: Vec<AnyElement> = workspaces
            .into_iter()
            .enumerate()
            .map(|(index, workspace)| {
                let id = workspace.id.clone();
                let load_id = id.clone();
                let rename_id = id.clone();
                let delete_id = id.clone();
                let confirm_id = id.clone();
                let is_deleting = deleting.as_deref() == Some(id.as_str());
                let is_editing = editing.as_deref() == Some(id.as_str());
                let is_last = last_loaded.as_deref() == Some(id.as_str());
                div()
                    .id(("workspace-row", index))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .bg(if is_editing {
                        self.palette.selected
                    } else {
                        self.palette.surface
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_sm().child(if is_last {
                                format!("{} • last loaded", workspace.name)
                            } else {
                                workspace.name.clone()
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(self.palette.muted)
                                    .child(format!(
                                        "{} tab(s) • updated {}",
                                        workspace.tabs.len(),
                                        workspace.updated_at
                                    )),
                            ),
                    )
                    .when(!is_deleting, |row| {
                        row.child(
                            toolbar_button(("load-workspace", index), "Load", self.palette.control)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.load_workspace(load_id.clone(), cx)
                                })),
                        )
                        .child(
                            toolbar_button(
                                ("rename-workspace", index),
                                "Rename",
                                self.palette.control,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.begin_workspace_rename(rename_id.clone(), cx)
                                },
                            )),
                        )
                        .child(
                            toolbar_button(
                                ("delete-workspace", index),
                                "Delete",
                                self.palette.control,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.request_workspace_delete(delete_id.clone(), cx)
                                },
                            )),
                        )
                    })
                    .when(is_deleting, |row| {
                        row.child(div().text_xs().child("Delete this snapshot?"))
                            .child(
                                toolbar_button(
                                    ("confirm-delete-workspace", index),
                                    "Delete",
                                    rgb(0x8b3340),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.confirm_workspace_delete(confirm_id.clone(), cx)
                                    },
                                )),
                            )
                            .child(
                                toolbar_button(
                                    ("cancel-delete-workspace", index),
                                    "Cancel",
                                    self.palette.control,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.workspace_ui.delete_confirmation = None;
                                        cx.notify();
                                    },
                                )),
                            )
                    })
                    .into_any_element()
            })
            .collect();
        let control_input = self.native_text_input_element(TextInputTarget::ControlQuery);
        div()
            .id("workspace-manager")
            .debug_selector(|| "workspace-manager".to_string())
            .role(Role::Dialog)
            .aria_label("Workspaces")
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(500.0 * self.palette.scale))
            .max_h(px(560.0))
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
                    .child(div().text_lg().child("Workspaces"))
                    .child(div().flex_1())
                    .child(
                        toolbar_button("close-workspaces", "Close", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| this.close_control_surface(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(self.palette.border)
                    .child(div().flex_1().children(control_input))
                    .child(
                        toolbar_button(
                            "commit-workspace-name",
                            if editing.is_some() {
                                "Rename"
                            } else {
                                "Save current"
                            },
                            self.palette.control,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.workspace_ui.editing.is_some() {
                                this.commit_workspace_rename(cx);
                            } else {
                                this.save_workspace(cx);
                            }
                        })),
                    ),
            )
            .child(
                div()
                    .id("workspace-results")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(rows.is_empty(), |list| {
                        list.child(
                            div()
                                .px_4()
                                .py_6()
                                .text_color(self.palette.muted)
                                .child("No saved workspaces"),
                        )
                    })
                    .children(rows),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(self.palette.border)
                    .child(
                        toolbar_button(
                            "import-workspaces",
                            "Import clipboard",
                            self.palette.control,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.import_workspaces_from_clipboard(cx)),
                        ),
                    )
                    .child(
                        toolbar_button("export-workspaces", "Copy all JSON", self.palette.control)
                            .on_click(cx.listener(|this, _, _, cx| this.copy_workspaces_json(cx))),
                    )
                    .child(
                        div()
                            .w_full()
                            .pt_1()
                            .whitespace_nowrap()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child("Type a name • Enter save/rename • Esc cancel"),
                    ),
            )
            .into_any_element()
    }
}
