//! `DirectoryWindow` behavior for listing view.

use super::selection::is_secondary_click;
use crate::*;

impl DirectoryWindow {
    pub(crate) fn render_empty_listing(
        &mut self,
        id: impl Into<ElementId>,
        column: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let query = self.browser.search_query().trim().to_string();
        let filter = self.browser.filter();
        let underlying_entries = self.browser.entries().len();
        let (title, description) = if column {
            (
                "This folder is empty".to_string(),
                "There are no items in this column.".to_string(),
            )
        } else if !query.is_empty() {
            (
                "No search results".to_string(),
                format!("No items match “{query}”."),
            )
        } else if filter != EntryFilter::All {
            (
                format!("No {} here", filter.label().to_lowercase()),
                format!("The current {} filter hides every item.", filter.label()),
            )
        } else if underlying_entries > 0 {
            (
                "No visible items".to_string(),
                "Hidden or system-file preferences are hiding this folder’s contents.".to_string(),
            )
        } else {
            (
                "This folder is empty".to_string(),
                "Create a folder or paste items here to get started.".to_string(),
            )
        };
        let mut actions = Vec::<AnyElement>::new();
        if !column && !query.is_empty() {
            actions.push(
                toolbar_button("empty-clear-search", "Clear search", self.palette.control)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.clear_search_and_focus_list(window, cx);
                        this.persist_session();
                    }))
                    .into_any_element(),
            );
        } else if !column && filter != EntryFilter::All {
            actions.push(
                toolbar_button("empty-show-all", "Show all items", self.palette.control)
                    .on_click(cx.listener(|this, _, _, cx| this.set_filter(EntryFilter::All, cx)))
                    .into_any_element(),
            );
        } else if !column && underlying_entries > 0 && !self.browser.show_hidden() {
            actions.push(
                toolbar_button(
                    "empty-show-hidden",
                    "Show hidden files",
                    self.palette.control,
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_hidden(cx)))
                .into_any_element(),
            );
        } else if !column && underlying_entries > 0 && !self.browser.show_system_files() {
            actions.push(
                toolbar_button(
                    "empty-show-system",
                    "Show system files",
                    self.palette.control,
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_system_files(cx)))
                .into_any_element(),
            );
        } else if !column {
            actions.push(
                toolbar_button("empty-new-folder", "New folder", self.palette.control)
                    .on_click(cx.listener(|this, _, _, cx| this.prompt_new_folder(cx)))
                    .into_any_element(),
            );
            if self.paste_count().is_some() {
                actions.push(
                    toolbar_button("empty-paste", "Paste", self.palette.control)
                        .debug_selector(|| "empty-paste".to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.paste(cx)))
                        .into_any_element(),
                );
            }
        }

        div()
            .id(id)
            .flex()
            .flex_1()
            .min_h(px(if column { 120.0 } else { 240.0 } * self.palette.scale))
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .text_center()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(self.palette.text)
                    .child(title),
            )
            .child(
                div()
                    .max_w(px(420.0 * self.palette.scale))
                    .text_xs()
                    .text_color(self.palette.muted)
                    .child(description),
            )
            .when(!actions.is_empty(), |state| {
                state.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_center()
                        .gap_2()
                        .mt_2()
                        .children(actions),
                )
            })
            .into_any_element()
    }

    /// Render the active listing view. Icons and thumbnails inside it decode
    /// through the window's bounded listing image cache.
    pub(crate) fn render_listing(&mut self, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(test)]
        {
            self.render_stats.listing += 1;
        }
        self.begin_entry_visual_frame();
        let image_cache = self.image_memory.listing_cache(cx);
        let listing = self.render_listing_content(cx);
        with_image_cache(&image_cache, listing).into_any_element()
    }

    fn render_listing_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.browser.view_mode() == ViewMode::Column {
            return self.render_column_view(cx);
        }

        let content = match &self.listing.state {
            ListingState::Loading if self.browser.entries().is_empty() => div()
                .id("listing-loading")
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(self.palette.muted)
                .child("Loading files…")
                .into_any_element(),
            ListingState::Failed(error) => {
                let error = error.clone();
                let path = self.browser.path().display().to_string();
                let picker_active = self.navigation_ui.folder_picker_active;
                let picker_error = self.navigation_ui.folder_picker_error.clone();
                let palette = self.palette;
                let danger = rgb(0xff6b6b);
                div()
                    .id("listing-error")
                    .debug_selector(|| "listing-error".to_string())
                    .role(Role::Alert)
                    .aria_label("Folder load error")
                    .flex()
                    .flex_1()
                    .w_full()
                    .min_w_0()
                    .min_h(px(240.0))
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .p_6()
                    .border_1()
                    .border_color(with_alpha(danger, 0.4))
                    .bg(with_alpha(danger, 0.06))
                    .text_center()
                    .text_color(palette.text)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(28.0))
                            .mb_1()
                            .rounded_full()
                            .border_1()
                            .border_color(danger)
                            .text_base()
                            .font_weight(FontWeight::BOLD)
                            .text_color(danger)
                            .child("!"),
                    )
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Couldn’t load this folder"),
                    )
                    .child(
                        div()
                            .max_w(px(560.0 * palette.scale))
                            .truncate()
                            .text_xs()
                            .text_color(palette.text)
                            .child(path),
                    )
                    .child(
                        div()
                            .max_w(px(560.0 * palette.scale))
                            .text_sm()
                            .text_color(palette.muted)
                            .child(error),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .mt_2()
                            .child(
                                div().text_color(palette.window).child(
                                    toolbar_button("retry-listing", "Try again", palette.accent)
                                        .debug_selector(|| "retry-listing".to_string())
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.start_listing(cx)),
                                        ),
                                ),
                            )
                            .child(
                                toolbar_button(
                                    "choose-another-folder",
                                    if picker_active {
                                        "Opening…"
                                    } else {
                                        "Choose another folder"
                                    },
                                    if picker_active {
                                        with_alpha(palette.control, 0.65)
                                    } else {
                                        palette.control
                                    },
                                )
                                .debug_selector(|| "choose-another-folder".to_string())
                                .when(!picker_active, |button| {
                                    button.on_click(
                                        cx.listener(|this, _, _, cx| this.open_folder_picker(cx)),
                                    )
                                }),
                            ),
                    )
                    .when_some(picker_error, |state, error| {
                        state.child(
                            div()
                                .id("folder-picker-error")
                                .debug_selector(|| "folder-picker-error".to_string())
                                .max_w(px(560.0 * palette.scale))
                                .mt_1()
                                .text_sm()
                                .text_color(danger)
                                .child(format!("Couldn’t open the folder picker: {error}")),
                        )
                    })
                    .into_any_element()
            }
            ListingState::Ready | ListingState::Loading
                if self.browser.visible_entries().is_empty() =>
            {
                self.render_empty_listing("listing-empty", false, cx)
            }
            ListingState::Ready | ListingState::Loading => match self.browser.view_mode() {
                ViewMode::List => self.render_list_view(cx),
                ViewMode::Grid => self.render_grid_view(cx),
                ViewMode::Column => unreachable!("column view is handled before listing state"),
            },
        };
        let target = self.browser.path().to_path_buf();
        let accessibility_label = format!(
            "Files in {}, {} items",
            self.browser.path().display(),
            self.browser.visible_entries().len()
        );
        let drop_target = target.clone();
        let palette = self.palette;
        div()
            .id("listing-drop-surface")
            .debug_selector(|| "listing-drop-surface".to_string())
            .role(Role::Group)
            .aria_label(accessibility_label)
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(content)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle, cx);
                    this.open_empty_context_menu(event.position, cx);
                }),
            )
            .drag_over::<FileDrag>(move |style, drag, window, _| {
                if valid_file_drop_target(&target, drag, file_drag_operation(window)) {
                    style.bg(with_alpha(palette.accent, 0.08))
                } else {
                    style
                }
            })
            .on_drag_move::<FileDrag>(cx.listener(
                |this, event: &gpui::DragMoveEvent<FileDrag>, window, cx| {
                    let drag = event.drag(cx).clone();
                    this.track_file_drag(&drag, cx);
                    this.maybe_start_external_file_drag(&drag, event.event.position, window, cx);
                    this.auto_scroll_primary_file_drag(event);
                },
            ))
            .on_drop(cx.listener(move |this, drag: &FileDrag, window, cx| {
                this.drop_files_to(drag, drop_target.clone(), false, window, cx);
            }))
            .into_any_element()
    }

    /// The list's custom columns with their widths, then the Size and
    /// Modified widths; the header and every row share this layout.
    pub(crate) fn list_column_layout(&self) -> (Vec<(String, f32)>, f32, f32) {
        let columns = self.browser.custom_columns();
        let custom_widths = columns
            .iter()
            .map(|column| {
                self.browser
                    .column_width(&format!("custom:{column}"))
                    .map(f32::from)
            })
            .collect::<Vec<_>>();
        let widths = list_column_widths(
            self.layout.listing_viewport_width,
            self.browser.column_width("size").map(f32::from),
            self.browser.column_width("modified").map(f32::from),
            &custom_widths,
        );
        (
            columns.into_iter().zip(widths.custom).collect(),
            widths.size,
            widths.modified,
        )
    }

    pub(crate) fn render_list_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let count = self.browser.visible_entries().len();
        let marquee_layout = MarqueeLayout::List {
            row_height: self.settings.appearance.list_row_height as f32,
        };
        if self
            .pointer
            .selection_marquee
            .as_ref()
            .is_some_and(|marquee| marquee.layout != marquee_layout)
        {
            self.pointer.selection_marquee = None;
        }
        let marquee_overlay = self.selection_marquee_overlay_rect(marquee_layout);
        let (columns, size_width, modified_width) = self.list_column_layout();
        let list = uniform_list(
            "listing-results",
            count,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                #[cfg(test)]
                {
                    this.last_rendered_items = range.len();
                    this.render_stats.listing_rows += range.len();
                }
                this.reserve_entry_visuals(range.len());
                let mut rows = Vec::with_capacity(range.end - range.start);
                for index in range {
                    let Some(entry) = this.browser.visible_entries().get(index).cloned() else {
                        continue;
                    };
                    let path = entry.path.clone();
                    let pointer_path = path.clone();
                    let context_path = path.clone();
                    let is_dir = is_folder_like(&entry);
                    let hidden = entry.hidden;
                    let selected = this.browser.is_selected(&path);
                    let dragging = this.pointer.file_drag_sources.contains(&path);
                    let drag_selection = this.browser.selection_snapshot();
                    let drag_view = cx.entity().downgrade();
                    let palette = this.palette;
                    let icon_size =
                        (this.settings.appearance.icon_size as f32 + 4.0).clamp(16.0, 28.0);
                    let native_icon = this.cached_entry_icon(&entry, cx);
                    let icon = entry_icon_visual(&entry, native_icon, icon_size, palette);
                    let size = entry_detail(&entry, this.calculate_folder_sizes, false);
                    let modified = format_modified(entry.modified);
                    let custom_cells = columns.iter().map(|(column, width)| {
                        let value = entry
                            .custom
                            .get(column)
                            .map(display_value)
                            .unwrap_or_else(|| "-".to_string());
                        let selector = format!("entry-{index}-custom-{}", column.to_lowercase());
                        let aria_label = format!("{column}: {value}");
                        div()
                            .id(ElementId::Name(selector.clone().into()))
                            .debug_selector(move || selector.clone())
                            .role(Role::Group)
                            .aria_label(aria_label)
                            .w(px(*width))
                            .min_w(px(*width))
                            .px_2()
                            .truncate()
                            .text_xs()
                            .text_color(palette.muted)
                            .child(value)
                            .into_any_element()
                    });
                    let accessibility_label = format!(
                        "{}{}: {}",
                        if hidden { "Hidden " } else { "" },
                        if is_dir { "folder" } else { "file" },
                        file_name(&entry)
                    );
                    let accessibility_path = path.clone();
                    let accessibility_view = cx.entity().downgrade();
                    let row_content = div()
                        .id(("entry-drag-source", index))
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .items_center()
                        .cursor_pointer()
                        .when(hidden, |content| {
                            content.debug_selector(move || format!("list-hidden-entry-{index}"))
                        })
                        .opacity(if hidden { HIDDEN_ENTRY_OPACITY } else { 1.0 })
                        .child(
                            div()
                                .flex()
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .px_3()
                                .child(icon)
                                .child(
                                    div()
                                        .debug_selector(move || format!("entry-{index}-name"))
                                        .min_w_0()
                                        .truncate()
                                        .child(file_name(&entry)),
                                )
                                .children(entry_tag_dots(
                                    &entry,
                                    format!("entry-tags-{index}"),
                                    (8.0 * palette.scale).clamp(6.0, 11.0),
                                    palette,
                                ))
                                .child(div().flex_1())
                                .children(entry_cloud_badge(
                                    &entry,
                                    format!("entry-cloud-{index}"),
                                    (13.0 * palette.scale).clamp(11.0, 16.0),
                                    palette,
                                ))
                                .when_some(
                                    this.plugin_entry_decoration(&entry.path),
                                    |row, label| {
                                        row.child(
                                            div().text_xs().text_color(palette.accent).child(label),
                                        )
                                    },
                                ),
                        )
                        .children(custom_cells)
                        .child(
                            div()
                                .flex_none()
                                .w(px(size_width))
                                .px_2()
                                .truncate()
                                .text_sm()
                                .child(size),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(modified_width))
                                .px_2()
                                .truncate()
                                .text_sm()
                                .child(modified),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                window.focus(&this.focus_handle, cx);
                                if is_secondary_click(event) {
                                    this.open_file_context_menu(
                                        pointer_path.clone(),
                                        is_dir,
                                        event.position,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                    return;
                                }
                                this.select_from_pointer(pointer_path.clone(), event, cx);
                                if event.click_count >= 2 {
                                    this.open_entry(pointer_path.clone(), is_dir, cx);
                                }
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        )
                        .on_drag(FileDrag::deferred(), move |drag, position, _, cx| {
                            drag_view
                                .read_with(cx, |view, _| {
                                    drag.initialize(
                                        &entry,
                                        view.pointer
                                            .file_drag_selection
                                            .as_ref()
                                            .unwrap_or(&drag_selection),
                                        view.browser.visible_entries(),
                                    );
                                })
                                .ok();
                            cx.new(|_| drag.clone().at(position))
                        });
                    rows.push(
                        div()
                            .id(("entry", index))
                            .debug_selector(move || format!("entry-{index}"))
                            .role(Role::ListItem)
                            .aria_label(accessibility_label)
                            .aria_selected(selected)
                            .aria_position_in_set(index + 1)
                            .aria_size_of_set(count)
                            .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                                accessibility_view
                                    .update(cx, |this, cx| {
                                        this.browser.select(accessibility_path.clone());
                                        this.sync_pinned_preview(cx);
                                        cx.notify();
                                    })
                                    .ok();
                            })
                            .flex()
                            .w_full()
                            .h(px(this.settings.appearance.list_row_height as f32))
                            .items_center()
                            .bg(if selected {
                                palette.selected
                            } else {
                                palette.window
                            })
                            .opacity(if dragging { 0.45 } else { 1.0 })
                            .hover(move |row| row.bg(palette.hover))
                            .child(row_content)
                            .child(
                                div()
                                    .id(("list-marquee-gutter", index))
                                    .debug_selector(move || format!("list-marquee-gutter-{index}"))
                                    .flex_none()
                                    .w(px(SELECTION_MARQUEE_GUTTER))
                                    .h_full(),
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(
                                    move |this, event: &gpui::MouseDownEvent, window, cx| {
                                        window.focus(&this.focus_handle, cx);
                                        this.open_file_context_menu(
                                            context_path.clone(),
                                            is_dir,
                                            event.position,
                                            cx,
                                        );
                                        cx.stop_propagation();
                                    },
                                ),
                            )
                            .on_drag_move::<FileDrag>(cx.listener(
                                |this, event: &gpui::DragMoveEvent<FileDrag>, window, cx| {
                                    let drag = event.drag(cx).clone();
                                    this.track_file_drag(&drag, cx);
                                    this.maybe_start_external_file_drag(
                                        &drag,
                                        event.event.position,
                                        window,
                                        cx,
                                    );
                                },
                            ))
                            .when(is_dir, |row| {
                                let hover_target = path.clone();
                                let leave_target = FileDragHoverTarget::Folder(path.clone());
                                let drop_target = path.clone();
                                let external_drop_target = path.clone();
                                row.drag_over::<FileDrag>(move |style, drag, window, _| {
                                    if valid_file_drop_target(
                                        &path,
                                        drag,
                                        file_drag_operation(window),
                                    ) {
                                        style.border_color(palette.accent).bg(palette.selected)
                                    } else {
                                        style
                                    }
                                })
                                .on_drag_move::<FileDrag>(cx.listener(
                                    move |this,
                                          event: &gpui::DragMoveEvent<FileDrag>,
                                          window,
                                          cx| {
                                        let drag = event.drag(cx).clone();
                                        this.hover_file_drag_folder(
                                            &drag,
                                            hover_target.clone(),
                                            false,
                                            window,
                                            cx,
                                        );
                                    },
                                ))
                                .on_hover(cx.listener(move |this, hovered: &bool, _, _| {
                                    if !*hovered {
                                        this.cancel_file_drag_hover(&leave_target);
                                    }
                                }))
                                .on_drop(cx.listener(move |this, drag: &FileDrag, window, cx| {
                                    cx.stop_propagation();
                                    this.drop_files_to(
                                        drag,
                                        drop_target.clone(),
                                        false,
                                        window,
                                        cx,
                                    );
                                }))
                                .map(|row| {
                                    row.drag_over::<ExternalPaths>(move |style, _, _, _| {
                                        style.border_color(palette.accent).bg(palette.selected)
                                    })
                                    .on_drop(cx.listener(
                                        move |this, paths: &ExternalPaths, window, cx| {
                                            cx.stop_propagation();
                                            this.drop_external_paths_to(
                                                paths,
                                                external_drop_target.clone(),
                                                window.modifiers(),
                                                cx,
                                            );
                                        },
                                    ))
                                })
                            })
                            .into_any_element(),
                    );
                }
                rows
            }),
        )
        .flex_1()
        .w_full()
        .track_scroll(&self.listing.scroll_handle)
        .into_any_element();
        let list = div()
            .id("file-list-accessibility")
            .role(Role::List)
            .aria_label(format!("File list, {count} items"))
            .flex()
            .flex_1()
            .min_h_0()
            .child(list)
            .into_any_element();
        div()
            .id("list-marquee-surface")
            .debug_selector(|| "list-marquee-surface".to_string())
            .relative()
            .flex()
            .flex_1()
            .w_full()
            .min_h_0()
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event, window, cx| {
                    this.begin_selection_marquee(marquee_layout, event, window, cx);
                }),
            )
            .child(list)
            .when_some(marquee_overlay, |surface, rect| {
                surface.child(self.render_selection_marquee_overlay(rect))
            })
            .into_any_element()
    }

    pub(crate) fn render_grid_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let metrics = grid_layout_metrics(
            self.layout.listing_viewport_width,
            self.settings.appearance.grid_min_width,
            self.settings.appearance.density,
            self.settings.appearance.ui_scale,
        );
        let marquee_layout = MarqueeLayout::Grid(metrics);
        if self
            .pointer
            .selection_marquee
            .as_ref()
            .is_some_and(|marquee| marquee.layout != marquee_layout)
        {
            self.pointer.selection_marquee = None;
        }
        self.layout.grid_columns = metrics.columns;
        let marquee_overlay = self.selection_marquee_overlay_rect(marquee_layout);
        let item_count = self.browser.visible_entries().len();
        let row_count = item_count.div_ceil(metrics.columns);
        let list = uniform_list(
            "grid-results",
            row_count,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                #[cfg(test)]
                {
                    this.last_rendered_items = (range.end * metrics.columns).min(item_count)
                        - range.start * metrics.columns;
                    this.render_stats.listing_rows += range.len();
                }
                this.reserve_entry_visuals(range.len() * metrics.columns);
                let mut rows = Vec::with_capacity(range.end - range.start);
                for row_index in range {
                    let mut cards = Vec::with_capacity(metrics.columns);
                    for column_index in 0..metrics.columns {
                        let index = row_index * metrics.columns + column_index;
                        let Some(entry) = this.browser.visible_entries().get(index).cloned() else {
                            continue;
                        };
                        let path = entry.path.clone();
                        let pointer_path = path.clone();
                        let context_path = path.clone();
                        let is_dir = is_folder_like(&entry);
                        let hidden = entry.hidden;
                        let selected = this.browser.is_selected(&path);
                        let dragging = this.pointer.file_drag_sources.contains(&path);
                        let drag_selection = this.browser.selection_snapshot();
                        let drag_view = cx.entity().downgrade();
                        let palette = this.palette;
                        let icon_size = (f32::from(this.settings.appearance.icon_size)
                            * 2.5
                            * this.settings.appearance.ui_scale)
                            .clamp(30.0, 60.0);
                        let thumbnail_max_size = u32::from(
                            (this.settings.appearance.grid_min_width * 2).clamp(128, 512),
                        );
                        let thumbnail = this.cached_entry_thumbnail(&entry, thumbnail_max_size, cx);
                        let native_icon = if thumbnail.is_none() {
                            this.cached_entry_icon(&entry, cx)
                        } else {
                            None
                        };
                        let visual = grid_entry_visual(
                            &entry,
                            thumbnail,
                            native_icon,
                            icon_size,
                            metrics.thumbnail_height,
                            palette,
                        );
                        let detail = format_size(entry.size);
                        let accessibility_label = format!(
                            "{}{}: {}",
                            if hidden { "Hidden " } else { "" },
                            if is_dir { "folder" } else { "file" },
                            file_name(&entry)
                        );
                        let accessibility_path = path.clone();
                        let accessibility_view = cx.entity().downgrade();
                        let card_content = div()
                            .id(("grid-entry-content", index))
                            .when(hidden, |content| {
                                content.debug_selector(move || format!("grid-hidden-entry-{index}"))
                            })
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .w_full()
                            .opacity(if hidden { HIDDEN_ENTRY_OPACITY } else { 1.0 })
                            .child(visual)
                            .child(
                                div()
                                    .w_full()
                                    .text_center()
                                    .whitespace_normal()
                                    .line_clamp(2)
                                    .text_ellipsis()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(file_name(&entry)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .gap_1()
                                    .max_w_full()
                                    .children(entry_tag_dots(
                                        &entry,
                                        format!("grid-entry-tags-{index}"),
                                        (7.0 * palette.scale).clamp(6.0, 10.0),
                                        palette,
                                    ))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .text_color(palette.muted)
                                            .child(detail),
                                    )
                                    .children(entry_cloud_badge(
                                        &entry,
                                        format!("grid-entry-cloud-{index}"),
                                        (12.0 * palette.scale).clamp(10.0, 15.0),
                                        palette,
                                    )),
                            );
                        cards.push(
                            div()
                                .id(("grid-entry", index))
                                .debug_selector(move || format!("grid-entry-{index}"))
                                .role(Role::ListItem)
                                .aria_label(accessibility_label)
                                .aria_selected(selected)
                                .aria_position_in_set(index + 1)
                                .aria_size_of_set(item_count)
                                .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                                    accessibility_view
                                        .update(cx, |this, cx| {
                                            this.browser.select(accessibility_path.clone());
                                            this.sync_pinned_preview(cx);
                                            cx.notify();
                                        })
                                        .ok();
                                })
                                .flex()
                                .flex_col()
                                .flex_none()
                                .w(px(metrics.item_width))
                                .min_w(px(metrics.item_width))
                                .h(px(metrics.item_height))
                                .items_center()
                                .justify_center()
                                .gap_1()
                                .px_2()
                                .border_1()
                                .border_color(if selected {
                                    palette.accent
                                } else {
                                    with_alpha(palette.border, 0.0)
                                })
                                .bg(if selected {
                                    palette.selected
                                } else {
                                    palette.window
                                })
                                .opacity(if dragging { 0.45 } else { 1.0 })
                                .hover(move |card| card.bg(palette.hover))
                                .cursor_pointer()
                                .child(card_content)
                                .when_some(
                                    this.plugin_entry_decoration(&entry.path),
                                    |row, label| {
                                        row.child(
                                            div().text_xs().text_color(palette.accent).child(label),
                                        )
                                    },
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(
                                        move |this, event: &gpui::MouseDownEvent, window, cx| {
                                            window.focus(&this.focus_handle, cx);
                                            if is_secondary_click(event) {
                                                this.open_file_context_menu(
                                                    pointer_path.clone(),
                                                    is_dir,
                                                    event.position,
                                                    cx,
                                                );
                                                cx.stop_propagation();
                                                return;
                                            }
                                            this.select_from_pointer(
                                                pointer_path.clone(),
                                                event,
                                                cx,
                                            );
                                            if event.click_count >= 2 {
                                                this.open_entry(pointer_path.clone(), is_dir, cx);
                                            }
                                            cx.stop_propagation();
                                            cx.notify();
                                        },
                                    ),
                                )
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(
                                        move |this, event: &gpui::MouseDownEvent, window, cx| {
                                            window.focus(&this.focus_handle, cx);
                                            this.open_file_context_menu(
                                                context_path.clone(),
                                                is_dir,
                                                event.position,
                                                cx,
                                            );
                                            cx.stop_propagation();
                                        },
                                    ),
                                )
                                .on_drag(FileDrag::deferred(), move |drag, position, _, cx| {
                                    drag_view
                                        .read_with(cx, |view, _| {
                                            drag.initialize(
                                                &entry,
                                                view.pointer
                                                    .file_drag_selection
                                                    .as_ref()
                                                    .unwrap_or(&drag_selection),
                                                view.browser.visible_entries(),
                                            );
                                        })
                                        .ok();
                                    cx.new(|_| drag.clone().at(position))
                                })
                                .on_drag_move::<FileDrag>(cx.listener(
                                    |this, event: &gpui::DragMoveEvent<FileDrag>, window, cx| {
                                        let drag = event.drag(cx).clone();
                                        this.track_file_drag(&drag, cx);
                                        this.maybe_start_external_file_drag(
                                            &drag,
                                            event.event.position,
                                            window,
                                            cx,
                                        );
                                    },
                                ))
                                .when(is_dir, |card| {
                                    let hover_target = path.clone();
                                    let leave_target = FileDragHoverTarget::Folder(path.clone());
                                    let drop_target = path.clone();
                                    let external_drop_target = path.clone();
                                    card.drag_over::<FileDrag>(move |style, drag, window, _| {
                                        if valid_file_drop_target(
                                            &path,
                                            drag,
                                            file_drag_operation(window),
                                        ) {
                                            style.border_color(palette.accent).bg(palette.selected)
                                        } else {
                                            style
                                        }
                                    })
                                    .on_drag_move::<FileDrag>(cx.listener(
                                        move |this,
                                              event: &gpui::DragMoveEvent<FileDrag>,
                                              window,
                                              cx| {
                                            let drag = event.drag(cx).clone();
                                            this.hover_file_drag_folder(
                                                &drag,
                                                hover_target.clone(),
                                                false,
                                                window,
                                                cx,
                                            );
                                        },
                                    ))
                                    .on_hover(cx.listener(move |this, hovered: &bool, _, _| {
                                        if !*hovered {
                                            this.cancel_file_drag_hover(&leave_target);
                                        }
                                    }))
                                    .on_drop(cx.listener(
                                        move |this, drag: &FileDrag, window, cx| {
                                            cx.stop_propagation();
                                            this.drop_files_to(
                                                drag,
                                                drop_target.clone(),
                                                false,
                                                window,
                                                cx,
                                            );
                                        },
                                    ))
                                    .map(|card| {
                                        card.drag_over::<ExternalPaths>(move |style, _, _, _| {
                                            style.border_color(palette.accent).bg(palette.selected)
                                        })
                                        .on_drop(
                                            cx.listener(
                                                move |this, paths: &ExternalPaths, window, cx| {
                                                    cx.stop_propagation();
                                                    this.drop_external_paths_to(
                                                        paths,
                                                        external_drop_target.clone(),
                                                        window.modifiers(),
                                                        cx,
                                                    );
                                                },
                                            ),
                                        )
                                    })
                                })
                                .into_any_element(),
                        );
                    }
                    rows.push(
                        div()
                            .id(("grid-row", row_index))
                            .flex()
                            .w_full()
                            .h(px(metrics.row_height))
                            .gap(px(metrics.gap))
                            .px_2()
                            .children(cards)
                            .into_any_element(),
                    );
                }
                rows
            }),
        )
        .flex_1()
        .w_full()
        .track_scroll(&self.listing.scroll_handle)
        .into_any_element();
        let list = div()
            .id("file-grid-accessibility")
            .role(Role::List)
            .aria_label(format!("File grid, {item_count} items"))
            .flex()
            .flex_1()
            .min_h_0()
            .child(list)
            .into_any_element();
        div()
            .id("grid-marquee-surface")
            .debug_selector(|| "grid-marquee-surface".to_string())
            .relative()
            .flex()
            .flex_1()
            .w_full()
            .min_h_0()
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event, window, cx| {
                    this.begin_selection_marquee(marquee_layout, event, window, cx);
                }),
            )
            .child(list)
            .when_some(marquee_overlay, |surface, rect| {
                surface.child(self.render_selection_marquee_overlay(rect))
            })
            .into_any_element()
    }

    pub(crate) fn render_column_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let snapshots: Vec<_> = self
            .column_view
            .columns
            .columns()
            .iter()
            .enumerate()
            .map(|(index, column)| {
                (
                    index,
                    column.path().to_path_buf(),
                    column.loading(),
                    column.error().map(str::to_string),
                    column.visible_entries(&self.browser).len(),
                )
            })
            .collect();
        let column_count = snapshots.len();
        if self
            .pointer
            .selection_marquee
            .as_ref()
            .is_some_and(|marquee| {
                matches!(
                    marquee.layout,
                    MarqueeLayout::Column { index, .. } if index >= column_count
                )
            })
        {
            self.pointer.selection_marquee = None;
        }
        let mut rendered_columns = Vec::with_capacity(column_count + 2);
        let mut column_widths = Vec::with_capacity(column_count);

        for (column_index, column_path, loading, error, entry_count) in snapshots {
            let column_width = self
                .browser
                .column_view_width(&column_path)
                .map(f32::from)
                .unwrap_or(DEFAULT_COLUMN_VIEW_WIDTH);
            column_widths.push(column_width);
            let marquee_layout = MarqueeLayout::Column {
                index: column_index,
                row_height: f32::from(self.settings.appearance.list_row_height),
            };
            let column_row_height = f32::from(self.settings.appearance.list_row_height);
            let handle = self
                .column_view
                .scroll_handles
                .get(column_index)
                .cloned()
                .unwrap_or_else(UniformListScrollHandle::new);
            let title = column_path
                .file_name()
                .unwrap_or_else(|| column_path.as_os_str())
                .to_string_lossy()
                .into_owned();
            let state_label = if loading {
                "Loading…".to_string()
            } else {
                format!("{entry_count} items")
            };
            let list_path = column_path.clone();
            let column_accessibility_label = format!("{title} column, {entry_count} items");

            let body = if entry_count == 0 {
                if error.is_none() && !loading {
                    self.render_empty_listing(("column-empty", column_index), true, cx)
                } else {
                    div()
                        .flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .p_4()
                        .text_sm()
                        .text_color(if error.is_some() {
                            rgb(0xff8f8f)
                        } else {
                            rgb(0x909090)
                        })
                        .child(error.unwrap_or_else(|| "Loading files…".to_string()))
                        .into_any_element()
                }
            } else {
                let list = uniform_list(
                    ("column-list", column_index),
                    entry_count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        let Some(column) = this.column_view.columns.columns().get(column_index)
                        else {
                            return Vec::new();
                        };
                        if column.path() != list_path.as_path() {
                            return Vec::new();
                        }
                        let entries = column.visible_entries(&this.browser);
                        let child_path = this
                            .column_view
                            .columns
                            .active_child_path(column_index)
                            .map(PathBuf::from);
                        let is_last = column_index + 1 == this.column_view.columns.columns().len();
                        this.reserve_entry_visuals(range.len());
                        let mut rows = Vec::with_capacity(range.end - range.start);
                        for index in range {
                            let Some(entry) = entries.get(index).cloned() else {
                                continue;
                            };
                            let path = entry.path.clone();
                            let pointer_path = path.clone();
                            let context_path = path.clone();
                            let is_dir = is_folder_like(&entry);
                            let hidden = entry.hidden;
                            let dragging = this.pointer.file_drag_sources.contains(&path);
                            let drag_view = cx.entity().downgrade();
                            let palette = this.palette;
                            let icon_size =
                                (this.settings.appearance.icon_size as f32 + 4.0).clamp(16.0, 28.0);
                            let native_icon = this.cached_entry_icon(&entry, cx);
                            let icon =
                                entry_icon_visual(&entry, native_icon, icon_size, this.palette);
                            let detail = entry_detail(&entry, this.calculate_folder_sizes, false);
                            let selected = this.column_view.selection.contains(&path)
                                || (is_last && this.browser.is_selected(&path));
                            let branch_active = !is_last && child_path.as_ref() == Some(&path);
                            let highlighted = selected || branch_active;
                            let accessibility_label = format!(
                                "{}{}: {}",
                                if hidden { "Hidden " } else { "" },
                                if is_dir { "folder" } else { "file" },
                                file_name(&entry)
                            );
                            let accessibility_path = path.clone();
                            let accessibility_view = cx.entity().downgrade();
                            let row_content = div()
                                .id(("column-entry-drag-source", column_index * 1_000_000 + index))
                                .when(highlighted, |row| {
                                    row.debug_selector(move || {
                                        format!("column-entry-selected-{column_index}-{index}")
                                    })
                                })
                                .flex()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .opacity(if hidden { HIDDEN_ENTRY_OPACITY } else { 1.0 })
                                .child(icon)
                                .child(
                                    div()
                                        .id((
                                            "column-entry-label",
                                            column_index * 1_000_000 + index,
                                        ))
                                        .when(hidden, |label| {
                                            label.debug_selector(move || {
                                                format!(
                                                    "column-hidden-entry-{column_index}-{index}"
                                                )
                                            })
                                        })
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .child(file_name(&entry)),
                                )
                                .when_some(
                                    this.plugin_entry_decoration(&entry.path),
                                    |row, label| {
                                        row.child(
                                            div().text_xs().text_color(palette.accent).child(label),
                                        )
                                    },
                                )
                                .when(!detail.is_empty(), |row| {
                                    row.child(
                                        div().text_xs().text_color(rgb(0x888888)).child(detail),
                                    )
                                })
                                .when(is_dir, |row| {
                                    row.child(toolbar_icon(
                                        "arrow-right",
                                        palette.icon_size.clamp(10.0, 13.0),
                                        palette.tertiary,
                                    ))
                                })
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(
                                        move |this, event: &gpui::MouseDownEvent, window, cx| {
                                            window.focus(&this.focus_handle, cx);
                                            if is_secondary_click(event) {
                                                this.activate_column(column_index, cx);
                                                this.column_view.pending_selection = None;
                                                this.open_file_context_menu(
                                                    pointer_path.clone(),
                                                    is_dir,
                                                    event.position,
                                                    cx,
                                                );
                                                cx.stop_propagation();
                                                return;
                                            }
                                            let extend = event.modifiers.shift
                                                || event.modifiers.control
                                                || event.modifiers.platform;
                                            if !is_dir || extend {
                                                this.activate_column(column_index, cx);
                                            }
                                            this.column_view.pending_selection = None;
                                            if is_last || !is_dir || extend {
                                                this.select_from_pointer(
                                                    pointer_path.clone(),
                                                    event,
                                                    cx,
                                                );
                                                this.sync_column_selection_from_browser();
                                            }
                                            if is_dir && !extend {
                                                this.navigate_to(pointer_path.clone(), cx);
                                            } else if !is_dir && !extend && event.click_count >= 2 {
                                                this.open_entry(pointer_path.clone(), false, cx);
                                            } else {
                                                this.sync_pinned_preview(cx);
                                            }
                                            cx.stop_propagation();
                                            cx.notify();
                                        },
                                    ),
                                )
                                .on_drag(FileDrag::deferred(), move |drag, position, _, cx| {
                                    drag_view
                                        .read_with(cx, |view, _| {
                                            view.initialize_column_file_drag(drag, &entry);
                                        })
                                        .ok();
                                    cx.new(|_| drag.clone().at(position))
                                });
                            rows.push(
                                div()
                                    .id(("column-entry", column_index * 1_000_000 + index))
                                    .debug_selector(move || {
                                        format!("column-entry-{column_index}-{index}")
                                    })
                                    .role(Role::ListItem)
                                    .aria_label(accessibility_label)
                                    .aria_selected(highlighted)
                                    .aria_position_in_set(index + 1)
                                    .aria_size_of_set(entry_count)
                                    .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                                        accessibility_view
                                            .update(cx, |this, cx| {
                                                if !is_dir {
                                                    this.activate_column(column_index, cx);
                                                }
                                                this.column_view.pending_selection = None;
                                                if is_last || !is_dir {
                                                    this.browser.select(accessibility_path.clone());
                                                    this.sync_column_selection_from_browser();
                                                }
                                                if is_dir {
                                                    this.navigate_to(
                                                        accessibility_path.clone(),
                                                        cx,
                                                    );
                                                } else {
                                                    this.sync_pinned_preview(cx);
                                                }
                                                cx.notify();
                                            })
                                            .ok();
                                    })
                                    .flex()
                                    .w_full()
                                    .h(px(column_row_height))
                                    .items_center()
                                    .px_2()
                                    .border_l_2()
                                    .border_color(if branch_active {
                                        palette.accent
                                    } else {
                                        with_alpha(palette.border, 0.0)
                                    })
                                    .bg(if selected {
                                        palette.selected
                                    } else if branch_active {
                                        with_alpha(palette.accent, 0.12)
                                    } else {
                                        palette.window
                                    })
                                    .opacity(if dragging { 0.45 } else { 1.0 })
                                    .hover(move |row| row.bg(palette.hover))
                                    .child(row_content)
                                    .child(
                                        div()
                                            .id((
                                                "column-marquee-gutter",
                                                column_index * 1_000_000 + index,
                                            ))
                                            .debug_selector(move || {
                                                format!(
                                                    "column-marquee-gutter-{column_index}-{index}"
                                                )
                                            })
                                            .flex_none()
                                            .w(px(SELECTION_MARQUEE_GUTTER))
                                            .h_full(),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener(
                                            move |this,
                                                  event: &gpui::MouseDownEvent,
                                                  window,
                                                  cx| {
                                                window.focus(&this.focus_handle, cx);
                                                this.activate_column(column_index, cx);
                                                this.column_view.pending_selection = None;
                                                this.open_file_context_menu(
                                                    context_path.clone(),
                                                    is_dir,
                                                    event.position,
                                                    cx,
                                                );
                                                this.sync_column_selection_from_browser();
                                                this.sync_pinned_preview(cx);
                                                cx.stop_propagation();
                                            },
                                        ),
                                    )
                                    .on_drag_move::<FileDrag>(cx.listener(
                                        |this,
                                         event: &gpui::DragMoveEvent<FileDrag>,
                                         window,
                                         cx| {
                                            let drag = event.drag(cx).clone();
                                            this.track_file_drag(&drag, cx);
                                            this.maybe_start_external_file_drag(
                                                &drag,
                                                event.event.position,
                                                window,
                                                cx,
                                            );
                                        },
                                    ))
                                    .when(is_dir, |row| {
                                        let hover_target = path.clone();
                                        let leave_target =
                                            FileDragHoverTarget::Folder(path.clone());
                                        let drop_target = path.clone();
                                        let external_drop_target = path.clone();
                                        row.drag_over::<FileDrag>(move |style, drag, window, _| {
                                            if valid_file_drop_target(
                                                &path,
                                                drag,
                                                file_drag_operation(window),
                                            ) {
                                                style
                                                    .border_color(palette.accent)
                                                    .bg(palette.selected)
                                            } else {
                                                style
                                            }
                                        })
                                        .on_drag_move::<FileDrag>(cx.listener(
                                            move |this,
                                                  event: &gpui::DragMoveEvent<FileDrag>,
                                                  window,
                                                  cx| {
                                                let drag = event.drag(cx).clone();
                                                this.hover_file_drag_folder(
                                                    &drag,
                                                    hover_target.clone(),
                                                    false,
                                                    window,
                                                    cx,
                                                );
                                            },
                                        ))
                                        .on_hover(cx.listener(move |this, hovered: &bool, _, _| {
                                            if !*hovered {
                                                this.cancel_file_drag_hover(&leave_target);
                                            }
                                        }))
                                        .on_drop(cx.listener(
                                            move |this, drag: &FileDrag, window, cx| {
                                                cx.stop_propagation();
                                                this.drop_files_to(
                                                    drag,
                                                    drop_target.clone(),
                                                    false,
                                                    window,
                                                    cx,
                                                );
                                            },
                                        ))
                                        .map(|row| {
                                            row.drag_over::<ExternalPaths>(move |style, _, _, _| {
                                                style
                                                    .border_color(palette.accent)
                                                    .bg(palette.selected)
                                            })
                                            .on_drop(cx.listener(
                                                move |this, paths: &ExternalPaths, window, cx| {
                                                    cx.stop_propagation();
                                                    this.drop_external_paths_to(
                                                        paths,
                                                        external_drop_target.clone(),
                                                        window.modifiers(),
                                                        cx,
                                                    );
                                                },
                                            ))
                                        })
                                    })
                                    .into_any_element(),
                            );
                        }
                        rows
                    }),
                )
                .flex_1()
                .w_full()
                .track_scroll(&handle)
                .into_any_element();
                div()
                    .id(("column-accessibility-list", column_index))
                    .role(Role::List)
                    .aria_label(column_accessibility_label)
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .into_any_element()
            };

            let marquee_overlay = self.selection_marquee_overlay_rect(marquee_layout);
            let body = div()
                .id(("column-marquee-surface", column_index))
                .debug_selector(move || format!("column-marquee-surface-{column_index}"))
                .relative()
                .flex()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event, window, cx| {
                        this.begin_selection_marquee(marquee_layout, event, window, cx);
                    }),
                )
                .child(body)
                .when_some(marquee_overlay, |surface, rect| {
                    surface.child(self.render_selection_marquee_overlay(rect))
                })
                .into_any_element();

            let column_drop_target = column_path.clone();
            let column_drag_target = column_path.clone();
            let resize_handle = self.render_column_view_resize_handle(
                column_index,
                column_path.clone(),
                column_width,
                cx,
            );
            let palette = self.palette;
            rendered_columns.push(
                div()
                    .id(("column", column_index))
                    .debug_selector(move || format!("column-{column_index}"))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            window.focus(&this.focus_handle, cx);
                            this.activate_column(column_index, cx);
                            this.column_view.pending_selection = None;
                            this.browser.clear_selection();
                            this.column_view.selection.clear();
                            this.open_empty_context_menu(event.position, cx);
                            this.sync_pinned_preview(cx);
                            cx.stop_propagation();
                        }),
                    )
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .w(px(column_width))
                    .h_full()
                    .border_r_1()
                    .border_color(palette.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .h(px(32.0))
                            .px_2()
                            .border_b_1()
                            .border_color(palette.border)
                            .bg(palette.surface)
                            .child(div().flex_1().min_w_0().truncate().text_sm().child(title))
                            .child(div().text_xs().text_color(palette.muted).child(state_label)),
                    )
                    .child(body)
                    .child(resize_handle)
                    .drag_over::<FileDrag>(move |style, drag, window, _| {
                        if valid_file_drop_target(
                            &column_drag_target,
                            drag,
                            file_drag_operation(window),
                        ) {
                            style.bg(with_alpha(palette.accent, 0.08))
                        } else {
                            style
                        }
                    })
                    .on_drag_move::<FileDrag>(cx.listener(
                        move |this, event: &gpui::DragMoveEvent<FileDrag>, window, cx| {
                            let drag = event.drag(cx).clone();
                            this.track_file_drag(&drag, cx);
                            this.maybe_start_external_file_drag(
                                &drag,
                                event.event.position,
                                window,
                                cx,
                            );
                            this.auto_scroll_column_file_drag(column_index, event);
                        },
                    ))
                    .on_drop(cx.listener(move |this, drag: &FileDrag, window, cx| {
                        this.drop_files_to(drag, column_drop_target.clone(), false, window, cx);
                    }))
                    .into_any_element(),
            );
        }

        let show_preview = self.settings.view.show_preview_panel && !self.quick_look.open;
        let alignment = leaf_alignment(
            &column_widths,
            if show_preview {
                self.layout.preview_panel_width * self.palette.scale
            } else {
                0.0
            },
            f32::from(self.column_view.strip_scroll.bounds().size.width),
        );
        let trailing_fill = alignment.map_or(0.0, |alignment| alignment.trailing_fill);
        self.column_view.preview_fill = if show_preview { trailing_fill } else { 0.0 };
        if show_preview {
            rendered_columns.push(if matches!(self.preview.state, PreviewState::Closed) {
                self.render_preview_summary_panel(true, true, cx)
            } else {
                self.render_preview_panel(true, false, true, cx)
            });
        } else if trailing_fill > 0.0 {
            rendered_columns.push(
                div()
                    .flex_shrink_0()
                    .w(px(trailing_fill))
                    .h_full()
                    .into_any_element(),
            );
        }
        let rendered_column_count = rendered_columns.len();

        if self.column_view.scroll_to_leaf_attempts > 0 && rendered_column_count > 0 {
            let max_offset = self.column_view.strip_scroll.max_offset().x;
            if let Some(alignment) = alignment {
                // Land on a column boundary rather than the far end, so the
                // leftmost visible column is never cut off.
                let offset = self.column_view.strip_scroll.offset();
                self.column_view
                    .strip_scroll
                    .set_offset(gpui::point(px(-alignment.offset), offset.y));
            } else if max_offset > px(0.0) {
                let offset = self.column_view.strip_scroll.offset();
                self.column_view
                    .strip_scroll
                    .set_offset(gpui::point(-max_offset, offset.y));
            } else {
                self.column_view
                    .strip_scroll
                    .scroll_to_item(rendered_column_count - 1);
            }
            self.column_view.scroll_to_leaf_attempts -= 1;
            if self.column_view.scroll_to_leaf_attempts > 0 {
                // A notification during render would reach neither the
                // listing pane nor the frame scheduler; retry after it.
                let this = cx.weak_entity();
                cx.defer(move |cx| {
                    this.update(cx, |_, cx| cx.notify()).ok();
                });
            }
        }

        let mut column_results = div()
            .id("column-results")
            .flex()
            .flex_1()
            .w_full()
            .min_w_0()
            .overflow_x_scroll()
            .track_scroll(&self.column_view.strip_scroll)
            .children(rendered_columns)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle, cx);
                    this.open_empty_context_menu(event.position, cx);
                }),
            );
        column_results.style().restrict_scroll_to_axis = Some(true);
        column_results.into_any_element()
    }
}
