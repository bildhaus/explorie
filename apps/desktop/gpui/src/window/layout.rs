//! `DirectoryWindow` behavior for layout.

use crate::*;

/// The window's geometry: sidebar and preview-panel widths with their
/// keyboard resize handles, the width and grid columns the listing last laid
/// out with, and the window bounds to remember or restore.
pub(crate) struct WindowLayout {
    pub(crate) sidebar_width: f32,
    pub(crate) sidebar_collapsed: bool,
    pub(crate) sidebar_resize_focus: FocusHandle,
    pub(crate) preview_panel_width: f32,
    pub(crate) preview_panel_resize_focus: FocusHandle,
    pub(crate) listing_viewport_width: f32,
    pub(crate) grid_columns: usize,
    pub(crate) last_window_bounds: WorkspaceWindowState,
    pub(crate) pending_workspace_bounds: Option<WorkspaceWindowState>,
}

impl DirectoryWindow {
    pub(crate) fn begin_sidebar_resize(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.layout.sidebar_collapsed {
            self.layout.sidebar_collapsed = false;
        }
        window.focus(&self.layout.sidebar_resize_focus, cx);
        self.pointer.sidebar_resize = Some(SidebarResize {
            start_x: f32::from(event.position.x),
            start_width: self.layout.sidebar_width,
        });
        cx.notify();
    }

    pub(crate) fn update_sidebar_resize(
        &mut self,
        event: &gpui::MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            return;
        }
        let Some(resize) = self.pointer.sidebar_resize else {
            return;
        };
        let delta = f32::from(event.position.x) - resize.start_x;
        let width = (resize.start_width + delta).clamp(160.0, 480.0);
        if (width - self.layout.sidebar_width).abs() >= f32::EPSILON {
            self.layout.sidebar_width = width;
            cx.notify();
        }
    }

    pub(crate) fn finish_sidebar_resize(&mut self, cx: &mut Context<Self>) {
        if self.pointer.sidebar_resize.take().is_none() {
            return;
        }
        self.settings.view.sidebar_width = self.layout.sidebar_width;
        self.finish_settings_change(
            format!(
                "Sidebar width: {} px",
                self.layout.sidebar_width.round() as u16
            ),
            cx,
        );
    }

    pub(crate) fn adjust_sidebar_width(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.layout.sidebar_collapsed = false;
        self.layout.sidebar_width = (self.layout.sidebar_width + delta).clamp(160.0, 480.0);
        self.settings.view.sidebar_width = self.layout.sidebar_width;
        self.finish_settings_change(
            format!(
                "Sidebar width: {} px",
                self.layout.sidebar_width.round() as u16
            ),
            cx,
        );
    }

    pub(crate) fn handle_sidebar_resize_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "left" => self.adjust_sidebar_width(-10.0, cx),
            "right" => self.adjust_sidebar_width(10.0, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(crate) fn begin_preview_panel_resize(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.layout.preview_panel_resize_focus, cx);
        self.pointer.preview_panel_resize = Some(PreviewPanelResize {
            start_x: f32::from(event.position.x),
            start_width: self.layout.preview_panel_width,
        });
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn update_preview_panel_resize(
        &mut self,
        event: &gpui::MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            return;
        }
        let Some(resize) = self.pointer.preview_panel_resize else {
            return;
        };
        let delta = (resize.start_x - f32::from(event.position.x)) / self.palette.scale.max(0.1);
        let width =
            (resize.start_width + delta).clamp(MIN_PREVIEW_PANEL_WIDTH, MAX_PREVIEW_PANEL_WIDTH);
        if (width - self.layout.preview_panel_width).abs() >= f32::EPSILON {
            self.layout.preview_panel_width = width;
            if self.browser.view_mode() == ViewMode::Column {
                self.column_view.scroll_to_leaf_attempts = 3;
            }
            cx.notify();
        }
    }

    pub(crate) fn finish_preview_panel_resize(&mut self, cx: &mut Context<Self>) {
        if self.pointer.preview_panel_resize.take().is_none() {
            return;
        }
        self.settings.view.preview_panel_width = self.layout.preview_panel_width;
        self.finish_settings_change(
            format!(
                "Preview width: {} px",
                self.layout.preview_panel_width.round() as u16
            ),
            cx,
        );
    }

    pub(crate) fn adjust_preview_panel_width(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.layout.preview_panel_width = (self.layout.preview_panel_width + delta)
            .clamp(MIN_PREVIEW_PANEL_WIDTH, MAX_PREVIEW_PANEL_WIDTH);
        if self.browser.view_mode() == ViewMode::Column {
            self.column_view.scroll_to_leaf_attempts = 3;
        }
        self.settings.view.preview_panel_width = self.layout.preview_panel_width;
        self.finish_settings_change(
            format!(
                "Preview width: {} px",
                self.layout.preview_panel_width.round() as u16
            ),
            cx,
        );
    }

    pub(crate) fn handle_preview_panel_resize_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "left" => self.adjust_preview_panel_width(10.0, cx),
            "right" => self.adjust_preview_panel_width(-10.0, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(crate) fn begin_list_column_resize(
        &mut self,
        key: String,
        width: f32,
        event: &gpui::MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        self.pointer.list_column_resize = Some(ListColumnResize {
            key,
            start_x: f32::from(event.position.x),
            start_width: width,
        });
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn set_list_column_width(
        &mut self,
        key: String,
        width: f32,
        persist: bool,
        cx: &mut Context<Self>,
    ) {
        let width = width.clamp(56.0, 360.0).round() as u16;
        self.browser.set_column_width(key, width);
        if persist {
            self.persist_session();
        } else {
            cx.notify();
        }
    }

    pub(crate) fn update_list_column_resize(
        &mut self,
        event: &gpui::MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            return;
        }
        let Some(resize) = self.pointer.list_column_resize.clone() else {
            return;
        };
        let delta = f32::from(event.position.x) - resize.start_x;
        self.set_list_column_width(resize.key, resize.start_width + delta, false, cx);
    }

    pub(crate) fn finish_list_column_resize(&mut self, cx: &mut Context<Self>) {
        if self.pointer.list_column_resize.take().is_some() {
            self.persist_session();
            cx.notify();
        }
    }

    pub(crate) fn begin_column_view_resize(
        &mut self,
        path: PathBuf,
        width: f32,
        event: &gpui::MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        if event.click_count >= 2 {
            self.auto_fit_column_view_width(path, cx);
            return;
        }
        self.pointer.column_view_resize = Some(ColumnViewResize {
            path,
            start_x: f32::from(event.position.x),
            start_width: width,
        });
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn auto_fit_column_view_width(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(column) = self
            .column_view
            .columns
            .columns()
            .iter()
            .find(|column| column.path() == path)
        else {
            return;
        };
        let title_width = path_label(&path).chars().count() as f32 * 7.5 + 92.0;
        let content_width = column
            .visible_entries(&self.browser)
            .iter()
            .map(|entry| {
                let name = file_name(entry).chars().count() as f32 * 7.2;
                let detail = entry_detail(entry, self.calculate_folder_sizes, false)
                    .chars()
                    .count() as f32
                    * 6.2;
                name + detail + if entry.is_dir { 70.0 } else { 52.0 }
            })
            .fold(title_width, f32::max);
        self.set_column_view_width(path, content_width, true, cx);
    }

    pub(crate) fn set_column_view_width(
        &mut self,
        path: PathBuf,
        width: f32,
        persist: bool,
        cx: &mut Context<Self>,
    ) {
        let width = width
            .clamp(MIN_COLUMN_VIEW_WIDTH, MAX_COLUMN_VIEW_WIDTH)
            .round() as u16;
        self.browser.set_column_view_width(path, width);
        if persist {
            self.persist_session();
        } else {
            cx.notify();
        }
    }

    pub(crate) fn update_column_view_resize(
        &mut self,
        event: &gpui::MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            return;
        }
        let Some(resize) = self.pointer.column_view_resize.clone() else {
            return;
        };
        let delta = f32::from(event.position.x) - resize.start_x;
        self.set_column_view_width(resize.path, resize.start_width + delta, false, cx);
    }

    pub(crate) fn finish_column_view_resize(&mut self, cx: &mut Context<Self>) {
        if self.pointer.column_view_resize.take().is_some() {
            self.persist_session();
            cx.notify();
        }
    }

    pub(crate) fn cycle_list_row_height(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance.list_row_height = next_u16(
            self.settings.appearance.list_row_height,
            &[26, 30, 34, 40, 46, 52],
        );
        self.finish_settings_change(
            format!(
                "List row height: {} px",
                self.settings.appearance.list_row_height
            ),
            cx,
        );
    }

    pub(crate) fn cycle_grid_width(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance.grid_min_width = next_u16(
            self.settings.appearance.grid_min_width,
            &[120, 140, 160, 180, 220, 260],
        );
        self.finish_settings_change(
            format!(
                "Grid card width: {} px",
                self.settings.appearance.grid_min_width
            ),
            cx,
        );
        self.persist_session();
    }

    pub(crate) fn adjust_grid_width(&mut self, delta: i16, cx: &mut Context<Self>) {
        let width = (i32::from(self.settings.appearance.grid_min_width) + i32::from(delta))
            .clamp(120, 260) as u16;
        self.set_grid_width(width, cx);
    }

    pub(crate) fn set_grid_width(&mut self, width: u16, cx: &mut Context<Self>) {
        self.settings.appearance.grid_min_width = width.clamp(120, 260);
        self.finish_settings_change(
            format!(
                "Grid card width: {} px",
                self.settings.appearance.grid_min_width
            ),
            cx,
        );
        self.persist_session();
    }

    pub(crate) fn handle_grid_width_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "left" | "down" => self.adjust_grid_width(-10, cx),
            "right" | "up" => self.adjust_grid_width(10, cx),
            "home" => self.set_grid_width(120, cx),
            "end" => self.set_grid_width(260, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(crate) fn render_sidebar_resizer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.layout.sidebar_collapsed {
            return div().id("sidebar-resizer-slot").w_0().into_any_element();
        }
        let accessibility_view = cx.entity().downgrade();
        let decrement_view = accessibility_view.clone();
        let line_color = if self.pointer.sidebar_resize.is_some() {
            self.palette.accent
        } else {
            self.palette.border
        };

        let handle = div()
            .id("sidebar-resizer")
            .debug_selector(|| "sidebar-resizer".to_string())
            .key_context("sidebar-resizer")
            .track_focus(&self.layout.sidebar_resize_focus)
            .tab_stop(true)
            .role(Role::Splitter)
            .aria_label("Resize sidebar")
            .aria_orientation(Orientation::Vertical)
            .aria_numeric_value(f64::from(self.layout.sidebar_width))
            .aria_min_numeric_value(160.0)
            .aria_max_numeric_value(480.0)
            .on_a11y_action(AccessibleAction::Increment, move |_, _, cx| {
                accessibility_view
                    .update(cx, |this, cx| this.adjust_sidebar_width(10.0, cx))
                    .ok();
            })
            .on_a11y_action(AccessibleAction::Decrement, move |_, _, cx| {
                decrement_view
                    .update(cx, |this, cx| this.adjust_sidebar_width(-10.0, cx))
                    .ok();
            })
            .w_full()
            .h_full()
            .hover(|resizer| resizer.bg(with_alpha(self.palette.accent, 0.22)))
            .focus(|resizer| resizer.bg(with_alpha(self.palette.accent, 0.28)))
            .child(
                div()
                    .absolute()
                    .left(px(7.0))
                    .top(px(0.0))
                    .w(px(1.0))
                    .h_full()
                    .bg(line_color),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event, window, cx| this.begin_sidebar_resize(event, window, cx)),
            )
            .on_key_down(
                cx.listener(|this, event, _, cx| this.handle_sidebar_resize_key(event, cx)),
            );

        div()
            .id("sidebar-resizer-slot")
            .absolute()
            .left(px(self.layout.sidebar_width - 8.0))
            .top(px(0.0))
            .w(px(8.0))
            .h_full()
            .child(handle)
            .into_any_element()
    }

    pub(crate) fn render_preview_panel_resizer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let accessibility_view = cx.entity().downgrade();
        let decrement_view = accessibility_view.clone();
        let line_color = if self.pointer.preview_panel_resize.is_some() {
            self.palette.accent
        } else {
            self.palette.border
        };

        div()
            .id("preview-panel-resizer")
            .debug_selector(|| "preview-panel-resizer".to_string())
            .key_context("preview-panel-resizer")
            .track_focus(&self.layout.preview_panel_resize_focus)
            .tab_stop(true)
            .role(Role::Splitter)
            .aria_label("Resize preview panel")
            .aria_orientation(Orientation::Vertical)
            .aria_numeric_value(f64::from(self.layout.preview_panel_width))
            .aria_min_numeric_value(f64::from(MIN_PREVIEW_PANEL_WIDTH))
            .aria_max_numeric_value(f64::from(MAX_PREVIEW_PANEL_WIDTH))
            .on_a11y_action(AccessibleAction::Increment, move |_, _, cx| {
                accessibility_view
                    .update(cx, |this, cx| this.adjust_preview_panel_width(10.0, cx))
                    .ok();
            })
            .on_a11y_action(AccessibleAction::Decrement, move |_, _, cx| {
                decrement_view
                    .update(cx, |this, cx| this.adjust_preview_panel_width(-10.0, cx))
                    .ok();
            })
            .absolute()
            .left(px(-8.0))
            .top_0()
            .w(px(8.0))
            .h_full()
            .hover(|resizer| resizer.bg(with_alpha(self.palette.accent, 0.22)))
            .focus(|resizer| resizer.bg(with_alpha(self.palette.accent, 0.28)))
            .child(
                div()
                    .absolute()
                    .right_0()
                    .top_0()
                    .w(px(1.0))
                    .h_full()
                    .bg(line_color),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event, window, cx| {
                    this.begin_preview_panel_resize(event, window, cx)
                }),
            )
            .on_key_down(
                cx.listener(|this, event, _, cx| this.handle_preview_panel_resize_key(event, cx)),
            )
            .into_any_element()
    }

    pub(crate) fn render_grid_width_control(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette;
        let width = self.settings.appearance.grid_min_width;
        let active_step = ((width.saturating_add(5) / 10) * 10).clamp(120, 260);
        let presets = [
            ("thumbnail-size-small", "S", "Small", 120_u16),
            ("thumbnail-size-medium", "M", "Medium", 160_u16),
            ("thumbnail-size-large", "L", "Large", 200_u16),
            ("thumbnail-size-extra-large", "XL", "Extra Large", 260_u16),
        ]
        .into_iter()
        .map(|(id, short_label, label, preset)| {
            let active = width == preset;
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .role(Role::Button)
                .aria_label(format!("{label}, {preset} pixels"))
                .aria_selected(active)
                .focusable()
                .tab_stop(true)
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .h(px(28.0))
                .border_1()
                .border_color(if active {
                    palette.accent
                } else {
                    palette.border
                })
                .bg(if active {
                    palette.accent
                } else {
                    palette.window
                })
                .text_color(if active {
                    palette.window
                } else {
                    palette.muted
                })
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .hover(move |button| {
                    button.bg(if active {
                        palette.accent
                    } else {
                        palette.hover
                    })
                })
                .focus(move |button| button.border_color(palette.accent))
                .cursor_pointer()
                .child(short_label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.set_grid_width(preset, cx);
                }))
                .into_any_element()
        })
        .collect::<Vec<_>>();
        let steps = (120_u16..=260)
            .step_by(10)
            .enumerate()
            .map(|(index, step)| {
                let selected = step == active_step;
                let filled = step <= active_step;
                div()
                    .id(("thumbnail-size-step", index))
                    .debug_selector(move || format!("thumbnail-size-step-{index}"))
                    .flex()
                    .flex_1()
                    .h_full()
                    .items_center()
                    .cursor_pointer()
                    .child(
                        div()
                            .w_full()
                            .h(px(if selected { 12.0 } else { 4.0 }))
                            .rounded(px(if selected { 6.0 } else { 2.0 }))
                            .bg(if filled {
                                palette.accent
                            } else {
                                palette.border
                            }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_grid_width(step, cx);
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let accessibility_view = cx.entity().downgrade();
        let decrement_view = accessibility_view.clone();

        div()
            .id("thumbnail-size-control")
            .debug_selector(|| "thumbnail-size-control".to_string())
            .role(Role::Group)
            .aria_label("Thumbnail size")
            .flex()
            .flex_col()
            .gap_2()
            .px_2()
            .pt_2()
            .pb_3()
            .child(
                div()
                    .id("thumbnail-size-label")
                    .debug_selector(|| "thumbnail-size-label".to_string())
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(palette.muted)
                    .child("THUMBNAIL SIZE")
                    .child(format!("{width}px")),
            )
            .child(
                div()
                    .id("thumbnail-size-presets")
                    .debug_selector(|| "thumbnail-size-presets".to_string())
                    .flex()
                    .gap_1()
                    .children(presets),
            )
            .child(
                div()
                    .id("thumbnail-size-slider")
                    .debug_selector(|| "thumbnail-size-slider".to_string())
                    .focusable()
                    .tab_stop(true)
                    .role(Role::Slider)
                    .aria_label("Thumbnail size")
                    .aria_numeric_value(f64::from(width))
                    .aria_min_numeric_value(120.0)
                    .aria_max_numeric_value(260.0)
                    .on_a11y_action(AccessibleAction::Increment, move |_, _, cx| {
                        accessibility_view
                            .update(cx, |this, cx| this.adjust_grid_width(10, cx))
                            .ok();
                    })
                    .on_a11y_action(AccessibleAction::Decrement, move |_, _, cx| {
                        decrement_view
                            .update(cx, |this, cx| this.adjust_grid_width(-10, cx))
                            .ok();
                    })
                    .on_key_down(
                        cx.listener(|this, event, _, cx| this.handle_grid_width_key(event, cx)),
                    )
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(24.0))
                    .border_1()
                    .border_color(palette.border)
                    .focus(move |slider| slider.border_color(palette.accent))
                    .px_2()
                    .child(toolbar_icon("grid", 12.0, palette.muted))
                    .child(div().flex().flex_1().h_full().gap(px(2.0)).children(steps))
                    .child(toolbar_icon("grid", 18.0, palette.muted)),
            )
            .into_any_element()
    }

    pub(crate) fn render_list_column_resize_handle(
        &mut self,
        key: String,
        width: f32,
        aria_label: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self
            .pointer
            .list_column_resize
            .as_ref()
            .is_some_and(|resize| resize.key == key);
        let down_key = key.clone();
        let keyboard_key = key.clone();
        div()
            .id(ElementId::Name(format!("resize-list-column-{key}").into()))
            .role(Role::Splitter)
            .aria_label(aria_label)
            .aria_orientation(Orientation::Vertical)
            .aria_numeric_value(f64::from(width))
            .aria_min_numeric_value(56.0)
            .aria_max_numeric_value(360.0)
            .focusable()
            .tab_stop(true)
            .absolute()
            .right_0()
            .top_0()
            .w(px(8.0))
            .h_full()
            .cursor_col_resize()
            .when(active, |handle| {
                handle.bg(with_alpha(self.palette.accent, 0.3))
            })
            .hover(|handle| handle.bg(with_alpha(self.palette.accent, 0.22)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event, _, cx| {
                    this.begin_list_column_resize(down_key.clone(), width, event, cx)
                }),
            )
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                let next = match event.keystroke.key.as_str() {
                    "left" => width - 10.0,
                    "right" => width + 10.0,
                    _ => return,
                };
                this.set_list_column_width(keyboard_key.clone(), next, true, cx);
                cx.stop_propagation();
            }))
            .on_click(|_, _, cx| cx.stop_propagation())
            .into_any_element()
    }

    pub(crate) fn render_column_view_resize_handle(
        &mut self,
        column_index: usize,
        path: PathBuf,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self
            .pointer
            .column_view_resize
            .as_ref()
            .is_some_and(|resize| resize.path == path);
        let down_path = path.clone();
        let keyboard_path = path;
        div()
            .id(("resize-column-view", column_index))
            .debug_selector(move || format!("resize-column-view-{column_index}"))
            .role(Role::Splitter)
            .aria_label("Resize folder column")
            .aria_orientation(Orientation::Vertical)
            .aria_numeric_value(f64::from(width))
            .aria_min_numeric_value(f64::from(MIN_COLUMN_VIEW_WIDTH))
            .aria_max_numeric_value(f64::from(MAX_COLUMN_VIEW_WIDTH))
            .focusable()
            .tab_stop(true)
            .absolute()
            .right_0()
            .top_0()
            .w(px(8.0))
            .h_full()
            .cursor_col_resize()
            .when(active, |handle| {
                handle.bg(with_alpha(self.palette.accent, 0.3))
            })
            .hover(|handle| handle.bg(with_alpha(self.palette.accent, 0.22)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event, _, cx| {
                    this.begin_column_view_resize(down_path.clone(), width, event, cx)
                }),
            )
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                let next = match event.keystroke.key.as_str() {
                    "left" => width - 10.0,
                    "right" => width + 10.0,
                    _ => return,
                };
                this.set_column_view_width(keyboard_path.clone(), next, true, cx);
                cx.stop_propagation();
            }))
            .on_click(|_, _, cx| cx.stop_propagation())
            .into_any_element()
    }
}
