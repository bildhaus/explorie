//! Reusable toolbar, menu, and settings controls.

use crate::*;

pub(crate) fn sidebar_section_label(label: &'static str, palette: UiPalette) -> impl IntoElement {
    div()
        .mt_2()
        .px_3()
        .py_1()
        .text_xs()
        .text_color(palette.tertiary)
        .child(label)
}

pub(crate) fn conflict_path_card(
    label: &'static str,
    path: &Path,
    palette: UiPalette,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap_2()
        .p_3()
        .rounded(px(palette.radius))
        .bg(palette.surface)
        .child(div().text_xs().text_color(palette.muted).child(label))
        .child(
            div()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .child(path_label(path)),
        )
        .child(
            div()
                .truncate()
                .text_xs()
                .text_color(palette.muted)
                .child(path.display().to_string()),
        )
        .into_any_element()
}

pub(crate) fn operation_context_row(
    id: &'static str,
    title: &str,
    details: String,
    accent: Rgba,
    palette: UiPalette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .flex()
        .items_center()
        .gap_2()
        .p_3()
        .rounded_sm()
        .bg(palette.surface)
        .child(div().w(px(10.0)).h(px(10.0)).rounded_full().bg(accent))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(palette.tertiary)
                        .child(details),
                ),
        )
}

pub(crate) fn operation_icon_button(
    id: impl Into<ElementId>,
    label: &str,
    icon: &'static str,
    palette: UiPalette,
) -> impl gpui::StatefulInteractiveElement + IntoElement {
    let size = 28.0 * palette.scale;
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.to_string())
        .focusable()
        .tab_stop(true)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .w(px(size))
        .h(px(size))
        .rounded(px(palette.radius))
        .border_1()
        .border_color(with_alpha(palette.border, 0.0))
        .focus_visible(move |button| button.bg(palette.hover).border_color(palette.accent))
        .hover(move |button| button.bg(palette.hover))
        .active(|button| button.opacity(0.82))
        .cursor_pointer()
        .tooltip({
            let label = label.to_string();
            move |_, cx| app_tooltip(label.clone(), palette, cx)
        })
        .child(toolbar_icon(
            icon,
            palette.icon_size.clamp(13.0, 18.0),
            palette.muted,
        ))
}

pub(crate) fn toolbar_button(
    id: impl Into<ElementId>,
    label: &str,
    color: gpui::Rgba,
) -> gpui::Stateful<gpui::Div> {
    toolbar_button_enabled(id, label, color, true)
}

pub(crate) fn toolbar_button_enabled(
    id: impl Into<ElementId>,
    label: &str,
    color: gpui::Rgba,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.to_string())
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .px_3()
        .py_1()
        .rounded_sm()
        .border_1()
        .border_color(with_alpha(color, 0.0))
        .bg(color)
        .when(enabled, |button| {
            button
                .focusable()
                .tab_stop(true)
                .hover(move |button| button.bg(adaptive_hover(color)))
                .focus_visible(move |button| {
                    button
                        .bg(adaptive_hover(color))
                        .border_color(adaptive_hover(adaptive_hover(color)))
                })
                .active(|button| button.opacity(0.82))
                .cursor_pointer()
        })
        .when(!enabled, |button| button.opacity(0.58))
        .whitespace_nowrap()
        .text_sm()
        .child(label.to_string())
}

pub(crate) fn toolbar_icon(name: &'static str, size: f32, color: Rgba) -> impl IntoElement {
    svg()
        .path(format!("icons/{name}.svg"))
        .w(px(size))
        .h(px(size))
        .text_color(color)
}

pub(crate) fn compact_toolbar_button(
    id: &'static str,
    label: &str,
    icon: &'static str,
    palette: UiPalette,
    active: bool,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    compact_toolbar_button_with_tooltip(
        id,
        label,
        icon,
        palette,
        active,
        enabled,
        control_tooltip_text(label),
    )
}

pub(crate) fn compact_toolbar_button_with_tooltip(
    id: &'static str,
    label: &str,
    icon: &'static str,
    palette: UiPalette,
    active: bool,
    enabled: bool,
    tooltip: String,
) -> gpui::Stateful<gpui::Div> {
    let density_scale = if palette.density == Density::Compact {
        0.94
    } else {
        1.0
    };
    let size = 32.0 * palette.scale * density_scale;
    let foreground = if !enabled {
        palette.tertiary
    } else if active {
        palette.accent
    } else {
        palette.muted
    };
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .role(Role::Button)
        .aria_label(label.to_string())
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .w(px(size))
        .h(px(size))
        .rounded(px(palette.radius))
        .border_1()
        .border_color(if active {
            palette.accent
        } else {
            with_alpha(palette.border, 0.0)
        })
        .bg(if active {
            palette.selected
        } else {
            with_alpha(palette.window, 0.0)
        })
        .when(enabled, |button| {
            button
                .focusable()
                .tab_stop(true)
                .focus_visible(move |button| button.bg(palette.hover).border_color(palette.accent))
                .hover(move |button| button.bg(palette.hover).border_color(palette.border))
                .active(|button| button.opacity(0.82))
                .cursor_pointer()
                .tooltip(move |_, cx| app_tooltip(tooltip.clone(), palette, cx))
        })
        .child(toolbar_icon(
            icon,
            palette.icon_size.clamp(14.0, 20.0),
            foreground,
        ))
}

pub(crate) fn toolbar_menu_item(
    id: &'static str,
    label: impl Into<String>,
    icon: &'static str,
    palette: UiPalette,
    active: bool,
) -> gpui::Stateful<gpui::Div> {
    let label = label.into();
    let foreground = if active { palette.accent } else { palette.text };
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .role(Role::Button)
        .aria_label(label.clone())
        .aria_selected(active)
        .focusable()
        .tab_stop(true)
        .flex()
        .items_center()
        .gap_2()
        .min_h(px(if palette.density == Density::Compact {
            30.0
        } else {
            34.0
        } * palette.scale))
        .px_2()
        .rounded(px(palette.radius))
        .border_1()
        .border_color(with_alpha(palette.border, 0.0))
        .bg(if active {
            palette.selected
        } else {
            with_alpha(palette.surface, 0.0)
        })
        .hover(move |row| row.bg(palette.hover))
        .focus_visible(move |row| row.bg(palette.hover).border_color(palette.accent))
        .active(|row| row.opacity(0.82))
        .cursor_pointer()
        .text_sm()
        .text_color(foreground)
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(20.0))
                .child(toolbar_icon(
                    icon,
                    palette.icon_size.clamp(13.0, 18.0),
                    foreground,
                )),
        )
        .child(div().flex_1().min_w_0().child(label))
}

pub(crate) fn history_menu_item(
    id: (&'static str, usize),
    label: String,
    direction: &'static str,
    palette: UiPalette,
) -> gpui::Stateful<gpui::Div> {
    let selector = format!("{}-{}", id.0, id.1);
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .role(Role::Button)
        .aria_label(format!("Go {direction} to {label}"))
        .focusable()
        .tab_stop(true)
        .flex()
        .items_center()
        .gap_2()
        .min_h(px(if palette.density == Density::Compact {
            30.0
        } else {
            34.0
        } * palette.scale))
        .px_2()
        .rounded(px(palette.radius))
        .bg(with_alpha(palette.surface, 0.0))
        .focus(move |row| row.bg(palette.hover))
        .hover(move |row| row.bg(palette.hover))
        .cursor_pointer()
        .text_sm()
        .text_color(palette.text)
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(20.0))
                .child(toolbar_icon(
                    "folder",
                    palette.icon_size.clamp(13.0, 18.0),
                    palette.muted,
                )),
        )
        .child(div().flex_1().min_w_0().truncate().child(label))
}

pub(crate) fn toolbar_popover(
    id: &'static str,
    palette: UiPalette,
    children: Vec<AnyElement>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .flex()
        .flex_col()
        .w(px(220.0 * palette.scale))
        .max_h(px(440.0))
        .overflow_y_scroll()
        .p_1()
        .rounded(px((palette.radius + 2.0).min(12.0)))
        .border_1()
        .border_color(palette.border)
        .bg(palette.surface)
        .text_color(palette.text)
        .shadow_lg()
        .occlude()
        .children(children)
}

pub(crate) fn toolbar_menu_separator(palette: UiPalette) -> AnyElement {
    div()
        .h(px(1.0))
        .my_1()
        .bg(palette.border)
        .into_any_element()
}

pub(crate) fn settings_control_with_palette(
    palette: UiPalette,
    id: &'static str,
    label: String,
    active: bool,
) -> impl gpui::StatefulInteractiveElement + IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .role(Role::Button)
        .aria_label(label.clone())
        .aria_selected(active)
        .focusable()
        .tab_stop(true)
        .flex()
        .items_center()
        .w_full()
        .min_h(px(if palette.density == Density::Compact {
            36.0
        } else {
            40.0
        } * palette.scale))
        .px_3()
        .py_2()
        .rounded(px(palette.radius))
        .border_1()
        .border_color(with_alpha(palette.border, 0.0))
        .bg(if active {
            palette.selected
        } else {
            palette.window
        })
        .hover(move |button| button.bg(palette.hover))
        .focus_visible(move |button| button.bg(palette.hover).border_color(palette.accent))
        .active(|button| button.opacity(0.82))
        .cursor_pointer()
        .text_sm()
        .text_color(if active { palette.text } else { palette.muted })
        .child(label)
}

pub(crate) fn settings_field_row(
    label: &'static str,
    description: &'static str,
    palette: UiPalette,
) -> gpui::Div {
    let compact = palette.density == Density::Compact;
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_4()
        .min_h(px(if compact { 48.0 } else { 56.0 } * palette.scale))
        .px_3()
        .when(compact, |row| row.py_1())
        .when(!compact, |row| row.py_2())
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .flex_1()
                .min_w(px(220.0 * palette.scale))
                .child(div().text_sm().text_color(palette.text).child(label))
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.tertiary)
                        .child(description),
                ),
        )
}

pub(crate) fn settings_toggle_control(
    palette: UiPalette,
    id: &'static str,
    label: &'static str,
    description: &'static str,
    enabled: bool,
    available: bool,
) -> gpui::Stateful<gpui::Div> {
    let foreground = if available {
        palette.text
    } else {
        palette.tertiary
    };
    let track = if enabled {
        palette.accent
    } else {
        palette.control
    };
    let compact = palette.density == Density::Compact;
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .role(Role::Switch)
        .aria_label(label)
        .aria_selected(enabled)
        .when(available, |control| control.focusable().tab_stop(true))
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .min_h(px(if compact { 48.0 } else { 56.0 } * palette.scale))
        .px_3()
        .when(compact, |row| row.py_1())
        .when(!compact, |row| row.py_2())
        .rounded(px(palette.radius))
        .border_1()
        .border_color(with_alpha(palette.border, 0.0))
        .when(available, |control| {
            control
                .hover(move |row| row.bg(palette.hover))
                .focus_visible(move |row| row.bg(palette.hover).border_color(palette.accent))
                .active(|row| row.opacity(0.82))
                .cursor_pointer()
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().text_color(foreground).child(label))
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.tertiary)
                        .child(description),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .when(enabled, |track| track.justify_end())
                .when(!enabled, |track| track.justify_start())
                .w(px(36.0 * palette.scale))
                .h(px(20.0 * palette.scale))
                .p(px(2.0 * palette.scale))
                .rounded_full()
                .border_1()
                .border_color(if enabled {
                    palette.accent
                } else {
                    palette.border
                })
                .bg(track)
                .child(
                    div()
                        .w(px(14.0 * palette.scale))
                        .h(px(14.0 * palette.scale))
                        .rounded_full()
                        .bg(if enabled {
                            palette.window
                        } else {
                            palette.muted
                        }),
                ),
        )
}

pub(crate) fn settings_segment_button(
    palette: UiPalette,
    id: impl Into<ElementId>,
    label: impl Into<String>,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    let label = label.into();
    div()
        .id(id)
        .role(Role::RadioButton)
        .aria_label(label.clone())
        .aria_selected(selected)
        .focusable()
        .tab_stop(true)
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(54.0 * palette.scale))
        .h(px(if palette.density == Density::Compact {
            28.0
        } else {
            32.0
        } * palette.scale))
        .px_2()
        .rounded(px(palette.radius))
        .border_1()
        .border_color(if selected {
            palette.accent
        } else {
            palette.border
        })
        .bg(if selected {
            palette.selected
        } else {
            palette.window
        })
        .hover(move |button| button.bg(palette.hover))
        .focus_visible(move |button| button.border_color(palette.accent))
        .active(|button| button.opacity(0.82))
        .cursor_pointer()
        .text_xs()
        .text_color(if selected {
            palette.text
        } else {
            palette.muted
        })
        .child(label)
}

pub(crate) fn settings_section(
    label: &'static str,
    controls: Vec<AnyElement>,
    palette: UiPalette,
) -> AnyElement {
    let compact = palette.density == Density::Compact;
    div()
        .flex()
        .flex_col()
        .gap_2()
        .when(compact, |section| section.p_2())
        .when(!compact, |section| section.p_3())
        .child(
            div()
                .flex()
                .items_center()
                .min_h(px(32.0 * palette.scale))
                .flex_none()
                .px_2()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(palette.text)
                .child(label),
        )
        .child(div().flex().flex_col().gap_1().children(controls))
        .into_any_element()
}
