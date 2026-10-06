//! `DirectoryWindow` behavior for selection.

use crate::*;

impl DirectoryWindow {
    pub(crate) fn select_next(&mut self, cx: &mut Context<Self>) {
        if self.quick_look.open {
            self.navigate_preview(1, cx);
            return;
        }
        let selected = if self.browser.view_mode() == ViewMode::Grid {
            self.browser
                .select_offset(self.layout.grid_columns as isize)
        } else {
            self.browser.select_next()
        };
        self.apply_keyboard_selection(selected, false, cx);
    }

    pub(crate) fn select_previous(&mut self, cx: &mut Context<Self>) {
        if self.quick_look.open {
            self.navigate_preview(-1, cx);
            return;
        }
        let selected = if self.browser.view_mode() == ViewMode::Grid {
            self.browser
                .select_offset(-(self.layout.grid_columns as isize))
        } else {
            self.browser.select_previous()
        };
        self.apply_keyboard_selection(selected, false, cx);
    }

    pub(crate) fn select_next_range(&mut self, cx: &mut Context<Self>) {
        let selected = if self.browser.view_mode() == ViewMode::Grid {
            self.browser
                .select_range_offset(self.layout.grid_columns as isize)
        } else {
            self.browser.select_next_range()
        };
        self.apply_keyboard_selection(selected, true, cx);
    }

    pub(crate) fn select_previous_range(&mut self, cx: &mut Context<Self>) {
        let selected = if self.browser.view_mode() == ViewMode::Grid {
            self.browser
                .select_range_offset(-(self.layout.grid_columns as isize))
        } else {
            self.browser.select_previous_range()
        };
        self.apply_keyboard_selection(selected, true, cx);
    }

    pub(crate) fn select_by_offset(&mut self, offset: isize, extend: bool, cx: &mut Context<Self>) {
        let selected = if extend {
            self.browser.select_range_offset(offset)
        } else {
            self.browser.select_offset(offset)
        };
        self.apply_keyboard_selection(selected, extend, cx);
    }

    pub(crate) fn apply_keyboard_selection(
        &mut self,
        selected: Option<usize>,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = selected {
            self.reveal_selected(index);
            self.sync_column_selection_from_browser();
            if self.browser.view_mode() == ViewMode::Column
                || extend
                || !matches!(self.preview.state, PreviewState::Closed)
                || self.settings.view.show_preview_panel
            {
                self.sync_pinned_preview_after_keyboard(cx);
            }
            cx.notify();
        }
    }

    pub(crate) fn set_column_selection(&mut self, path: PathBuf) {
        self.column_view.selection.clear();
        self.column_view.selection.insert(path);
    }

    pub(crate) fn sync_column_selection_from_browser(&mut self) {
        self.column_view.selection = self.browser.selected_paths().into_iter().collect();
    }

    pub(crate) fn effective_selected_entries(&self) -> Vec<FileEntry> {
        if self.browser.view_mode() != ViewMode::Column || self.column_view.selection.is_empty() {
            return self.browser.selected_entries();
        }
        let mut selected = Vec::new();
        for column in self.column_view.columns.columns() {
            selected.extend(
                column
                    .visible_entries(&self.browser)
                    .iter()
                    .filter(|entry| self.column_view.selection.contains(&entry.path))
                    .map(|entry| entry.as_ref().clone()),
            );
        }
        selected
    }

    pub(crate) fn effective_selected_paths(&self) -> Vec<PathBuf> {
        self.effective_selected_entries()
            .into_iter()
            .map(|entry| entry.path)
            .collect()
    }

    pub(crate) fn effective_selected_entry(&self) -> Option<FileEntry> {
        if self.browser.view_mode() != ViewMode::Column || self.column_view.selection.is_empty() {
            return self.browser.selected_entry().cloned();
        }
        if self.column_view.selection.len() == 1
            && let Some(path) = self.column_view.selection.first()
        {
            // Each path belongs to at most one column: the one listing its parent.
            return self
                .column_view
                .columns
                .columns()
                .iter()
                .find_map(|column| column.visible_entry(&self.browser, path))
                .map(|entry| entry.as_ref().clone());
        }
        let mut entries = self.effective_selected_entries().into_iter();
        let entry = entries.next()?;
        entries.next().is_none().then_some(entry)
    }

    pub(crate) fn effective_selection_count(&self) -> usize {
        if self.browser.view_mode() == ViewMode::Column && !self.column_view.selection.is_empty() {
            self.column_view.selection.len()
        } else {
            self.browser.selection_count()
        }
    }

    pub(crate) fn is_effectively_selected(&self, path: &Path) -> bool {
        if self.browser.view_mode() == ViewMode::Column && !self.column_view.selection.is_empty() {
            self.column_view.selection.contains(path)
        } else {
            self.browser.is_selected(path)
        }
    }

    pub(crate) fn select_from_pointer(
        &mut self,
        path: PathBuf,
        event: &gpui::MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        self.context_menu.menu = None;
        // Keep the mouse-down selection even if a redraw occurs before drag start.
        self.pointer.file_drag_selection = Some(self.browser.selection_snapshot());
        if event.modifiers.shift {
            self.browser.select_range_to(path);
        } else if event.modifiers.control || event.modifiers.platform {
            self.browser.toggle_selection(path);
        } else {
            self.browser.select(path);
        }
        self.sync_pinned_preview(cx);
    }

    pub(crate) fn marquee_viewport(
        &self,
        layout: MarqueeLayout,
    ) -> Option<(GridRect, f32, ScrollHandle)> {
        let scroll_handle = match layout {
            MarqueeLayout::List { .. } | MarqueeLayout::Grid(_) => {
                self.listing.scroll_handle.0.borrow().base_handle.clone()
            }
            MarqueeLayout::Column { index, .. } => self
                .column_view
                .scroll_handles
                .get(index)?
                .0
                .borrow()
                .base_handle
                .clone(),
        };
        let bounds = scroll_handle.bounds();
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        Some((
            GridRect {
                left: f32::from(bounds.origin.x),
                top: f32::from(bounds.origin.y),
                right: f32::from(bounds.origin.x) + width,
                bottom: f32::from(bounds.origin.y) + height,
            },
            -f32::from(scroll_handle.offset().y),
            scroll_handle,
        ))
    }

    pub(crate) fn marquee_paths(&self, layout: MarqueeLayout) -> Vec<PathBuf> {
        match layout {
            MarqueeLayout::List { .. } | MarqueeLayout::Grid(_) => self
                .browser
                .visible_entries()
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
            MarqueeLayout::Column { index, .. } => self
                .column_view
                .columns
                .columns()
                .get(index)
                .map(|column| {
                    column
                        .visible_entries(&self.browser)
                        .iter()
                        .map(|entry| entry.path.clone())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    pub(crate) fn begin_selection_marquee(
        &mut self,
        layout: MarqueeLayout,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        if let MarqueeLayout::Column { index, .. } = layout {
            self.activate_column(index, cx);
            self.column_view.pending_selection = None;
        }
        let Some((viewport, scroll_top, _)) = self.marquee_viewport(layout) else {
            if matches!(layout, MarqueeLayout::Column { .. }) {
                self.browser.clear_selection();
                self.column_view.selection.clear();
                self.sync_pinned_preview(cx);
                cx.notify();
            }
            return;
        };
        let window_point = GridPoint {
            x: f32::from(event.position.x),
            y: f32::from(event.position.y),
        };
        let content_point = GridPoint {
            x: window_point.x - viewport.left,
            y: window_point.y - viewport.top + scroll_top,
        };
        let additive = event.modifiers.control || event.modifiers.platform;
        self.pointer.selection_marquee = Some(SelectionMarquee {
            start_window: window_point,
            start_content: content_point,
            current_content: content_point,
            activated: false,
            additive,
            layout,
            initial_selection: if additive {
                self.effective_selected_paths().into_iter().collect()
            } else {
                BTreeSet::new()
            },
            paths: self.marquee_paths(layout),
        });
        cx.notify();
    }

    pub(crate) fn update_selection_marquee(
        &mut self,
        event: &gpui::MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() || self.pointer.selection_marquee.is_none() {
            return;
        }
        let layout = self
            .pointer
            .selection_marquee
            .as_ref()
            .expect("checked above")
            .layout;
        let Some((viewport, _, scroll_handle)) = self.marquee_viewport(layout) else {
            return;
        };
        let pointer_y = f32::from(event.position.y) - viewport.top;
        let scroll_delta = selection_marquee_scroll_delta(pointer_y, viewport.height());
        if scroll_delta != 0.0 {
            let offset = scroll_handle.offset();
            let max_offset = f32::from(scroll_handle.max_offset().y);
            let next_y = (f32::from(offset.y) + scroll_delta).clamp(-max_offset, 0.0);
            if next_y != f32::from(offset.y) {
                scroll_handle.set_offset(gpui::point(offset.x, px(next_y)));
            }
        }
        let scroll_top = -f32::from(scroll_handle.offset().y);
        let window_point = GridPoint {
            x: f32::from(event.position.x),
            y: f32::from(event.position.y),
        };
        let content_point = GridPoint {
            x: window_point.x - viewport.left,
            y: window_point.y - viewport.top + scroll_top,
        };
        let selection = {
            let marquee = self
                .pointer
                .selection_marquee
                .as_mut()
                .expect("checked above");
            marquee.update(window_point, content_point);
            if !marquee.activated {
                None
            } else {
                let hits = match marquee.layout {
                    MarqueeLayout::Grid(metrics) => grid_marquee_hit_indices(
                        marquee.content_rect(),
                        marquee.paths.len(),
                        metrics,
                    ),
                    MarqueeLayout::List { row_height }
                    | MarqueeLayout::Column { row_height, .. } => row_marquee_hit_indices(
                        marquee.content_rect(),
                        marquee.paths.len(),
                        row_height,
                        viewport.width(),
                    ),
                };
                let paths = hits
                    .into_iter()
                    .filter_map(|index| marquee.paths.get(index).cloned());
                if marquee.additive {
                    Some(
                        marquee
                            .initial_selection
                            .iter()
                            .cloned()
                            .chain(paths)
                            .collect::<Vec<PathBuf>>(),
                    )
                } else {
                    Some(paths.collect::<Vec<PathBuf>>())
                }
            }
        };
        if let Some(selection) = selection {
            if let MarqueeLayout::Column { index, .. } = layout {
                self.column_view.selection = selection.iter().cloned().collect();
                if index + 1 == self.column_view.columns.columns().len() {
                    self.browser.replace_selection(selection);
                } else {
                    self.browser.clear_selection();
                }
            } else {
                self.browser.replace_selection(selection);
            }
        }
        cx.notify();
    }

    pub(crate) fn finish_selection_marquee(&mut self, cx: &mut Context<Self>) {
        let Some(marquee) = self.pointer.selection_marquee.take() else {
            return;
        };
        if !marquee.activated {
            self.browser.clear_selection();
            if matches!(marquee.layout, MarqueeLayout::Column { .. }) {
                self.column_view.selection.clear();
            }
        }
        self.sync_pinned_preview(cx);
        cx.notify();
    }

    pub(crate) fn cancel_selection_marquee(&mut self, cx: &mut Context<Self>) {
        if self.pointer.selection_marquee.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn handle_type_select_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.control
            || event.keystroke.modifiers.alt
            || event.keystroke.modifiers.platform
        {
            return;
        }
        if event.keystroke.key == "backspace" && crate::shortcut::BACKSPACE_GOES_UP {
            self.go_up(cx);
            cx.stop_propagation();
            return;
        }
        if self.browser.view_mode() == ViewMode::Grid {
            let key_char = event.keystroke.key_char.as_deref();
            if matches!(event.keystroke.key.as_str(), "+" | "=")
                || matches!(key_char, Some("+") | Some("="))
            {
                self.adjust_grid_width(10, cx);
                cx.stop_propagation();
                return;
            }
            if event.keystroke.key == "-" || key_char == Some("-") {
                self.adjust_grid_width(-10, cx);
                cx.stop_propagation();
                return;
            }
        }
        let Some(text) = event.keystroke.key_char.as_deref() else {
            return;
        };
        if text.chars().any(char::is_control) || text.chars().all(char::is_whitespace) {
            return;
        }
        let now = Instant::now();
        if self
            .listing
            .type_select_at
            .is_none_or(|previous| now.duration_since(previous) > Duration::from_millis(750))
        {
            self.listing.type_select_value.clear();
        }
        self.listing
            .type_select_value
            .push_str(&text.to_lowercase());
        self.listing.type_select_at = Some(now);
        let selected = self
            .browser
            .select_prefix(&self.listing.type_select_value)
            .or_else(|| self.browser.select_prefix(&text.to_lowercase()));
        if let Some(index) = selected {
            self.reveal_selected(index);
            self.sync_column_selection_from_browser();
            self.sync_pinned_preview_after_keyboard(cx);
            cx.stop_propagation();
            cx.notify();
        }
    }
}

/// Whether a left mouse-down on an item asks for its context menu instead
/// of selecting: Control-click on macOS, as in Finder.
pub(crate) fn is_secondary_click(event: &gpui::MouseDownEvent) -> bool {
    let modifiers = event.modifiers;
    cfg!(target_os = "macos")
        && modifiers.control
        && !modifiers.shift
        && !modifiers.alt
        && !modifiers.platform
}
