//! Top-level `Render` implementation for `DirectoryWindow`.

use crate::*;

impl Render for DirectoryWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        let render_started = Instant::now();
        self.release_retired_frames(window);
        if !self.plugin_ui.activation_observed {
            self.plugin_ui.activation_observed = true;
            cx.observe_window_activation(window, |view, window, cx| {
                if window.is_window_active() {
                    view.start_plugin_scan(true, cx);
                    // Another app may have changed the clipboard meanwhile.
                    view.refresh_system_clipboard();
                    cx.notify();
                }
            })
            .detach();
            // A clicked button takes the focus; when it goes away with its
            // panel or prompt, nothing is focused and every shortcut stops
            // working until the list is clicked. Hand the focus back.
            cx.on_focus_lost(window, |view, window, cx| {
                window.focus(&view.focus_handle, cx);
            })
            .detach();
        }
        window.set_rem_size(px(16.0 * self.settings.appearance.ui_scale));
        if let Some(saved) = self.layout.pending_workspace_bounds.take() {
            let current = window.bounds();
            let displays: Vec<_> = cx
                .displays()
                .into_iter()
                .map(|display| display.bounds())
                .collect();
            let restored = constrain_workspace_bounds(saved, current, &displays);
            window.resize(restored.size);
            if let Err(error) = move_native_window(window, restored.origin) {
                self.status_message =
                    Some(format!("Workspace position could not be restored: {error}"));
            }
        }
        let bounds = window.bounds();
        self.layout.last_window_bounds = WorkspaceWindowState {
            width: Some(f32::from(bounds.size.width)),
            height: Some(f32::from(bounds.size.height)),
            x: Some(f32::from(bounds.origin.x)),
            y: Some(f32::from(bounds.origin.y)),
        };
        self.palette = UiPalette::for_settings(&self.settings, window.appearance());
        self.capture_overlay_focus(window, cx);
        self.ensure_native_text_input(cx);
        let removed_focus = self.text_input.removed_focus.take();
        if self.text_input.focus_pending
            && let Some(input) = self.text_input.entity.as_ref()
        {
            window.focus(&input.focus_handle(cx), cx);
            self.text_input.focus_pending = false;
        } else if removed_focus.is_some_and(|focus| focus.is_focused(window)) {
            // The removed text field had the focus; typing and type-to-select
            // must keep working without clicking the list first.
            window.focus(&self.focus_handle, cx);
        }
        self.sync_overlay_focus(window, cx);
        let selection_count = self.effective_selection_count();
        let stats = self.browser.entry_stats();
        let (folder_count, file_count) = (stats.folders, stats.files);
        let total_file_size = stats.file_bytes;
        let hidden_count = stats.hidden;
        let item_summary = if folder_count == 0 && file_count == 0 {
            "Empty folder".to_string()
        } else {
            let mut parts = Vec::with_capacity(2);
            if folder_count > 0 {
                parts.push(format!(
                    "{folder_count} folder{}",
                    if folder_count == 1 { "" } else { "s" }
                ));
            }
            if file_count > 0 {
                parts.push(format!(
                    "{file_count} file{}",
                    if file_count == 1 { "" } else { "s" }
                ));
            }
            parts.join(", ")
        };
        let total_size_summary = (total_file_size > 0).then(|| format_size(total_file_size));
        let mut filter_parts = Vec::with_capacity(3);
        if self.browser.filter() != EntryFilter::All {
            filter_parts.push(format!(
                "Showing {}",
                self.browser.filter().label().to_lowercase()
            ));
        }
        if !self.browser.search_query().is_empty() {
            filter_parts.push(format!("Searching \"{}\"", self.browser.search_query()));
        }
        if !self.browser.show_hidden() && hidden_count > 0 {
            filter_parts.push(format!("{hidden_count} hidden"));
        }
        let filter_summary = (!filter_parts.is_empty()).then(|| filter_parts.join(" / "));
        let selection_summary = if selection_count > 1 {
            Some(format!("{selection_count} selected"))
        } else {
            self.effective_selected_entry().map(|entry| {
                let name = file_name(&entry);
                // Folders (and links to them) have no size to show; packages
                // do once folder sizes have been calculated.
                let known_size = !is_directory_entry(&entry)
                    || (entry.is_package && entry.is_dir && entry.size > 0);
                if !known_size {
                    format!("Selected: {name}")
                } else {
                    format!("Selected: {name} ({})", format_size(entry.size))
                }
            })
        };
        let active_operation_count = self.process_active_operation_count();
        let operation_summary = if active_operation_count > 1 {
            Some((
                format!("{active_operation_count} operations"),
                format!("{active_operation_count} file operations in progress"),
            ))
        } else if active_operation_count == 1 {
            self.operations
                .operations()
                .iter()
                .rev()
                .find(|operation| operation.status() == OperationStatus::Running)
                .map(|operation| {
                    let kind = match operation.request().kind {
                        FileOperationKind::Copy => "Copying",
                        FileOperationKind::Move => "Moving",
                        FileOperationKind::Trash => "Deleting",
                    };
                    let progress = operation.progress();
                    let percent = progress.and_then(|progress| {
                        let (processed, total) = if progress.total_bytes > 0 {
                            (progress.processed_bytes, progress.total_bytes)
                        } else {
                            (progress.processed_entries, progress.total_entries)
                        };
                        (total > 0).then(|| processed.saturating_mul(100) / total)
                    });
                    let label = percent
                        .map_or_else(|| kind.to_string(), |value| format!("{kind} {value}%"));
                    let detail = progress
                        .filter(|progress| progress.total_entries > 0)
                        .map(|progress| {
                            format!(
                                "{}/{} items",
                                progress.processed_entries, progress.total_entries
                            )
                        })
                        .or_else(|| {
                            progress
                                .and_then(|progress| progress.current_path.as_deref())
                                .map(path_label)
                        })
                        .unwrap_or_default();
                    let description = if detail.is_empty() {
                        format!("{label}. Show operation details")
                    } else {
                        format!("{label}, {detail}. Show operation details")
                    };
                    (label, description)
                })
        } else {
            None
        };
        let status = self
            .status_message
            .clone()
            .or_else(|| {
                self.session_store.as_ref().and_then(|store| {
                    store
                        .last_error()
                        .map(|error| format!("Unable to save session: {error}"))
                })
            })
            .or_else(|| {
                self.settings_store.as_ref().and_then(|store| {
                    store
                        .last_error()
                        .map(|error| format!("Unable to save settings: {error}"))
                })
            })
            .or_else(|| {
                self.workspace_store.as_ref().and_then(|store| {
                    store
                        .last_error()
                        .map(|error| format!("Unable to save workspaces: {error}"))
                })
            })
            .or_else(|| self.current_listing_warning().map(str::to_string));
        let watch_summary = match &self.watcher.status {
            WatchStatus::Starting => {
                Some(("Watcher: starting…".to_string(), self.palette.tertiary))
            }
            WatchStatus::Watching => None,
            WatchStatus::Unavailable(error) => Some((
                format!("Watcher unavailable — manual Refresh remains available: {error}"),
                rgb(0xffb86c),
            )),
        };
        let main_area_width = f32::from(bounds.size.width)
            - if self.layout.sidebar_collapsed {
                0.0
            } else {
                self.layout.sidebar_width
            };
        let logical_main_width = main_area_width / self.palette.scale.max(0.1);
        let status_selection_width = if logical_main_width <= 620.0 && operation_summary.is_some() {
            96.0 * self.palette.scale
        } else if logical_main_width <= 620.0 {
            200.0 * self.palette.scale
        } else {
            300.0 * self.palette.scale
        };
        let title_bar = self.render_title_bar(window.is_maximized(), cx);
        let toolbar = self.render_toolbar(logical_main_width < 760.0, cx);
        let plugin_invitation = self.render_plugin_invitation(cx);
        let plugin_details = self.render_plugin_details(cx);
        let settings_panel = self.render_settings_panel(cx);
        let settings_confirmation = self.render_settings_confirmation(window, cx);
        let go_to_folder = self.render_go_to_folder(f32::from(bounds.size.height), cx);
        let recovery_notice = self.render_recovery_notice(cx);
        let search_scope_bar = self.render_search_scope_bar(cx);
        let operation_panel = self.render_operation_panel(status.is_some(), cx);
        let archive_inspection = self.render_archive_inspection(cx);
        let preview_open = !matches!(self.preview.state, PreviewState::Closed);
        let inspector_preview_open = self.settings.view.show_preview_panel
            && !self.quick_look.open
            && self.browser.view_mode() != ViewMode::Column;
        let preview_as_side_panel = inspector_preview_open && logical_main_width >= 708.0;
        self.layout.listing_viewport_width = (main_area_width
            - 16.0 * self.palette.scale
            - if preview_as_side_panel {
                (self.layout.preview_panel_width + 8.0) * self.palette.scale
            } else {
                0.0
            })
        .max(1.0);
        let side_preview = preview_as_side_panel.then(|| {
            if preview_open {
                self.render_preview_panel(true, false, false, cx)
            } else {
                self.render_preview_summary_panel(true, false, cx)
            }
        });
        let bottom_preview = (inspector_preview_open && !preview_as_side_panel).then(|| {
            if preview_open {
                self.render_preview_panel(false, false, false, cx)
            } else {
                self.render_preview_summary_panel(false, false, cx)
            }
        });
        let quick_look = self
            .quick_look
            .open
            .then(|| self.render_preview_panel(false, true, false, cx));
        let mutation_prompt = self.render_mutation_prompt(cx);
        let tabs = self.render_tabs(cx);
        let header = if self.browser.view_mode() == ViewMode::List {
            self.render_header(cx)
        } else {
            div().into_any_element()
        };
        // The listing and the sidebar render in their own cached views, so
        // they are rebuilt only when this window notifies (or they notify
        // themselves), not when another child view such as the media player
        // does.
        let pane_probe = self.panes.begin_render(PaneInputs {
            palette: self.palette,
            listing_viewport_width: self.layout.listing_viewport_width,
        });
        let listing_style = div().flex_1().min_h_0().min_w_0().style().clone();
        let listing = self.panes.element(PaneKind::Listing, listing_style);
        let sidebar_style = if self.layout.sidebar_collapsed {
            div().w_0().style().clone()
        } else {
            div()
                .flex_shrink_0()
                .w(px(self.layout.sidebar_width))
                .h_full()
                .style()
                .clone()
        };
        let sidebar = self.panes.element(PaneKind::Sidebar, sidebar_style);
        let sidebar_resizer = self.render_sidebar_resizer(cx);
        let control_surface = self.render_control_surface(cx);
        let mutation_exit_notice = self.render_mutation_exit_notice(cx);
        let file_conflict_prompt = self.render_file_conflict_prompt(cx);
        let named_theme_editor = self.render_named_theme_editor(cx);
        let shortcut_editor = self.render_shortcut_editor(cx);
        let appearance_value_editor = self.render_appearance_value_editor(cx);
        let context_menu = self.render_file_context_menu(bounds.size, cx);
        let toast = self.render_toast(cx);
        let search_running = self.search.task.is_some() && self.listing_shows_search_results();
        let status_panel = self.settings.view.show_status_bar.then(|| {
            let status_line = div()
                .id("watcher-status")
                .debug_selector(|| "watcher-status".to_string())
                .role(Role::Status)
                .aria_label("Folder status")
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .h(px(28.0 * self.palette.scale))
                .min_h(px(28.0 * self.palette.scale))
                .px_2()
                .border_t_1()
                .border_color(self.palette.border)
                .bg(self.palette.surface)
                .text_xs()
                .text_color(self.palette.muted)
                .overflow_hidden()
                .child(
                    div()
                        .id("status-left-cluster")
                        .debug_selector(|| "status-left-cluster".to_string())
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .id("status-item-summary")
                                .debug_selector(|| "status-item-summary".to_string())
                                .flex()
                                .items_center()
                                .gap_2()
                                .flex_shrink_0()
                                .text_color(self.palette.text)
                                .child(item_summary)
                                .when_some(total_size_summary, |summary, size| {
                                    summary
                                        .child(div().text_color(self.palette.border).child("/"))
                                        .child(div().text_color(self.palette.tertiary).child(size))
                                }),
                        )
                        .when_some(filter_summary, |cluster, summary| {
                            cluster.child(
                                div()
                                    .id("status-filter-summary")
                                    .debug_selector(|| "status-filter-summary".to_string())
                                    .flex_1()
                                    .w(px(0.0))
                                    .min_w_0()
                                    .truncate()
                                    .text_color(self.palette.accent)
                                    .child(summary),
                            )
                        })
                        .when_some(operation_summary, |cluster, (label, description)| {
                            cluster.child(
                                div()
                                    .id("status-operation-summary")
                                    .debug_selector(|| "status-operation-summary".to_string())
                                    .role(Role::Button)
                                    .aria_label(description)
                                    .focusable()
                                    .tab_stop(true)
                                    .flex()
                                    .items_center()
                                    .h(px(24.0 * self.palette.scale))
                                    .flex_shrink_0()
                                    .px_2()
                                    .border_1()
                                    .border_color(with_alpha(self.palette.border, 0.0))
                                    .text_color(self.palette.accent)
                                    .focus(|button| {
                                        button
                                            .bg(self.palette.hover)
                                            .border_color(self.palette.accent)
                                    })
                                    .hover(|button| {
                                        button
                                            .bg(self.palette.hover)
                                            .border_color(self.palette.border)
                                    })
                                    .cursor_pointer()
                                    .child(label)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.open_operation_panel(cx)),
                                    ),
                            )
                        })
                        .when(search_running, |cluster| {
                            cluster.child(
                                toolbar_button("stop-search", "Stop search", self.palette.control)
                                    .aria_label("Stop the active search")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.cancel_smart_search(cx)),
                                    ),
                            )
                        }),
                )
                .child(
                    div()
                        .id("status-right-cluster")
                        .debug_selector(|| "status-right-cluster".to_string())
                        .flex()
                        .flex_shrink(1.)
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .min_w_0()
                        .overflow_hidden()
                        .when_some(selection_summary, |cluster, summary| {
                            cluster.child(
                                div()
                                    .id("status-selection-summary")
                                    .debug_selector(|| "status-selection-summary".to_string())
                                    .flex_shrink(1.)
                                    .max_w(px(status_selection_width))
                                    .min_w_0()
                                    .overflow_hidden()
                                    .truncate()
                                    .text_color(self.palette.muted)
                                    .child(summary),
                            )
                        })
                        .when_some(watch_summary, |cluster, (label, color)| {
                            cluster.child(
                                div()
                                    .id("status-watcher-summary")
                                    .debug_selector(|| "status-watcher-summary".to_string())
                                    .flex_shrink_0()
                                    .text_color(color)
                                    .child(label),
                            )
                        }),
                )
                .into_any_element();
            div()
                .flex()
                .flex_col()
                .child(status_line)
                .when_some(status, |panel, status| {
                    panel.child(
                        div()
                            .id("status-message")
                            .px_3()
                            .py_2()
                            .border_t_1()
                            .border_color(self.palette.border)
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(status),
                    )
                })
                .into_any_element()
        });
        let main_footer = div()
            .id("main-footer")
            .debug_selector(|| "main-footer".to_string())
            .flex()
            .flex_col()
            .when_some(bottom_preview, |footer, preview| footer.child(preview))
            .child(archive_inspection)
            .when_some(status_panel, |footer, status| footer.child(status))
            .into_any_element();

        let root = div()
            .id("explorie-window")
            .role(Role::Application)
            .aria_label("Explorie file manager")
            .key_context(if self.settings_ui.confirmation.is_some() {
                "settings-confirmation"
            } else if self.quick_look.open {
                "quick-look"
            } else if self.navigation_ui.go_to_folder.is_some() {
                "go-to-folder"
            } else {
                "browser"
            })
            .track_focus(&self.focus_handle)
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(with_alpha(rgb(0x4ea1ff), 0.06)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.drop_external_paths_to(paths, this.browser.path().to_path_buf(), cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(Self::handle_search_key))
            .capture_key_up(cx.listener(Self::handle_key_up))
            .on_action(cx.listener(|this, _: &GoBack, _, cx| this.go_back(cx)))
            .on_action(cx.listener(|this, _: &GoForward, _, cx| this.go_forward(cx)))
            .on_action(cx.listener(|this, _: &GoUp, _, cx| this.go_up(cx)))
            .on_action(cx.listener(|this, _: &GoToFolder, _, cx| {
                this.open_go_to_folder(cx);
            }))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.refresh(cx)))
            .on_action(cx.listener(|this, _: &ToggleHidden, _, cx| {
                this.toggle_hidden(cx);
            }))
            .on_action(cx.listener(|this, _: &CycleFilter, _, cx| {
                this.cycle_filter(cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select_next(cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.select_previous(cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| {
                if this.settings_ui.named_theme_editor.is_some() {
                    this.submit_named_theme_editor(cx);
                } else if this.settings_ui.appearance_value_editor.is_some() {
                    this.submit_appearance_value_editor(cx);
                } else if !this.confirm_open_prompt(cx) {
                    this.open_selected(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ClearSelection, window, cx| {
                if this.settings_ui.confirmation.is_some() {
                    this.cancel_settings_confirmation(window, cx);
                } else if !this.operation_ui.conflict_prompts.is_empty() {
                    this.cancel_all_file_conflicts(cx);
                } else if this.mutation.prompt.is_some() {
                    this.cancel_mutation_prompt(cx);
                } else if this.quick_look.open {
                    this.close_quick_look(cx);
                } else if this.navigation_ui.go_to_folder.is_some() {
                    this.close_go_to_folder(cx);
                } else if this.search_field_focused(window, cx) {
                    // Escape in the search field clears it and returns to the list.
                    this.clear_search_and_focus_list(window, cx);
                } else if this.search.task.is_some() && this.listing_shows_search_results() {
                    this.cancel_smart_search(cx);
                } else if this.context_menu.menu.is_some() {
                    this.close_context_menu(cx);
                } else if this.settings_ui.shortcut_editor.is_some() {
                    this.cancel_shortcut_editor(cx);
                } else if this.settings_ui.named_theme_editor.is_some() {
                    this.cancel_named_theme_editor(cx);
                } else if this.settings_ui.appearance_value_editor.is_some() {
                    this.cancel_appearance_value_editor(cx);
                } else if this.operation_ui.undo_progress.is_some() {
                    this.cancel_undo(cx);
                } else if this.pointer.selection_marquee.is_some() {
                    this.cancel_selection_marquee(cx);
                } else if this.mutation.exit_waiting {
                    this.keep_app_open(cx);
                } else if this.overlay.toolbar_menu != ToolbarMenu::Closed {
                    this.close_toolbar_menu(cx);
                } else if this.overlay.surface != ControlSurface::Closed {
                    this.close_control_surface(cx);
                } else if this.plugin_ui.details_open {
                    this.plugin_ui.details_open = false;
                    cx.notify();
                } else if this.settings_ui.panel_open {
                    this.close_settings_panel(cx);
                } else if matches!(this.preview.state, PreviewState::Closed) {
                    this.browser.clear_selection();
                    cx.notify();
                } else {
                    this.close_preview(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ShowListView, _, cx| {
                this.set_view_mode(ViewMode::List, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowGridView, _, cx| {
                this.set_view_mode(ViewMode::Grid, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowColumnView, _, cx| {
                this.set_view_mode(ViewMode::Column, cx);
            }))
            .on_action(cx.listener(|this, _: &ColumnLeft, _, cx| this.column_left(cx)))
            .on_action(cx.listener(|this, _: &ColumnRight, _, cx| {
                this.column_right(cx);
            }))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.search.active = true;
                window.focus(&this.focus_handle, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleFolderSizes, _, cx| {
                this.toggle_folder_sizes(cx);
            }))
            .on_action(cx.listener(|this, _: &NewWindow, _, cx| {
                this.open_new_window(false, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveTabToNewWindow, _, cx| {
                this.open_new_window(true, cx);
            }))
            .on_action(cx.listener(|this, _: &NewTab, _, cx| this.new_tab(cx)))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                this.close_active_tab_or_window(window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextTab, _, cx| {
                this.activate_tab_offset(1, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousTab, _, cx| {
                this.activate_tab_offset(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveTabLeft, _, cx| {
                this.move_active_tab(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveTabRight, _, cx| {
                this.move_active_tab(1, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFavorite, _, cx| {
                this.toggle_current_favorite(cx);
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.browser.select_all();
                this.sync_column_selection_from_browser();
                this.sync_pinned_preview(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectNextRange, _, cx| {
                this.select_next_range(cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousRange, _, cx| {
                this.select_previous_range(cx);
            }))
            .on_action(cx.listener(|this, _: &SaveSearch, _, cx| {
                this.save_current_search(cx);
            }))
            .on_action(cx.listener(|this, _: &CopySelected, _, cx| {
                this.copy_selected(cx);
            }))
            .on_action(cx.listener(|this, _: &CutSelected, _, cx| {
                this.cut_selected(cx);
            }))
            .on_action(cx.listener(|this, _: &Paste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &TrashSelected, _, cx| {
                this.trash_selected(cx);
            }))
            .on_action(cx.listener(|this, _: &PermanentDeleteSelected, _, cx| {
                this.prompt_permanent_delete_selected(cx);
            }))
            .on_action(cx.listener(|this, _: &CreateArchive, _, cx| {
                this.prompt_create_archive(cx);
            }))
            .on_action(cx.listener(|this, _: &ExtractArchive, _, cx| {
                this.prompt_extract_archive(cx);
            }))
            .on_action(cx.listener(|this, _: &InspectArchive, _, cx| {
                this.inspect_selected_archive(cx);
            }))
            .on_action(cx.listener(|this, _: &CloseArchiveInspection, _, cx| {
                this.close_archive_inspection(cx);
            }))
            .on_action(cx.listener(|this, _: &PreviewSelected, _, cx| {
                this.toggle_quick_look_selected(cx);
            }))
            .on_action(cx.listener(|this, _: &ClosePreview, _, cx| {
                if this.quick_look.open {
                    this.close_quick_look(cx);
                } else {
                    this.close_preview(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &RetryPreview, _, cx| {
                this.retry_preview(cx);
            }))
            .on_action(cx.listener(|this, _: &ClearPreviewCache, _, cx| {
                this.request_settings_confirmation(SettingsConfirmation::ClearPreviewCache, cx);
            }))
            .on_action(cx.listener(|this, _: &RefreshPreviewHelpers, _, cx| {
                this.start_preview_helpers(cx);
            }))
            .on_action(cx.listener(|this, _: &CycleArchiveFormat, _, cx| {
                this.cycle_archive_format(cx);
            }))
            .on_action(cx.listener(|this, _: &CycleArchiveCompression, _, cx| {
                this.cycle_archive_compression(cx);
            }))
            .on_action(cx.listener(|this, _: &CancelOperation, _, cx| {
                this.cancel_latest_operation(cx);
            }))
            .on_action(cx.listener(|this, _: &RetryOperation, _, cx| {
                this.retry_latest_operation(cx);
            }))
            .on_action(cx.listener(|this, _: &ClearCompletedOperations, _, cx| {
                this.clear_completed_operations(cx);
            }))
            .on_action(cx.listener(|this, _: &CycleConflictPolicy, _, cx| {
                this.cycle_conflict_policy(cx);
            }))
            .on_action(cx.listener(|this, _: &NewFolder, _, cx| {
                this.prompt_new_folder(cx);
            }))
            .on_action(cx.listener(|this, _: &NewNote, _, cx| {
                this.prompt_new_note(cx);
            }))
            .on_action(cx.listener(|this, _: &RenameSelected, _, cx| {
                if !this.confirm_open_prompt(cx) {
                    this.prompt_rename_selected(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NewWebsiteLink, _, cx| {
                this.prompt_new_website_link(cx);
            }))
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.undo(cx)))
            .on_action(cx.listener(|this, _: &Redo, _, cx| this.redo(cx)))
            .on_action(cx.listener(|this, _: &ToggleSettingsPanel, _, cx| {
                this.toggle_settings_panel(cx);
            }))
            .on_action(cx.listener(|this, _: &CloseSettingsPanel, _, cx| {
                this.close_settings_panel(cx);
            }))
            .on_action(cx.listener(|this, _: &ResetSettings, _, cx| {
                this.request_settings_confirmation(SettingsConfirmation::ResetSettings, cx);
            }))
            .on_action(cx.listener(|this, _: &CycleTheme, _, cx| this.cycle_theme(cx)))
            .on_action(cx.listener(|this, _: &CycleAccent, _, cx| this.cycle_accent(cx)))
            .on_action(cx.listener(|this, _: &CycleDensity, _, cx| this.cycle_density(cx)))
            .on_action(cx.listener(|this, _: &CycleUiScale, _, cx| this.cycle_ui_scale(cx)))
            .on_action(cx.listener(|this, _: &IncreaseUiScale, _, cx| {
                this.adjust_ui_scale(1, cx);
            }))
            .on_action(cx.listener(|this, _: &DecreaseUiScale, _, cx| {
                this.adjust_ui_scale(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &ResetUiScale, _, cx| this.reset_ui_scale(cx)))
            .on_action(cx.listener(|this, _: &CycleListRowHeight, _, cx| {
                this.cycle_list_row_height(cx);
            }))
            .on_action(cx.listener(|this, _: &CycleGridWidth, _, cx| {
                this.cycle_grid_width(cx);
            }))
            .on_action(cx.listener(|this, _: &CycleFont, _, cx| this.cycle_font(cx)))
            .on_action(cx.listener(|this, _: &CycleBorderRadius, _, cx| {
                this.cycle_border_radius(cx);
            }))
            .on_action(cx.listener(|this, _: &CycleIconSize, _, cx| {
                this.cycle_icon_size(cx);
            }))
            .on_action(cx.listener(|this, _: &CycleUndoTimeout, _, cx| {
                this.cycle_undo_timeout(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSystemFiles, _, cx| {
                this.toggle_system_files(cx);
            }))
            .on_action(cx.listener(|this, _: &TogglePreviewPanel, _, cx| {
                this.toggle_preview_panel(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleStatusBar, _, cx| {
                this.toggle_status_bar(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleConfirmDelete, _, cx| {
                this.toggle_confirm_delete(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleScriptPreview, _, cx| {
                this.toggle_script_preview(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleErrorReporting, _, cx| {
                this.toggle_error_reporting(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRemoteDrives, _, cx| {
                this.toggle_remote_drives(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleReduceMotion, _, cx| {
                this.toggle_reduce_motion(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleHighContrast, _, cx| {
                this.toggle_high_contrast(cx);
            }))
            .on_action(cx.listener(|this, _: &OpenCommandPalette, _, cx| {
                this.open_control_surface(ControlSurface::CommandPalette, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleShortcutsOverlay, _, cx| {
                this.toggle_control_surface(ControlSurface::Shortcuts, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleDiagnostics, _, cx| {
                this.toggle_control_surface(ControlSurface::Diagnostics, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleWorkspaceManager, _, cx| {
                this.toggle_control_surface(ControlSurface::Workspaces, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRemoteDriveManager, _, cx| {
                this.toggle_control_surface(ControlSurface::RemoteDrives, cx);
                if this.overlay.surface == ControlSurface::RemoteDrives {
                    this.refresh_remote_environment(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SaveWorkspace, _, cx| {
                this.open_workspace_manager(cx);
            }))
            .on_action(cx.listener(|this, _: &CloseControlSurface, _, cx| {
                this.close_control_surface(cx);
            }))
            .on_action(cx.listener(|this, _: &CopyDiagnostics, _, cx| {
                this.copy_diagnostics(cx);
            }))
            .on_action(cx.listener(|this, _: &DismissRecovery, _, cx| {
                this.dismiss_recovery(cx);
            }))
            .on_action(cx.listener(|this, _: &DismissToast, _, cx| {
                this.dismiss_toast(cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.finish_sidebar_resize(cx);
                    this.finish_preview_panel_resize(cx);
                    this.finish_list_column_resize(cx);
                    this.finish_column_view_resize(cx);
                    this.finish_selection_marquee(cx);
                    this.finish_file_drag(cx);
                    this.media
                        .update(cx, |media, _| media.finish_pointer_drags());
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.finish_sidebar_resize(cx);
                    this.finish_preview_panel_resize(cx);
                    this.finish_list_column_resize(cx);
                    this.finish_column_view_resize(cx);
                    this.finish_selection_marquee(cx);
                    this.finish_file_drag(cx);
                    this.media
                        .update(cx, |media, _| media.finish_pointer_drags());
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    this.media.update(cx, |media, _| media.finish_model_drag());
                }),
            )
            .on_mouse_up_out(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    this.media.update(cx, |media, _| media.finish_model_drag());
                }),
            )
            .on_mouse_move(cx.listener(|this, event, _, cx| {
                this.update_sidebar_resize(event, cx);
                this.update_preview_panel_resize(event, cx);
                this.update_list_column_resize(event, cx);
                this.update_column_view_resize(event, cx);
                this.update_selection_marquee(event, cx);
                this.media
                    .update(cx, |media, cx| media.update_pointer_drags(event, cx));
            }))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(self.palette.window)
            .text_color(self.palette.text)
            .font_family(font_family(&self.settings))
            .text_sm()
            .child(title_bar)
            .child(
                div()
                    .id("app-body")
                    .relative()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .overflow_hidden()
                    .child(sidebar)
                    .child(
                        div()
                            .id("main-area")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .w(px(0.0))
                            .min_w_0()
                            .overflow_hidden()
                            .child(div().px_2().bg(self.palette.topbar).child(toolbar))
                            .child(tabs)
                            .child(plugin_invitation)
                            .child(recovery_notice)
                            .child(search_scope_bar)
                            .child(
                                div()
                                    .id("main-content")
                                    .debug_selector(|| "main-content".to_string())
                                    .role(Role::Main)
                                    .aria_label("File browser")
                                    .flex()
                                    .flex_1()
                                    .min_h_0()
                                    .min_w_0()
                                    .w_full()
                                    .gap_2()
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .id("file-surface")
                                            .debug_selector(|| "file-surface".to_string())
                                            .role(Role::Region)
                                            .aria_label("File list")
                                            .flex()
                                            .flex_col()
                                            .flex_1()
                                            .w(px(0.0))
                                            .min_w_0()
                                            .child(header)
                                            .child(listing),
                                    )
                                    .when_some(side_preview, |body, preview| body.child(preview)),
                            )
                            .child(main_footer),
                    )
                    .child(sidebar_resizer),
            )
            // The operations panel floats over the file list but under every
            // dialog, sheet and menu, so it never covers a modal prompt.
            .child(operation_panel)
            .child(plugin_details)
            .child(settings_panel)
            .child(control_surface)
            .child(go_to_folder)
            .child(mutation_exit_notice)
            .child(file_conflict_prompt)
            .child(mutation_prompt)
            .child(named_theme_editor)
            .child(shortcut_editor)
            .child(appearance_value_editor)
            .child(settings_confirmation)
            .child(context_menu)
            .when_some(quick_look, |window, quick_look| window.child(quick_look))
            .child(toast)
            .child(pane_probe);
        #[cfg(test)]
        {
            self.render_stats.root += 1;
            self.render_stats.root_time += render_started.elapsed();
        }
        root
    }
}
