//! `DirectoryWindow` behavior for settings panel.

use crate::*;

/// The settings panel: whether it is open and on which tab, its selectors and
/// value editors, and the confirmation dialog with its focus handling.
pub(crate) struct SettingsUi {
    pub(crate) panel_open: bool,
    pub(crate) tab: SettingsTab,
    pub(crate) selector: Option<SettingsSelector>,
    pub(crate) confirmation: Option<SettingsConfirmation>,
    pub(crate) confirmation_cancel_focus: FocusHandle,
    pub(crate) confirmation_confirm_focus: FocusHandle,
    pub(crate) confirmation_return_focus: Option<FocusHandle>,
    pub(crate) confirmation_focus_pending: bool,
    pub(crate) appearance_value_editor: Option<AppearanceValueEditor>,
    pub(crate) named_theme_editor: Option<NamedThemeEditor>,
    pub(crate) shortcut_editor: Option<ShortcutEditor>,
}

impl SettingsUi {
    pub(crate) fn new(cx: &App) -> Self {
        Self {
            panel_open: false,
            tab: SettingsTab::default(),
            selector: None,
            confirmation: None,
            confirmation_cancel_focus: cx.focus_handle(),
            confirmation_confirm_focus: cx.focus_handle(),
            confirmation_return_focus: None,
            confirmation_focus_pending: false,
            appearance_value_editor: None,
            named_theme_editor: None,
            shortcut_editor: None,
        }
    }
}

impl DirectoryWindow {
    pub(crate) fn persist_settings(&mut self) {
        let persisted_settings = if let Some(runtime) = self
            .window_lifetime
            .as_ref()
            .map(|lifetime| &lifetime.runtime)
        {
            runtime.publish_settings(&mut self.shared_state_revision, self.settings.clone())
        } else {
            self.settings.clone()
        };
        let error = self
            .settings_store
            .as_ref()
            .and_then(|store| store.save(&persisted_settings).err());
        if let Some(error) = error {
            self.record_error("Settings save failed", &error);
            self.status_message = Some(format!("Unable to save settings: {error}"));
        }
    }

    pub(crate) fn finish_settings_change(
        &mut self,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.persist_settings();
        self.status_message = Some(message.into());
        cx.notify();
    }

    pub(crate) fn apply_global_browser_preferences(&mut self) {
        self.browser.apply_browser_preferences(
            self.settings.view.show_hidden,
            self.settings.view.show_system_files,
            self.settings.view.filter_mode,
            self.settings.view.sort_key.clone(),
            self.settings.view.sort_direction,
            self.settings.view.view_mode,
        );
    }

    pub(crate) fn set_theme(&mut self, theme: ThemeMode, cx: &mut Context<Self>) {
        self.settings.appearance.theme = theme;
        self.finish_settings_change(format!("Theme: {}", theme.label()), cx);
    }

    pub(crate) fn toggle_settings_panel(&mut self, cx: &mut Context<Self>) {
        if self.settings_ui.panel_open {
            self.close_settings_panel(cx);
            return;
        }
        if self.has_control_draft() {
            self.request_settings_confirmation(
                SettingsConfirmation::DiscardDraft(PendingDraftAction::OpenSettings),
                cx,
            );
            return;
        }
        self.begin_overlay_focus();
        self.settings_ui.panel_open = true;
        self.overlay.toolbar_menu = ToolbarMenu::Closed;
        self.search.active = false;
        cx.notify();
    }

    pub(crate) fn close_settings_panel(&mut self, cx: &mut Context<Self>) {
        if self.has_settings_draft() {
            self.request_settings_confirmation(
                SettingsConfirmation::DiscardDraft(PendingDraftAction::CloseSettings),
                cx,
            );
            return;
        }
        self.close_settings_panel_unchecked(cx);
    }

    pub(crate) fn close_settings_panel_unchecked(&mut self, cx: &mut Context<Self>) {
        if self.settings_ui.panel_open {
            self.settings_ui.panel_open = false;
            self.settings_ui.appearance_value_editor = None;
            self.settings_ui.named_theme_editor = None;
            self.settings_ui.shortcut_editor = None;
            self.settings_ui.selector = None;
            self.settings_ui.confirmation = None;
            self.deactivate_native_text_input();
            self.finish_overlay_focus_if_inactive();
            cx.notify();
        }
    }

    pub(crate) fn reset_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_ui.appearance_value_editor = None;
        self.settings_ui.named_theme_editor = None;
        self.settings_ui.shortcut_editor = None;
        self.settings.reset_preserving_legacy();
        self.install_shortcut_bindings(cx);
        self.layout.sidebar_width = self.settings.view.sidebar_width;
        self.layout.preview_panel_width = self.settings.view.preview_panel_width;
        self.layout.sidebar_collapsed = false;
        self.pointer.sidebar_resize = None;
        self.pointer.preview_panel_resize = None;
        self.apply_global_browser_preferences();
        self.calculate_folder_sizes = self.settings.view.show_folder_sizes;
        self.undo_ledger.set_timeout(Duration::from_secs(
            u64::from(self.settings.behavior.undo_timeout_minutes) * 60,
        ));
        self.close_preview(cx);
        self.refresh(cx);
        self.finish_settings_change("Settings restored to defaults", cx);
    }

    pub(crate) fn request_settings_confirmation(
        &mut self,
        confirmation: SettingsConfirmation,
        cx: &mut Context<Self>,
    ) {
        self.settings_ui.confirmation = Some(confirmation);
        self.settings_ui.selector = None;
        self.settings_ui.confirmation_focus_pending = true;
        cx.notify();
    }

    pub(crate) fn restore_settings_confirmation_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = self
            .settings_ui
            .confirmation_return_focus
            .take()
            .unwrap_or_else(|| self.focus_handle.clone());
        window.focus(&focus, cx);
        self.settings_ui.confirmation_focus_pending = false;
    }

    pub(crate) fn cancel_settings_confirmation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_ui.confirmation.take().is_some() {
            self.restore_settings_confirmation_focus(window, cx);
            cx.notify();
        }
    }

    pub(crate) fn confirm_settings_confirmation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(confirmation) = self.settings_ui.confirmation.take() else {
            return;
        };
        if matches!(&confirmation, SettingsConfirmation::DiscardDraft(_)) {
            self.settings_ui.confirmation_return_focus = Some(self.focus_handle.clone());
        }
        match confirmation {
            SettingsConfirmation::ResetSettings => self.reset_settings(cx),
            SettingsConfirmation::ResetShortcut(command_id) => self.reset_shortcut(&command_id, cx),
            SettingsConfirmation::ResetAllShortcuts => self.reset_all_shortcuts(cx),
            SettingsConfirmation::ClearPreviewCache => self.clear_preview_cache(cx),
            SettingsConfirmation::DeleteTheme(name) => self.delete_named_theme(&name, cx),
            SettingsConfirmation::RemoveCustomField(name) => self.remove_custom_field(&name, cx),
            SettingsConfirmation::InstallUpdate(update) => self.start_update_download(update, cx),
            SettingsConfirmation::CleanupInstallMedia(offer) => {
                self.start_install_media_cleanup(offer, cx)
            }
            SettingsConfirmation::DiscardDraft(action) => {
                self.discard_draft_and_continue(action, cx)
            }
        }
        self.restore_settings_confirmation_focus(window, cx);
    }

    pub(crate) fn has_settings_draft(&self) -> bool {
        self.settings_ui
            .appearance_value_editor
            .as_ref()
            .is_some_and(|editor| {
                let saved = match editor.kind {
                    AppearanceValueKind::Accent => &self.settings.appearance.accent_custom,
                    AppearanceValueKind::Font => &self.settings.appearance.font_custom,
                };
                editor.input != *saved
            })
            || self
                .settings_ui
                .named_theme_editor
                .as_ref()
                .is_some_and(|editor| !editor.input.trim().is_empty())
            || self
                .settings_ui
                .shortcut_editor
                .as_ref()
                .is_some_and(|editor| {
                    binding_for(&self.settings.shortcut_bindings, &editor.command_id)
                        .is_some_and(|saved| saved != editor.binding)
                })
    }

    pub(crate) fn discard_draft_and_continue(
        &mut self,
        action: PendingDraftAction,
        cx: &mut Context<Self>,
    ) {
        match action {
            PendingDraftAction::CloseControlSurface => {
                self.close_control_surface_unchecked(cx);
            }
            PendingDraftAction::OpenControlSurface(surface) => {
                self.open_control_surface_unchecked(surface, cx);
            }
            PendingDraftAction::ToggleToolbarMenu(menu) => {
                self.close_control_surface_unchecked(cx);
                self.toggle_toolbar_menu_unchecked(menu, cx);
            }
            PendingDraftAction::OpenSettings => {
                self.close_control_surface_unchecked(cx);
                self.begin_overlay_focus();
                self.settings_ui.panel_open = true;
                self.overlay.toolbar_menu = ToolbarMenu::Closed;
                self.search.active = false;
                cx.notify();
            }
            PendingDraftAction::CloseSettings => self.close_settings_panel_unchecked(cx),
        }
    }

    pub(crate) fn cycle_theme(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance.theme = self.settings.appearance.theme.next();
        self.finish_settings_change(
            format!("Theme: {}", self.settings.appearance.theme.label()),
            cx,
        );
    }

    pub(crate) fn set_density(&mut self, density: Density, cx: &mut Context<Self>) {
        if self.settings.appearance.density != density {
            self.settings.appearance.density = density;
            self.finish_settings_change(format!("Density: {}", density.label()), cx);
        }
    }

    pub(crate) fn set_border_radius(&mut self, radius: u8, cx: &mut Context<Self>) {
        let radius = match radius {
            0 | 4 | 8 => radius,
            _ => return,
        };
        if self.settings.appearance.border_radius != radius {
            self.settings.appearance.border_radius = radius;
            self.finish_settings_change(format!("Border radius: {radius} px"), cx);
        }
    }

    pub(crate) fn set_undo_timeout(&mut self, minutes: u32, cx: &mut Context<Self>) {
        if !matches!(minutes, 1 | 5 | 15 | 30 | 60) {
            return;
        }
        self.settings.behavior.undo_timeout_minutes = minutes;
        self.undo_ledger
            .set_timeout(Duration::from_secs(u64::from(minutes) * 60));
        self.finish_settings_change(format!("Undo timeout: {minutes} minute(s)"), cx);
    }

    pub(crate) fn set_accent_choice(&mut self, accent: AccentColor, cx: &mut Context<Self>) {
        self.settings_ui.selector = None;
        if accent == AccentColor::Custom {
            self.open_appearance_value_editor(AppearanceValueKind::Accent, cx);
        } else {
            self.settings.appearance.accent = accent;
            self.finish_settings_change(format!("Accent: {}", accent.label()), cx);
        }
    }

    pub(crate) fn set_font_choice(&mut self, font: FontChoice, cx: &mut Context<Self>) {
        self.settings_ui.selector = None;
        if font == FontChoice::Custom {
            self.open_appearance_value_editor(AppearanceValueKind::Font, cx);
        } else {
            self.settings.appearance.font = font;
            self.finish_settings_change(format!("Font: {}", font.label()), cx);
        }
    }

    pub(crate) fn settings_slider_values(kind: SettingsSlider) -> &'static [f32] {
        match kind {
            SettingsSlider::UiScale => UI_SCALE_STEPS,
            SettingsSlider::ListRowHeight => &[26.0, 30.0, 34.0, 40.0, 46.0, 52.0],
            SettingsSlider::GridWidth => &[
                120.0, 130.0, 140.0, 150.0, 160.0, 170.0, 180.0, 190.0, 200.0, 210.0, 220.0, 230.0,
                240.0, 250.0, 260.0,
            ],
            SettingsSlider::IconSize => &[10.0, 12.0, 14.0, 16.0, 18.0, 20.0, 22.0, 24.0],
        }
    }

    pub(crate) fn settings_slider_value(&self, kind: SettingsSlider) -> f32 {
        match kind {
            SettingsSlider::UiScale => self.settings.appearance.ui_scale,
            SettingsSlider::ListRowHeight => f32::from(self.settings.appearance.list_row_height),
            SettingsSlider::GridWidth => f32::from(self.settings.appearance.grid_min_width),
            SettingsSlider::IconSize => f32::from(self.settings.appearance.icon_size),
        }
    }

    pub(crate) fn set_settings_slider_index(
        &mut self,
        kind: SettingsSlider,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(value) = Self::settings_slider_values(kind).get(index).copied() else {
            return;
        };
        match kind {
            SettingsSlider::UiScale => {
                self.settings.appearance.ui_scale = value;
                self.finish_settings_change(format!("UI scale: {value:.2}×"), cx);
            }
            SettingsSlider::ListRowHeight => {
                self.settings.appearance.list_row_height = value as u16;
                self.finish_settings_change(format!("List row height: {value:.0} px"), cx);
            }
            SettingsSlider::GridWidth => self.set_grid_width(value as u16, cx),
            SettingsSlider::IconSize => {
                self.settings.appearance.icon_size = value as u8;
                self.finish_settings_change(format!("Icon size: {value:.0} px"), cx);
            }
        }
    }

    pub(crate) fn adjust_settings_slider(
        &mut self,
        kind: SettingsSlider,
        delta: isize,
        cx: &mut Context<Self>,
    ) {
        let values = Self::settings_slider_values(kind);
        let current = self.settings_slider_value(kind);
        let index = values
            .iter()
            .position(|value| (*value - current).abs() < f32::EPSILON)
            .unwrap_or(0);
        let next = (index as isize + delta).clamp(0, values.len() as isize - 1) as usize;
        self.set_settings_slider_index(kind, next, cx);
    }

    pub(crate) fn handle_settings_slider_key(
        &mut self,
        kind: SettingsSlider,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "left" | "down" => self.adjust_settings_slider(kind, -1, cx),
            "right" | "up" => self.adjust_settings_slider(kind, 1, cx),
            "home" => self.set_settings_slider_index(kind, 0, cx),
            "end" => self.set_settings_slider_index(
                kind,
                Self::settings_slider_values(kind).len() - 1,
                cx,
            ),
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(crate) fn render_settings_slider(
        &mut self,
        kind: SettingsSlider,
        id: &'static str,
        label: &'static str,
        description: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = self.palette;
        let values = Self::settings_slider_values(kind);
        let current = self.settings_slider_value(kind);
        let current_index = values
            .iter()
            .position(|value| (*value - current).abs() < f32::EPSILON)
            .unwrap_or(0);
        let fraction = stepped_slider_fraction(current_index, values.len());
        let display = match kind {
            SettingsSlider::UiScale => format!("{current:.2}×"),
            _ => format!("{current:.0} px"),
        };
        let steps = values
            .iter()
            .enumerate()
            .map(|(index, _)| {
                div()
                    .id((id, index))
                    .flex_1()
                    .h_full()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_settings_slider_index(kind, index, cx)
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let increment_view = cx.entity().downgrade();
        let decrement_view = increment_view.clone();
        settings_field_row(label, description, self.palette)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .w_full()
                    .max_w(px(280.0 * palette.scale))
                    .child(
                        div()
                            .id(id)
                            .debug_selector(move || id.to_string())
                            .role(Role::Slider)
                            .aria_label(label)
                            .aria_numeric_value(f64::from(current))
                            .aria_min_numeric_value(f64::from(values[0]))
                            .aria_max_numeric_value(f64::from(values[values.len() - 1]))
                            .focusable()
                            .tab_stop(true)
                            .on_a11y_action(AccessibleAction::Increment, move |_, _, cx| {
                                increment_view
                                    .update(cx, |this, cx| this.adjust_settings_slider(kind, 1, cx))
                                    .ok();
                            })
                            .on_a11y_action(AccessibleAction::Decrement, move |_, _, cx| {
                                decrement_view
                                    .update(cx, |this, cx| {
                                        this.adjust_settings_slider(kind, -1, cx)
                                    })
                                    .ok();
                            })
                            .on_key_down(cx.listener(move |this, event, _, cx| {
                                this.handle_settings_slider_key(kind, event, cx)
                            }))
                            .relative()
                            .flex()
                            .items_center()
                            .gap(px(2.0))
                            .h(px(28.0 * palette.scale))
                            .flex_1()
                            .px_2()
                            .rounded(px(palette.radius))
                            .border_1()
                            .border_color(palette.border)
                            .focus(move |slider| slider.border_color(palette.accent))
                            .child(
                                div()
                                    .absolute()
                                    .debug_selector(move || format!("{id}-track"))
                                    .left(px(8.0 * palette.scale))
                                    .right(px(8.0 * palette.scale))
                                    .top(px(9.0 * palette.scale))
                                    .child(media_slider_track(fraction, palette)),
                            )
                            .child(div().absolute().inset_0().flex().px_2().children(steps)),
                    )
                    .child(
                        div()
                            .w(px(56.0 * palette.scale))
                            .text_right()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(display),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn cycle_accent(&mut self, cx: &mut Context<Self>) {
        if self.settings.appearance.accent == AccentColor::Custom {
            self.open_appearance_value_editor(AppearanceValueKind::Accent, cx);
            return;
        }
        let next = self.settings.appearance.accent.next();
        if next == AccentColor::Custom {
            self.open_appearance_value_editor(AppearanceValueKind::Accent, cx);
            return;
        }
        self.settings.appearance.accent = next;
        self.finish_settings_change(
            format!("Accent: {}", self.settings.appearance.accent.label()),
            cx,
        );
    }

    pub(crate) fn cycle_density(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance.density = self.settings.appearance.density.next();
        self.finish_settings_change(
            format!("Density: {}", self.settings.appearance.density.label()),
            cx,
        );
    }

    pub(crate) fn cycle_ui_scale(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance.ui_scale =
            next_f32(self.settings.appearance.ui_scale, UI_SCALE_STEPS);
        self.finish_settings_change(
            format!("UI scale: {:.2}×", self.settings.appearance.ui_scale),
            cx,
        );
    }

    pub(crate) fn adjust_ui_scale(&mut self, direction: isize, cx: &mut Context<Self>) {
        let value = stepped_f32(self.settings.appearance.ui_scale, UI_SCALE_STEPS, direction);
        self.set_ui_scale(value, cx);
    }

    pub(crate) fn reset_ui_scale(&mut self, cx: &mut Context<Self>) {
        self.set_ui_scale(AppearanceSettings::default().ui_scale, cx);
    }

    pub(crate) fn set_ui_scale(&mut self, value: f32, cx: &mut Context<Self>) {
        if (self.settings.appearance.ui_scale - value).abs() < f32::EPSILON {
            return;
        }
        self.settings.appearance.ui_scale = value;
        self.finish_settings_change(format!("UI scale: {value:.2}×"), cx);
    }

    pub(crate) fn cycle_font(&mut self, cx: &mut Context<Self>) {
        if self.settings.appearance.font == FontChoice::Custom {
            self.open_appearance_value_editor(AppearanceValueKind::Font, cx);
            return;
        }
        let next = self.settings.appearance.font.next();
        if next == FontChoice::Custom {
            self.open_appearance_value_editor(AppearanceValueKind::Font, cx);
            return;
        }
        self.settings.appearance.font = next;
        self.finish_settings_change(
            format!("Font: {}", self.settings.appearance.font.label()),
            cx,
        );
    }

    pub(crate) fn open_appearance_value_editor(
        &mut self,
        kind: AppearanceValueKind,
        cx: &mut Context<Self>,
    ) {
        let input = match kind {
            AppearanceValueKind::Accent => self.settings.appearance.accent_custom.clone(),
            AppearanceValueKind::Font => self.settings.appearance.font_custom.clone(),
        };
        self.begin_overlay_focus();
        self.settings_ui.panel_open = true;
        self.search.active = false;
        self.settings_ui.named_theme_editor = None;
        self.settings_ui.appearance_value_editor = Some(AppearanceValueEditor {
            kind,
            input,
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn cancel_appearance_value_editor(&mut self, cx: &mut Context<Self>) {
        if self.settings_ui.appearance_value_editor.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn use_appearance_preset(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.settings_ui.appearance_value_editor.take() else {
            return;
        };
        match editor.kind {
            AppearanceValueKind::Accent => self.settings.appearance.accent = AccentColor::Blue,
            AppearanceValueKind::Font => self.settings.appearance.font = FontChoice::Mono,
        }
        self.finish_settings_change("Custom appearance value disabled", cx);
    }

    pub(crate) fn submit_appearance_value_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.settings_ui.appearance_value_editor.as_ref() else {
            return;
        };
        let value = editor.input.trim().to_string();
        let validation_error = match editor.kind {
            AppearanceValueKind::Accent if parse_hex_color(&value).is_none() => {
                Some("Enter a six-digit color such as #7CC7FF")
            }
            AppearanceValueKind::Font if value.is_empty() => Some("Enter a font family name"),
            AppearanceValueKind::Font
                if value.len() > 80 || value.chars().any(char::is_control) =>
            {
                Some("Font family names must be 1–80 printable characters")
            }
            _ => None,
        };
        if let Some(error) = validation_error {
            self.settings_ui
                .appearance_value_editor
                .as_mut()
                .unwrap()
                .error = Some(error.to_string());
            cx.notify();
            return;
        }

        let editor = self.settings_ui.appearance_value_editor.take().unwrap();
        match editor.kind {
            AppearanceValueKind::Accent => {
                self.settings.appearance.accent = AccentColor::Custom;
                self.settings.appearance.accent_custom = value.to_ascii_uppercase();
                self.finish_settings_change(
                    format!("Custom accent: {}", self.settings.appearance.accent_custom),
                    cx,
                );
            }
            AppearanceValueKind::Font => {
                self.settings.appearance.font = FontChoice::Custom;
                self.settings.appearance.font_custom = value;
                self.finish_settings_change(
                    format!("Custom font: {}", self.settings.appearance.font_custom),
                    cx,
                );
            }
        }
    }

    pub(crate) fn handle_appearance_value_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => self.cancel_appearance_value_editor(cx),
            "enter" => self.submit_appearance_value_editor(cx),
            "backspace" => {
                if let Some(editor) = self.settings_ui.appearance_value_editor.as_mut() {
                    editor.input.pop();
                    editor.error = None;
                    cx.notify();
                }
            }
            _ if !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(text) = event.keystroke.key_char.as_deref()
                    && !text.chars().any(char::is_control)
                    && let Some(editor) = self.settings_ui.appearance_value_editor.as_mut()
                    && editor.input.len() + text.len() <= 80
                {
                    editor.input.push_str(text);
                    editor.error = None;
                    cx.notify();
                }
            }
            _ => {}
        }
        cx.stop_propagation();
    }

    pub(crate) fn open_named_theme_editor(&mut self, cx: &mut Context<Self>) {
        self.begin_overlay_focus();
        self.settings_ui.panel_open = true;
        self.search.active = false;
        self.settings_ui.appearance_value_editor = None;
        self.settings_ui.shortcut_editor = None;
        self.settings_ui.named_theme_editor = Some(NamedThemeEditor::default());
        cx.notify();
    }

    pub(crate) fn open_shortcut_editor(&mut self, command_id: &str, cx: &mut Context<Self>) {
        let Some(binding) = binding_for(&self.settings.shortcut_bindings, command_id) else {
            self.show_toast("Shortcut command no longer exists", ToastKind::Warning, cx);
            return;
        };
        install_shortcut_recorder(cx);
        self.begin_overlay_focus();
        self.settings_ui.panel_open = true;
        self.search.active = false;
        self.settings_ui.appearance_value_editor = None;
        self.settings_ui.named_theme_editor = None;
        self.settings_ui.shortcut_editor = Some(ShortcutEditor {
            command_id: command_id.to_string(),
            binding,
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn cancel_shortcut_editor(&mut self, cx: &mut Context<Self>) {
        if self.settings_ui.shortcut_editor.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn candidate_shortcut_overrides(
        &self,
        command_id: &str,
        binding: &str,
    ) -> BTreeMap<String, String> {
        let mut overrides = self.settings.shortcut_bindings.clone();
        if EDITABLE_SHORTCUTS
            .iter()
            .any(|shortcut| shortcut.id == command_id && shortcut.default_binding == binding)
        {
            overrides.remove(command_id);
        } else {
            overrides.insert(command_id.to_string(), binding.to_string());
        }
        overrides
    }

    pub(crate) fn submit_shortcut_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.settings_ui.shortcut_editor.as_ref() else {
            return;
        };
        let overrides = self.candidate_shortcut_overrides(&editor.command_id, &editor.binding);
        if let Err(error) = validate_shortcut_overrides(&overrides) {
            self.settings_ui.shortcut_editor.as_mut().unwrap().error = Some(error);
            cx.notify();
            return;
        }
        let label = EDITABLE_SHORTCUTS
            .iter()
            .find(|shortcut| shortcut.id == editor.command_id)
            .map(|shortcut| shortcut.label)
            .unwrap_or("Shortcut");
        let binding = display_binding(&editor.binding);
        self.settings.shortcut_bindings = overrides;
        self.settings_ui.shortcut_editor = None;
        self.install_shortcut_bindings(cx);
        self.finish_settings_change(format!("{label}: {binding}"), cx);
    }

    pub(crate) fn handle_shortcut_editor_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" {
            self.cancel_shortcut_editor(cx);
            cx.stop_propagation();
            return;
        }
        let Some(binding) = binding_from_keystroke(&event.keystroke) else {
            cx.stop_propagation();
            return;
        };
        let Some(editor) = self.settings_ui.shortcut_editor.as_ref() else {
            return;
        };
        let overrides = self.candidate_shortcut_overrides(&editor.command_id, &binding);
        let error = validate_shortcut_overrides(&overrides).err();
        if let Some(editor) = self.settings_ui.shortcut_editor.as_mut() {
            editor.binding = binding;
            editor.error = error;
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn reset_shortcut(&mut self, command_id: &str, cx: &mut Context<Self>) {
        if self.settings.shortcut_bindings.remove(command_id).is_some() {
            self.install_shortcut_bindings(cx);
            self.finish_settings_change("Shortcut restored to default", cx);
        }
    }

    pub(crate) fn reset_all_shortcuts(&mut self, cx: &mut Context<Self>) {
        if !self.settings.shortcut_bindings.is_empty() {
            self.settings.shortcut_bindings.clear();
            self.settings_ui.shortcut_editor = None;
            self.install_shortcut_bindings(cx);
            self.finish_settings_change("All shortcuts restored to defaults", cx);
        }
    }

    pub(crate) fn cancel_named_theme_editor(&mut self, cx: &mut Context<Self>) {
        if self.settings_ui.named_theme_editor.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn submit_named_theme_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.settings_ui.named_theme_editor.as_ref() else {
            return;
        };
        let name = match validate_theme_name(&editor.input) {
            Ok(name) => name,
            Err(error) => {
                self.settings_ui.named_theme_editor.as_mut().unwrap().error = Some(error);
                cx.notify();
                return;
            }
        };
        let mut themes = self.settings.named_themes.clone();
        themes.insert(
            name.clone(),
            ThemeSpec::from_appearance(&self.settings.appearance),
        );
        match validate_theme_map(themes) {
            Ok(themes) => {
                self.settings.named_themes = themes;
                self.settings_ui.named_theme_editor = None;
                self.finish_settings_change(format!("Saved theme: {name}"), cx);
            }
            Err(error) => {
                self.settings_ui.named_theme_editor.as_mut().unwrap().error = Some(error);
                cx.notify();
            }
        }
    }

    pub(crate) fn handle_named_theme_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => self.cancel_named_theme_editor(cx),
            "enter" => self.submit_named_theme_editor(cx),
            "backspace" => {
                if let Some(editor) = self.settings_ui.named_theme_editor.as_mut() {
                    editor.input.pop();
                    editor.error = None;
                    cx.notify();
                }
            }
            _ if !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(text) = event.keystroke.key_char.as_deref()
                    && !text.chars().any(char::is_control)
                    && let Some(editor) = self.settings_ui.named_theme_editor.as_mut()
                    && editor.input.len() + text.len() <= 80
                {
                    editor.input.push_str(text);
                    editor.error = None;
                    cx.notify();
                }
            }
            _ => {}
        }
        cx.stop_propagation();
    }

    pub(crate) fn apply_named_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(theme) = self.settings.named_themes.get(name).cloned() else {
            self.show_toast("Theme no longer exists", ToastKind::Warning, cx);
            return;
        };
        theme.apply_to(&mut self.settings.appearance);
        self.finish_settings_change(format!("Applied theme: {name}"), cx);
    }

    pub(crate) fn apply_default_theme(&mut self, cx: &mut Context<Self>) {
        ThemeSpec::from_appearance(&AppearanceSettings::default())
            .apply_to(&mut self.settings.appearance);
        self.finish_settings_change("Applied default theme", cx);
    }

    pub(crate) fn update_named_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(theme) = self.settings.named_themes.get_mut(name) {
            *theme = ThemeSpec::from_appearance(&self.settings.appearance);
            self.finish_settings_change(format!("Updated theme: {name}"), cx);
        }
    }

    pub(crate) fn delete_named_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.settings.named_themes.remove(name).is_some() {
            self.finish_settings_change(format!("Deleted theme: {name}"), cx);
        }
    }

    pub(crate) fn copy_current_theme(&mut self, cx: &mut Context<Self>) {
        let theme = ThemeSpec::from_appearance(&self.settings.appearance);
        match serde_json::to_string_pretty(&theme) {
            Ok(json) => {
                cx.write_to_clipboard(ClipboardItem::new_string(json));
                self.show_toast("Copied current theme JSON", ToastKind::Success, cx);
            }
            Err(error) => self.show_toast(
                format!("Unable to export current theme: {error}"),
                ToastKind::Warning,
                cx,
            ),
        }
    }

    pub(crate) fn copy_all_named_themes(&mut self, cx: &mut Context<Self>) {
        match serde_json::to_string_pretty(&self.settings.named_themes) {
            Ok(json) => {
                cx.write_to_clipboard(ClipboardItem::new_string(json));
                self.show_toast("Copied all named themes JSON", ToastKind::Success, cx);
            }
            Err(error) => self.show_toast(
                format!("Unable to export named themes: {error}"),
                ToastKind::Warning,
                cx,
            ),
        }
    }

    pub(crate) fn next_imported_theme_name(&self) -> String {
        let base = "Imported theme";
        if !self.settings.named_themes.contains_key(base) {
            return base.to_string();
        }
        (2..=100)
            .map(|suffix| format!("{base} {suffix}"))
            .find(|name| !self.settings.named_themes.contains_key(name))
            .unwrap_or_else(|| format!("{base} 101"))
    }

    pub(crate) fn import_named_themes_from_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(json) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.show_toast(
                "Clipboard does not contain theme JSON",
                ToastKind::Warning,
                cx,
            );
            return;
        };
        let imported = if let Ok(theme) = serde_json::from_str::<ThemeSpec>(&json) {
            BTreeMap::from([(self.next_imported_theme_name(), theme)])
        } else {
            match serde_json::from_str::<BTreeMap<String, ThemeSpec>>(&json) {
                Ok(themes) => themes,
                Err(_) => {
                    self.show_toast("Clipboard theme JSON is invalid", ToastKind::Warning, cx);
                    return;
                }
            }
        };
        let imported = match validate_theme_map(imported) {
            Ok(themes) if !themes.is_empty() => themes,
            Ok(_) => {
                self.show_toast("Theme map is empty", ToastKind::Warning, cx);
                return;
            }
            Err(error) => {
                self.show_toast(
                    format!("Invalid theme JSON: {error}"),
                    ToastKind::Warning,
                    cx,
                );
                return;
            }
        };
        let count = imported.len();
        let mut themes = self.settings.named_themes.clone();
        themes.extend(imported);
        match validate_theme_map(themes) {
            Ok(themes) => {
                self.settings.named_themes = themes;
                self.finish_settings_change(format!("Imported {count} theme(s)"), cx);
            }
            Err(error) => self.show_toast(
                format!("Unable to import themes: {error}"),
                ToastKind::Warning,
                cx,
            ),
        }
    }

    pub(crate) fn cycle_border_radius(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance.border_radius = match self.settings.appearance.border_radius {
            0 => 4,
            4 => 8,
            _ => 0,
        };
        self.finish_settings_change(
            format!(
                "Border radius: {} px",
                self.settings.appearance.border_radius
            ),
            cx,
        );
    }

    pub(crate) fn cycle_icon_size(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance.icon_size = next_u8(
            self.settings.appearance.icon_size,
            &[10, 12, 14, 16, 20, 24],
        );
        self.finish_settings_change(
            format!("Icon size: {} px", self.settings.appearance.icon_size),
            cx,
        );
    }

    pub(crate) fn cycle_undo_timeout(&mut self, cx: &mut Context<Self>) {
        self.settings.behavior.undo_timeout_minutes = next_u32(
            self.settings.behavior.undo_timeout_minutes,
            &[1, 5, 15, 30, 60],
        );
        self.undo_ledger.set_timeout(Duration::from_secs(
            u64::from(self.settings.behavior.undo_timeout_minutes) * 60,
        ));
        self.finish_settings_change(
            format!(
                "Undo timeout: {} minute(s)",
                self.settings.behavior.undo_timeout_minutes
            ),
            cx,
        );
    }

    pub(crate) fn toggle_system_files(&mut self, cx: &mut Context<Self>) {
        self.browser.toggle_system_files();
        self.settings.view.show_system_files = self.browser.show_system_files();
        self.apply_global_browser_preferences();
        let state = on_off(self.settings.view.show_system_files);
        self.finish_settings_change(format!("System files: {state}"), cx);
    }

    pub(crate) fn toggle_status_bar(&mut self, cx: &mut Context<Self>) {
        self.settings.view.show_status_bar = !self.settings.view.show_status_bar;
        let state = on_off(self.settings.view.show_status_bar);
        self.finish_settings_change(format!("Status bar: {state}"), cx);
    }

    pub(crate) fn toggle_preview_panel(&mut self, cx: &mut Context<Self>) {
        self.settings.view.show_preview_panel = !self.settings.view.show_preview_panel;
        if self.settings.view.show_preview_panel {
            self.sync_pinned_preview(cx);
        } else {
            self.close_preview(cx);
        }
        let state = on_off(self.settings.view.show_preview_panel);
        self.finish_settings_change(format!("Pinned preview: {state}"), cx);
        self.persist_session();
    }

    pub(crate) fn toggle_confirm_delete(&mut self, cx: &mut Context<Self>) {
        self.settings.behavior.confirm_before_delete =
            !self.settings.behavior.confirm_before_delete;
        let state = on_off(self.settings.behavior.confirm_before_delete);
        self.finish_settings_change(format!("Delete confirmation: {state}"), cx);
    }

    pub(crate) fn toggle_script_preview(&mut self, cx: &mut Context<Self>) {
        self.settings.behavior.preview_executable_scripts =
            !self.settings.behavior.preview_executable_scripts;
        let preview_path = self
            .preview
            .state
            .path()
            .filter(|path| preview_panel::is_executable_script(path))
            .map(Path::to_path_buf);
        if let Some(path) = preview_path {
            self.start_preview(path, cx);
        }
        let state = on_off(self.settings.behavior.preview_executable_scripts);
        self.finish_settings_change(format!("Executable-script previews: {state}"), cx);
    }

    pub(crate) fn toggle_error_reporting(&mut self, cx: &mut Context<Self>) {
        self.settings.behavior.enable_error_reporting =
            !self.settings.behavior.enable_error_reporting;
        let state = on_off(self.settings.behavior.enable_error_reporting);
        self.finish_settings_change(format!("Error reporting: {state}"), cx);
    }

    pub(crate) fn toggle_reduce_motion(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance.reduce_motion = !self.settings.appearance.reduce_motion;
        let state = on_off(self.settings.appearance.reduce_motion);
        self.finish_settings_change(format!("Reduce motion: {state}"), cx);
    }

    pub(crate) fn toggle_high_contrast(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance.high_contrast = !self.settings.appearance.high_contrast;
        let state = on_off(self.settings.appearance.high_contrast);
        self.finish_settings_change(format!("High contrast: {state}"), cx);
    }

    pub(crate) fn render_named_theme_editor(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = self.settings_ui.named_theme_editor.clone() else {
            return div().into_any_element();
        };
        let replacing = validate_theme_name(&editor.input)
            .ok()
            .is_some_and(|name| self.settings.named_themes.contains_key(&name));

        div()
            .id("named-theme-backdrop")
            .debug_selector(|| "named-theme-backdrop".to_string())
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_center()
            .px_4()
            .bg(with_alpha(rgb(0x000000), 0.58))
            .child(
                div()
                    .id("named-theme-editor")
                    .debug_selector(|| "named-theme-editor".to_string())
                    .role(Role::Dialog)
                    .aria_label("Save named theme")
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w_full()
                    .max_w(px(520.0 * self.palette.scale))
                    .p_5()
                    .border_1()
                    .border_color(self.palette.accent)
                    .rounded_sm()
                    .bg(self.palette.panel)
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Save named theme"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(
                                "Captures appearance from theme mode through reduced motion. Default is reserved.",
                            ),
                    )
                    .child(
                        div()
                            .id("named-theme-name-input")
                            .debug_selector(|| "named-theme-name-input".to_string())
                            .w_full()
                            .child(
                                self.native_text_input_element(TextInputTarget::NamedTheme)
                                    .unwrap_or_else(|| div().child(editor.input.clone()).into_any_element()),
                            ),
                    )
                    .when(replacing, |panel| {
                        panel.child(
                            div()
                                .text_xs()
                                .text_color(rgb(0xffb86c))
                                .child("Saving will update the existing theme with this name"),
                        )
                    })
                    .when_some(editor.error, |panel, error| {
                        panel.child(
                            div()
                                .id("named-theme-name-error")
                                .debug_selector(|| "named-theme-name-error".to_string())
                                .text_xs()
                                .text_color(rgb(0xff6b6b))
                                .child(error),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                toolbar_button(
                                    "named-theme-name-save",
                                    if replacing { "Update" } else { "Save" },
                                    self.palette.accent,
                                )
                                .debug_selector(|| "named-theme-name-save".to_string())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.submit_named_theme_editor(cx)
                                })),
                            )
                            .child(
                                toolbar_button(
                                    "named-theme-name-cancel",
                                    "Cancel",
                                    self.palette.control,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_named_theme_editor(cx)
                                })),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .whitespace_nowrap()
                                    .text_right()
                                    .text_xs()
                                    .text_color(self.palette.muted)
                                    .child("Enter saves • Esc cancels"),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn render_shortcut_editor(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = self.settings_ui.shortcut_editor.clone() else {
            return div().into_any_element();
        };
        let label = EDITABLE_SHORTCUTS
            .iter()
            .find(|shortcut| shortcut.id == editor.command_id)
            .map(|shortcut| shortcut.label)
            .unwrap_or("Shortcut");
        let has_error = editor.error.is_some();
        div()
            .id("shortcut-editor-backdrop")
            .debug_selector(|| "shortcut-editor-backdrop".to_string())
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_center()
            .px_4()
            .bg(with_alpha(rgb(0x000000), 0.58))
            .child(
                div()
                    .id("shortcut-editor")
                    .debug_selector(|| "shortcut-editor".to_string())
                    .role(Role::Dialog)
                    .aria_label(format!("Change shortcut for {label}"))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w_full()
                    .max_w(px(520.0 * self.palette.scale))
                    .p_5()
                    .border_1()
                    .border_color(self.palette.accent)
                    .rounded_sm()
                    .bg(self.palette.panel)
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Rebind {label}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child("Press the desired key combination, then save. Esc cancels."),
                    )
                    .child(
                        div()
                            .id("shortcut-capture")
                            .debug_selector(|| "shortcut-capture".to_string())
                            .w_full()
                            .px_3()
                            .py_3()
                            .border_1()
                            .border_color(if editor.error.is_some() {
                                rgb(0xff6b6b)
                            } else {
                                self.palette.accent
                            })
                            .bg(self.palette.surface)
                            .text_center()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(display_binding(&editor.binding)),
                    )
                    .when_some(editor.error, |panel, error| {
                        panel.child(
                            div()
                                .id("shortcut-error")
                                .debug_selector(|| "shortcut-error".to_string())
                                .text_xs()
                                .text_color(rgb(0xff6b6b))
                                .child(error),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                toolbar_button(
                                    "shortcut-save",
                                    "Save binding",
                                    if !has_error {
                                        self.palette.accent
                                    } else {
                                        self.palette.disabled
                                    },
                                )
                                .debug_selector(|| "shortcut-save".to_string())
                                .when(!has_error, |button| {
                                    button.on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.submit_shortcut_editor(cx)
                                        }),
                                    )
                                }),
                            )
                            .child(
                                toolbar_button("shortcut-cancel", "Cancel", self.palette.control)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cancel_shortcut_editor(cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn render_appearance_value_editor(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = self.settings_ui.appearance_value_editor.clone() else {
            return div().into_any_element();
        };
        let preview = match editor.kind {
            AppearanceValueKind::Accent => div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(24.0))
                        .h(px(24.0))
                        .rounded_sm()
                        .border_1()
                        .border_color(self.palette.border)
                        .bg(parse_hex_color(editor.input.trim()).unwrap_or(self.palette.disabled)),
                )
                .child("Preview accent")
                .into_any_element(),
            AppearanceValueKind::Font => div()
                .font_family(if editor.input.trim().is_empty() {
                    font_family(&self.settings)
                } else {
                    editor.input.trim().to_string()
                })
                .child("The quick brown fox jumps over the lazy dog")
                .into_any_element(),
        };

        div()
            .id("appearance-value-backdrop")
            .debug_selector(|| "appearance-value-backdrop".to_string())
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_center()
            .px_4()
            .bg(with_alpha(rgb(0x000000), 0.58))
            .child(
                div()
                    .id("appearance-value-editor")
                    .debug_selector(|| "appearance-value-editor".to_string())
                    .role(Role::Dialog)
                    .aria_label(editor.title())
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w_full()
                    .max_w(px(560.0 * self.palette.scale))
                    .p_5()
                    .border_1()
                    .border_color(self.palette.accent)
                    .rounded_sm()
                    .bg(self.palette.panel)
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(editor.title()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(editor.hint()),
                    )
                    .child(
                        div()
                            .id("appearance-value-input")
                            .debug_selector(|| "appearance-value-input".to_string())
                            .w_full()
                            .child(
                                self.native_text_input_element(TextInputTarget::AppearanceValue)
                                    .unwrap_or_else(|| {
                                        div().child(editor.input.clone()).into_any_element()
                                    }),
                            ),
                    )
                    .child(preview)
                    .when_some(editor.error, |panel, error| {
                        panel.child(
                            div()
                                .id("appearance-value-error")
                                .debug_selector(|| "appearance-value-error".to_string())
                                .text_xs()
                                .text_color(rgb(0xff6b6b))
                                .child(error),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                toolbar_button(
                                    "appearance-value-save",
                                    "Save",
                                    self.palette.accent,
                                )
                                .debug_selector(|| "appearance-value-save".to_string())
                                .on_click(cx.listener(
                                    |this, _, _, cx| this.submit_appearance_value_editor(cx),
                                )),
                            )
                            .child(
                                toolbar_button(
                                    "appearance-value-cancel",
                                    "Cancel",
                                    self.palette.control,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| this.cancel_appearance_value_editor(cx),
                                )),
                            )
                            .child(
                                toolbar_button(
                                    "appearance-value-preset",
                                    "Use preset",
                                    self.palette.control,
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.use_appearance_preset(cx)),
                                ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .whitespace_nowrap()
                                    .text_right()
                                    .text_xs()
                                    .text_color(self.palette.muted)
                                    .child("Enter saves • Esc cancels"),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn render_settings_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if !self.settings_ui.panel_open {
            return div().into_any_element();
        }
        let settings_palette = self.palette;
        let settings_control = move |id: &'static str, label: &str, active| {
            settings_control_with_palette(settings_palette, id, label.to_string(), active)
        };

        let mut general_files = Vec::new();
        let mut general_workspace = Vec::new();
        let mut general_safety = Vec::new();
        let mut general_advanced = Vec::new();
        macro_rules! toggle_setting {
            ($target:ident, $id:literal, $label:literal, $description:literal, $value:expr, $method:ident) => {
                $target.push(
                    settings_toggle_control(self.palette, $id, $label, $description, $value, true)
                        .on_click(cx.listener(|this, _, _, cx| this.$method(cx)))
                        .into_any_element(),
                );
            };
        }
        toggle_setting!(
            general_files,
            "settings-hidden",
            "Hidden files",
            "Show dotfiles and items marked hidden by the operating system.",
            self.browser.show_hidden(),
            toggle_hidden
        );
        toggle_setting!(
            general_files,
            "settings-system",
            "System files",
            "Include protected operating-system entries in folder listings.",
            self.browser.show_system_files(),
            toggle_system_files
        );
        toggle_setting!(
            general_files,
            "settings-folder-sizes",
            "Folder sizes",
            "Calculate directory sizes in the background; large trees may take time.",
            self.calculate_folder_sizes,
            toggle_folder_sizes
        );
        toggle_setting!(
            general_workspace,
            "settings-preview-panel",
            "Pinned preview",
            "Keep the selected file preview visible beside or below the listing.",
            self.settings.view.show_preview_panel,
            toggle_preview_panel
        );
        toggle_setting!(
            general_workspace,
            "settings-status",
            "Status bar",
            "Show selection, filter, operation, disk, and watcher context.",
            self.settings.view.show_status_bar,
            toggle_status_bar
        );
        toggle_setting!(
            general_safety,
            "settings-confirm-delete",
            "Confirm before Trash",
            "Ask before moving selected files to the operating-system Trash.",
            self.settings.behavior.confirm_before_delete,
            toggle_confirm_delete
        );
        toggle_setting!(
            general_safety,
            "settings-script-preview",
            "Executable-script previews",
            "Allow source previews for executable script formats; scripts are never run.",
            self.settings.behavior.preview_executable_scripts,
            toggle_script_preview
        );
        toggle_setting!(
            general_advanced,
            "settings-error-reporting",
            "Local error reporting",
            "Keep path-redacted error details in memory for diagnostics export.",
            self.settings.behavior.enable_error_reporting,
            toggle_error_reporting
        );
        toggle_setting!(
            general_advanced,
            "settings-remote-drives",
            "Remote drives",
            "Enable rclone-backed remote profiles and automatic reconnect behavior.",
            self.settings.behavior.remote_drives_enabled,
            toggle_remote_drives
        );
        let undo_choices = [1_u32, 5, 15, 30, 60]
            .into_iter()
            .map(|minutes| {
                settings_segment_button(
                    self.palette,
                    ("settings-undo-timeout-option", minutes as usize),
                    if minutes == 60 {
                        "1 hr".to_string()
                    } else {
                        format!("{minutes} min")
                    },
                    self.settings.behavior.undo_timeout_minutes == minutes,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_undo_timeout(minutes, cx)))
                .into_any_element()
            })
            .collect::<Vec<_>>();
        general_advanced.push(
            settings_field_row(
                "Undo window",
                "How long completed file operations remain available for Undo.",
                self.palette,
            )
            .child(
                div()
                    .id("settings-undo-window")
                    .role(Role::RadioGroup)
                    .aria_label("Undo window")
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_1()
                    .children(undo_choices),
            )
            .into_any_element(),
        );

        let theme_choices = [ThemeMode::Dark, ThemeMode::Light, ThemeMode::System]
            .into_iter()
            .enumerate()
            .map(|(index, theme)| {
                settings_segment_button(
                    self.palette,
                    ("settings-theme-option", index),
                    theme.label(),
                    self.settings.appearance.theme == theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_theme(theme, cx)))
                .into_any_element()
            })
            .collect::<Vec<_>>();
        let density_choices = [Density::Comfortable, Density::Compact]
            .into_iter()
            .enumerate()
            .map(|(index, density)| {
                settings_segment_button(
                    self.palette,
                    ("settings-density-option", index),
                    density.label(),
                    self.settings.appearance.density == density,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_density(density, cx)))
                .into_any_element()
            })
            .collect::<Vec<_>>();
        let radius_choices = [0_u8, 4, 8]
            .into_iter()
            .enumerate()
            .map(|(index, radius)| {
                settings_segment_button(
                    self.palette,
                    ("settings-radius-option", index),
                    format!("{radius} px"),
                    self.settings.appearance.border_radius == radius,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_border_radius(radius, cx)))
                .into_any_element()
            })
            .collect::<Vec<_>>();
        let accent_label = if self.settings.appearance.accent == AccentColor::Custom {
            self.settings.appearance.accent_custom.clone()
        } else {
            self.settings.appearance.accent.label().to_string()
        };
        let accent_options = [
            AccentColor::Blue,
            AccentColor::Green,
            AccentColor::Purple,
            AccentColor::Orange,
            AccentColor::Pink,
            AccentColor::Custom,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, accent)| {
            let selected = self.settings.appearance.accent == accent;
            settings_segment_button(
                self.palette,
                ("settings-accent-option", index),
                accent.label(),
                selected,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.set_accent_choice(accent, cx)))
            .into_any_element()
        })
        .collect::<Vec<_>>();
        let font_label = if self.settings.appearance.font == FontChoice::Custom {
            self.settings.appearance.font_custom.clone()
        } else {
            self.settings.appearance.font.label().to_string()
        };
        let font_options = [
            FontChoice::Mono,
            FontChoice::System,
            FontChoice::Serif,
            FontChoice::Custom,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, font)| {
            let selected = self.settings.appearance.font == font;
            settings_segment_button(
                self.palette,
                ("settings-font-option", index),
                font.label(),
                selected,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.set_font_choice(font, cx)))
            .into_any_element()
        })
        .collect::<Vec<_>>();

        let mut appearance = vec![
            settings_field_row(
                "Theme",
                "Choose a dark, light, or operating-system appearance.",
                self.palette,
            )
            .child(
                div()
                    .id("settings-theme-options")
                    .debug_selector(|| "settings-theme-options".to_string())
                    .role(Role::RadioGroup)
                    .aria_label("Theme")
                    .flex()
                    .gap_1()
                    .children(theme_choices),
            )
            .into_any_element(),
            div()
                .flex()
                .flex_col()
                .child(
                    settings_field_row(
                        "Accent color",
                        "Used for selection, focus, progress, and active controls.",
                        self.palette,
                    )
                    .child(
                        settings_segment_button(
                            self.palette,
                            "settings-accent",
                            accent_label,
                            self.settings_ui.selector == Some(SettingsSelector::Accent),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings_ui.selector =
                                if this.settings_ui.selector == Some(SettingsSelector::Accent) {
                                    None
                                } else {
                                    Some(SettingsSelector::Accent)
                                };
                            cx.notify();
                        })),
                    ),
                )
                .when(
                    self.settings_ui.selector == Some(SettingsSelector::Accent),
                    |selector| {
                        selector.child(
                            div()
                                .id("settings-accent-options")
                                .role(Role::RadioGroup)
                                .aria_label("Accent color")
                                .flex()
                                .flex_wrap()
                                .gap_1()
                                .px_3()
                                .pb_2()
                                .children(accent_options),
                        )
                    },
                )
                .into_any_element(),
            settings_field_row(
                "Density",
                "Adjust spacing in common controls and Grid view without changing type size.",
                self.palette,
            )
            .child(
                div()
                    .id("settings-density-options")
                    .role(Role::RadioGroup)
                    .aria_label("Density")
                    .flex()
                    .gap_1()
                    .children(density_choices),
            )
            .into_any_element(),
        ];
        appearance.push(self.render_settings_slider(
            SettingsSlider::UiScale,
            "settings-ui-scale",
            "UI scale",
            "Scale interface typography, spacing, and common controls together.",
            cx,
        ));
        appearance.push(self.render_settings_slider(
            SettingsSlider::ListRowHeight,
            "settings-row-height",
            "List row height",
            "Control vertical density in List and Column views.",
            cx,
        ));
        appearance.push(self.render_settings_slider(
            SettingsSlider::GridWidth,
            "settings-grid-width",
            "Grid item width",
            "Set the minimum thumbnail card width in Grid view.",
            cx,
        ));
        appearance.push(
            div()
                .flex()
                .flex_col()
                .child(
                    settings_field_row(
                        "Font",
                        "Choose the interface font family or enter an installed custom font.",
                        self.palette,
                    )
                    .child(
                        settings_segment_button(
                            self.palette,
                            "settings-font",
                            font_label,
                            self.settings_ui.selector == Some(SettingsSelector::Font),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings_ui.selector =
                                if this.settings_ui.selector == Some(SettingsSelector::Font) {
                                    None
                                } else {
                                    Some(SettingsSelector::Font)
                                };
                            cx.notify();
                        })),
                    ),
                )
                .when(
                    self.settings_ui.selector == Some(SettingsSelector::Font),
                    |selector| {
                        selector.child(
                            div()
                                .id("settings-font-options")
                                .role(Role::RadioGroup)
                                .aria_label("Font")
                                .flex()
                                .flex_wrap()
                                .gap_1()
                                .px_3()
                                .pb_2()
                                .children(font_options),
                        )
                    },
                )
                .into_any_element(),
        );
        appearance.push(
            settings_field_row(
                "Corner radius",
                "Apply a consistent radius to supported surfaces and controls.",
                self.palette,
            )
            .child(
                div()
                    .id("settings-radius-options")
                    .role(Role::RadioGroup)
                    .aria_label("Corner radius")
                    .flex()
                    .gap_1()
                    .children(radius_choices),
            )
            .into_any_element(),
        );
        appearance.push(self.render_settings_slider(
            SettingsSlider::IconSize,
            "settings-icon-size",
            "Icon size",
            "Adjust file and toolbar icon size independently of UI scale.",
            cx,
        ));
        appearance.push(
            settings_toggle_control(
                self.palette,
                "settings-reduce-motion",
                "Reduce motion",
                "Limit nonessential movement and keep future transitions calm.",
                self.settings.appearance.reduce_motion,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| this.toggle_reduce_motion(cx)))
            .into_any_element(),
        );
        appearance.push(
            settings_toggle_control(
                self.palette,
                "settings-high-contrast",
                "High contrast",
                "Increase separation between text, controls, and surfaces.",
                self.settings.appearance.high_contrast,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| this.toggle_high_contrast(cx)))
            .into_any_element(),
        );
        let appearance_accessibility = appearance.split_off(9);
        let appearance_typography = appearance.split_off(6);
        let appearance_layout = appearance.split_off(2);
        let appearance_color = appearance;

        let theme_rows: Vec<_> = self
            .settings
            .named_themes
            .keys()
            .cloned()
            .enumerate()
            .map(|(index, name)| {
                let apply_name = name.clone();
                let update_name = name.clone();
                let delete_name = name.clone();
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        div()
                            .min_w(px(140.0))
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .child(
                        toolbar_button(("named-theme-apply", index), "Apply", self.palette.control)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.apply_named_theme(&apply_name, cx)
                            })),
                    )
                    .child(
                        toolbar_button(
                            ("named-theme-update", index),
                            "Update",
                            self.palette.control,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_named_theme(&update_name, cx)
                        })),
                    )
                    .child(
                        toolbar_button(("named-theme-delete", index), "Delete", rgb(0x6b3434))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.request_settings_confirmation(
                                    SettingsConfirmation::DeleteTheme(delete_name.clone()),
                                    cx,
                                )
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let named_theme_count = self.settings.named_themes.len();
        let named_themes = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Named themes"),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        settings_control("settings-theme-save", "Save current", false)
                            .debug_selector(|| "settings-theme-save".to_string())
                            .on_click(
                                cx.listener(|this, _, _, cx| this.open_named_theme_editor(cx)),
                            ),
                    )
                    .child(
                        settings_control("settings-theme-default", "Apply default", false)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.apply_default_theme(cx)
                            })),
                    )
                    .child(
                        settings_control("settings-theme-copy-current", "Copy current", false)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.copy_current_theme(cx)),
                            ),
                    )
                    .child(
                        settings_control("settings-theme-copy-all", "Copy all", false).on_click(
                            cx.listener(|this, _, _, cx| this.copy_all_named_themes(cx)),
                        ),
                    )
                    .child(
                        settings_control(
                            "settings-theme-import",
                            "Import clipboard",
                            false,
                        )
                        .debug_selector(|| "settings-theme-import".to_string())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.import_named_themes_from_clipboard(cx)
                        })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(self.palette.muted)
                    .child(format!(
                        "{named_theme_count} custom theme(s) • imports accept one legacy ThemeSpec or a named map"
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(named_theme_count == 0, |rows| {
                        rows.child(
                            div()
                                .text_xs()
                                .text_color(self.palette.muted)
                                .child("No custom themes yet"),
                        )
                    })
                    .children(theme_rows),
            );

        let shortcut_rows: Vec<_> = EDITABLE_SHORTCUTS
            .iter()
            .enumerate()
            .map(|(index, shortcut)| {
                let command_id = shortcut.id;
                let reset_id = shortcut.id;
                let binding = binding_for(&self.settings.shortcut_bindings, shortcut.id)
                    .unwrap_or_else(|| shortcut.default_binding.to_string());
                let customized = self.settings.shortcut_bindings.contains_key(shortcut.id);
                div()
                    .id(("shortcut-setting", index))
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .py_1()
                    .child(
                        div()
                            .w(px(110.0))
                            .text_xs()
                            .text_color(self.palette.muted)
                            .child(shortcut.category),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(180.0))
                            .text_sm()
                            .child(shortcut.label),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(self.palette.control)
                            .text_xs()
                            .child(display_binding(&binding)),
                    )
                    .child(
                        toolbar_button(("shortcut-rebind", index), "Change", self.palette.control)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_shortcut_editor(command_id, cx)
                            })),
                    )
                    .when(customized, |row| {
                        row.child(
                            toolbar_button(
                                ("shortcut-reset", index),
                                "Reset",
                                self.palette.control,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.request_settings_confirmation(
                                        SettingsConfirmation::ResetShortcut(reset_id.to_string()),
                                        cx,
                                    )
                                },
                            )),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        let shortcut_override_count = self.settings.shortcut_bindings.len();
        let shortcuts = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Keyboard shortcuts"),
                    )
                    .child(
                        settings_control(
                            "settings-shortcuts-reset",
                            "Reset all",
                            shortcut_override_count > 0,
                        )
                        .debug_selector(|| "settings-shortcuts-reset".to_string())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.request_settings_confirmation(
                                SettingsConfirmation::ResetAllShortcuts,
                                cx,
                            )
                        })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(self.palette.muted)
                    .child(format!(
                        "{shortcut_override_count} customized • duplicate and reserved bindings are rejected"
                    )),
            )
            .child(div().flex().flex_col().gap_1().children(shortcut_rows));

        let helper_rows: Vec<_> = self
            .preview
            .helpers
            .iter()
            .map(|helper| {
                let state = if helper.available {
                    helper.version.as_deref().unwrap_or("available").to_string()
                } else {
                    helper
                        .error
                        .as_deref()
                        .unwrap_or("not installed")
                        .to_string()
                };
                div()
                    .text_xs()
                    .text_color(if helper.available {
                        rgb(0x6fcf97)
                    } else {
                        rgb(0xffb86c)
                    })
                    .child(format!("{}: {state}", helper.name))
                    .into_any_element()
            })
            .collect();
        let import_status = self.settings.legacy_import.as_ref().map(|record| {
            format!(
                "Legacy import: {} retained, {} invalid • source preserved at {}",
                record.imported_keys,
                record.invalid_keys.len(),
                record.source.display()
            )
        });
        let window_width = self
            .layout
            .last_window_bounds
            .width
            .unwrap_or(DEFAULT_WINDOW_WIDTH);
        let window_height = self
            .layout
            .last_window_bounds
            .height
            .unwrap_or(DEFAULT_WINDOW_HEIGHT);
        let palette = self.palette;
        let logical_window_width = window_width / palette.scale.max(0.1);
        let compact_settings = logical_window_width <= 860.0;
        let panel_inset = if compact_settings { 16.0 } else { 24.0 } * palette.scale;
        let panel_width = (window_width - panel_inset * 2.0)
            .max(1.0)
            .min(1040.0 * palette.scale);
        let panel_height = (window_height - panel_inset * 2.0)
            .max(1.0)
            .min(640.0 * palette.scale);
        let tab_rows: Vec<_> = SettingsTab::ALL
            .into_iter()
            .enumerate()
            .map(|(index, tab)| {
                let active = tab == self.settings_ui.tab;
                div()
                    .id(("settings-tab", index))
                    .debug_selector(move || format!("settings-tab-{index}"))
                    .role(Role::Tab)
                    .aria_label(tab.label())
                    .aria_selected(active)
                    .focusable()
                    .tab_stop(true)
                    .flex()
                    .items_center()
                    .h(px(if palette.density == Density::Compact {
                        32.0
                    } else {
                        36.0
                    } * palette.scale))
                    .px_2()
                    .border_l_2()
                    .border_color(if active {
                        palette.accent
                    } else {
                        with_alpha(palette.border, 0.0)
                    })
                    .rounded(px(palette.radius))
                    .bg(if active {
                        palette.selected
                    } else {
                        palette.window
                    })
                    .text_color(if active { palette.text } else { palette.muted })
                    .hover(move |tab| tab.bg(palette.hover).text_color(palette.text))
                    .cursor_pointer()
                    .text_sm()
                    .when(compact_settings, |tab| tab.flex_none())
                    .when(!compact_settings, |tab| tab.w_full())
                    .child(tab.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_ui.tab = tab;
                        if tab == SettingsTab::Plugins {
                            this.start_plugin_status(cx);
                        }
                        if tab == SettingsTab::Integration {
                            this.start_system_integration_status(cx);
                        } else {
                            cx.notify();
                        }
                    }))
                    .into_any_element()
            })
            .collect();
        let folder_handler_enabled = self
            .system
            .integration_status
            .as_ref()
            .is_some_and(|status| status.enabled);
        let folder_handler_available = !self.system.integration_pending
            && self
                .system
                .integration_status
                .as_ref()
                .is_none_or(|status| status.supported);
        let integration_platform = if cfg!(target_os = "macos") {
            "macOS"
        } else if cfg!(windows) {
            "Windows"
        } else {
            "this platform"
        };
        let folder_handler_state = if let Some(error) = self.system.integration_error.as_deref() {
            format!("{integration_platform} integration could not be read: {error}")
        } else if self
            .system
            .integration_status
            .as_ref()
            .is_some_and(|status| !status.supported)
        {
            "Automatic folder opening is available on Windows and macOS.".to_string()
        } else if folder_handler_enabled {
            if cfg!(target_os = "macos") {
                "Folder opens are routed to this installed copy of Explorie. Turning this off restores the previous macOS handler."
                    .to_string()
            } else {
                "Windows folder and drive opens are routed to this installed copy of Explorie. Turning this off restores the previous handler."
                    .to_string()
            }
        } else if cfg!(target_os = "macos") {
            "Keep Finder as the macOS shell, but use Explorie when folders are opened. This is per-user and reversible."
                .to_string()
        } else {
            "Keep Explorer as the Windows shell, but route folder and drive opens to Explorie. This is per-user and reversible."
                .to_string()
        };
        let search_index_health = self.services.search.index_health();
        let search_index_status = format!(
            "Search index: {} cached root{} • {} indexed path{}",
            search_index_health.roots,
            if search_index_health.roots == 1 {
                ""
            } else {
                "s"
            },
            search_index_health.indexed_entries,
            if search_index_health.indexed_entries == 1 {
                ""
            } else {
                "s"
            }
        );
        let (update_message, update_button_label, update_action) = match &self.system.update_status
        {
            UpdateStatus::Idle => (
                "Updates are checked automatically when Explorie starts.".to_string(),
                "Check for updates",
                Some(UpdateAction::Check),
            ),
            UpdateStatus::Checking => (
                "Checking the latest published release…".to_string(),
                "Checking…",
                None,
            ),
            UpdateStatus::UpToDate => (
                "Explorie is up to date.".to_string(),
                "Check again",
                Some(UpdateAction::Check),
            ),
            UpdateStatus::Available(update) => (
                format!("Explorie {} is available.", update.version),
                "Download and install",
                Some(UpdateAction::Download(update.clone())),
            ),
            UpdateStatus::Downloading(update) => (
                format!("Downloading and verifying Explorie {}…", update.version),
                "Downloading…",
                None,
            ),
            UpdateStatus::Ready(update) => (
                format!("Explorie {} is verified and ready.", update.info.version),
                "Install and restart",
                Some(UpdateAction::Install(update.clone())),
            ),
            UpdateStatus::Installing => (
                "Replacing Explorie and preparing to reopen…".to_string(),
                "Installing…",
                None,
            ),
            UpdateStatus::Failed(error) => (
                format!("Update unavailable: {error}"),
                "Retry",
                Some(UpdateAction::Check),
            ),
        };
        let update_action_enabled = update_action.is_some();
        let update_control = toolbar_button_enabled(
            "settings-update-action",
            update_button_label,
            self.palette.control,
            update_action_enabled,
        )
        .debug_selector(|| "settings-update-action".to_string())
        .on_click(cx.listener(move |this, _, _, cx| {
            let Some(action) = update_action.clone() else {
                return;
            };
            match action {
                UpdateAction::Check => this.start_update_check(true, cx),
                UpdateAction::Download(update) => this.start_update_download(update, cx),
                UpdateAction::Install(update) => this.start_prepared_update(update, cx),
            }
        }));
        let active_content = match self.settings_ui.tab {
            SettingsTab::Plugins => self.render_plugin_settings(cx),
            SettingsTab::General => div()
                .id("settings-general")
                .debug_selector(|| "settings-general".to_string())
                .children(vec![
                    settings_section("Files and visibility", general_files, self.palette),
                    settings_section("Workspace", general_workspace, self.palette),
                    settings_section("Safety", general_safety, self.palette),
                    settings_section("Advanced", general_advanced, self.palette),
                ])
                .into_any_element(),
            SettingsTab::Integration => div()
                .id("settings-integration")
                .debug_selector(|| "settings-integration".to_string())
                .flex()
                .flex_col()
                .gap_3()
                .p_3()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("System integration"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(self.palette.muted)
                        .child(format!(
                            "Control how Explorie participates in {integration_platform} without replacing the desktop shell."
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            settings_toggle_control(
                                self.palette,
                                "settings-folder-handler",
                                "Open folders in Explorie",
                                if cfg!(target_os = "macos") {
                                    "Use Explorie for folder opens without replacing Finder."
                                } else {
                                    "Route Windows folder and drive opens here without replacing the desktop shell."
                                },
                                folder_handler_enabled,
                                folder_handler_available,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.system.integration_pending {
                                    this.toggle_system_integration(cx)
                                }
                            })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(self.palette.muted)
                                .child(folder_handler_state),
                        ),
                )
                .children(helper_rows)
                .child(
                    div()
                        .text_xs()
                        .text_color(self.palette.muted)
                        .child(search_index_status),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            settings_control("settings-refresh-helpers", "Refresh helpers", false)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.start_preview_helpers(cx)),
                                ),
                        )
                        .child(
                            settings_control("settings-clear-cache", "Clear preview cache", false)
                                .on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.request_settings_confirmation(
                                            SettingsConfirmation::ClearPreviewCache,
                                            cx,
                                        )
                                    }),
                                ),
                        )
                        .child(
                            settings_control(
                                "settings-rebuild-search-index",
                                "Rebuild search index",
                                false,
                            )
                            .on_click(
                                cx.listener(|this, _, _, cx| this.rebuild_search_index(cx)),
                            ),
                        ),
                )
                .into_any_element(),
            SettingsTab::Appearance => div()
                .id("settings-appearance")
                .debug_selector(|| "settings-appearance".to_string())
                .children(vec![
                    settings_section("Theme and color", appearance_color, self.palette),
                    settings_section("Layout and sizing", appearance_layout, self.palette),
                    settings_section(
                        "Typography and icons",
                        appearance_typography,
                        self.palette,
                    ),
                    settings_section(
                        "Accessibility",
                        appearance_accessibility,
                        self.palette,
                    ),
                ])
                .into_any_element(),
            SettingsTab::Themes => div()
                .id("settings-themes")
                .debug_selector(|| "settings-themes".to_string())
                .p_3()
                .child(named_themes)
                .into_any_element(),
            SettingsTab::Shortcuts => div()
                .id("settings-shortcuts")
                .debug_selector(|| "settings-shortcuts".to_string())
                .p_3()
                .child(shortcuts)
                .into_any_element(),
            SettingsTab::About => div()
                .id("settings-about")
                .debug_selector(|| "settings-about".to_string())
                .flex()
                .flex_col()
                .gap_3()
                .p_3()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("About"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().w(px(120.0)).text_sm().child("Version"))
                        .child(env!("CARGO_PKG_VERSION")),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().w(px(120.0)).text_sm().child("Desktop"))
                        .child("Native GPUI"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().w(px(120.0)).text_sm().child("Updates"))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_start()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(self.palette.muted)
                                        .child(update_message),
                                )
                                .child(update_control),
                        ),
                )
                .when_some(import_status, |section, status| {
                    section.child(div().text_xs().text_color(self.palette.muted).child(status))
                })
                .into_any_element(),
        };
        div()
            .id("settings-backdrop")
            .debug_selector(|| "settings-backdrop".to_string())
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .when(compact_settings, |backdrop| backdrop.p_2())
            .when(!compact_settings, |backdrop| backdrop.p_3())
            .bg(with_alpha(rgb(0x000000), 0.58))
            .occlude()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .id("settings-panel")
                    .debug_selector(|| "settings-panel".to_string())
                    .role(Role::Dialog)
                    .aria_label("Settings")
                    .flex()
                    .flex_col()
                    .w(px(panel_width))
                    .h(px(panel_height))
                    .min_h_0()
                    .rounded(px((self.palette.radius + 2.0).min(12.0)))
                    .border_1()
                    .border_color(self.palette.border)
                    .bg(self.palette.surface)
                    .shadow_lg()
                    .child(
                        div()
                            .id("settings-header")
                            .debug_selector(|| "settings-header".to_string())
                            .flex()
                            .items_center()
                            .h(px(46.0 * self.palette.scale))
                            .flex_none()
                            .px_3()
                            .border_b_1()
                            .border_color(self.palette.border)
                            .bg(self.palette.topbar)
                            .child(
                                div()
                                    .flex_1()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Settings"),
                            )
                            .child(
                                compact_toolbar_button(
                                    "settings-close",
                                    "Close settings",
                                    "close",
                                    self.palette,
                                    false,
                                    true,
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.close_settings_panel(cx)),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .id("settings-content")
                            .debug_selector(|| "settings-content".to_string())
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .when(compact_settings, |content| content.flex_col().gap_2().p_2())
                            .when(!compact_settings, |content| content.gap_3().p_3())
                            .child(
                                div()
                                    .id("settings-tabs")
                                    .debug_selector(|| "settings-tabs".to_string())
                                    .role(Role::TabList)
                                    .aria_label("Settings sections")
                                    .flex()
                                    .gap_1()
                                    .flex_none()
                                    .bg(self.palette.window)
                                    .when(compact_settings, |tabs| {
                                        tabs.flex_row()
                                            .w_full()
                                            .h(px(48.0 * self.palette.scale))
                                            .p_1()
                                            .overflow_x_scroll()
                                    })
                                    .when(!compact_settings, |tabs| {
                                        tabs.flex_col().w(px(172.0 * self.palette.scale)).p_2()
                                    })
                                    .children(tab_rows),
                            )
                            .child(
                                div()
                                    .id("settings-active-section")
                                    .debug_selector(|| "settings-active-section".to_string())
                                    .role(Role::TabPanel)
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .bg(self.palette.window)
                                    .child(active_content),
                            ),
                    )
                    .child(
                        div()
                            .id("settings-footer")
                            .debug_selector(|| "settings-footer".to_string())
                            .flex()
                            .items_center()
                            .h(px(46.0 * self.palette.scale))
                            .flex_none()
                            .gap_3()
                            .px_3()
                            .border_t_1()
                            .border_color(self.palette.border)
                            .bg(self.palette.topbar)
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(self.palette.muted)
                                    .child("Changes apply immediately and persist natively."),
                            )
                            .child(
                                toolbar_button(
                                    "settings-reset",
                                    "Reset to defaults",
                                    self.palette.control,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.request_settings_confirmation(
                                            SettingsConfirmation::ResetSettings,
                                            cx,
                                        )
                                    },
                                )),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(self.palette.tertiary)
                                    .child("Esc"),
                            )
                            .child(
                                div().min_w(px(72.0)).child(
                                    toolbar_button(
                                        "settings-footer-close",
                                        "Close",
                                        self.palette.control,
                                    )
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.close_settings_panel(cx)),
                                    ),
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn render_settings_confirmation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(confirmation) = self.settings_ui.confirmation.as_ref() else {
            return div().into_any_element();
        };
        if self.settings_ui.confirmation_focus_pending {
            self.settings_ui.confirmation_return_focus = window.focused(cx);
            window.focus(&self.settings_ui.confirmation_cancel_focus, cx);
            self.settings_ui.confirmation_focus_pending = false;
        }
        let (title, message, confirm_label, cancel_label, confirm_color): (
            Cow<'_, str>,
            Cow<'_, str>,
            &str,
            &str,
            Rgba,
        ) = match confirmation {
            SettingsConfirmation::ResetSettings => (
                Cow::Borrowed("Reset all settings?"),
                Cow::Borrowed(
                    "Appearance, behavior, view preferences, and shortcut overrides will return to their defaults.",
                ),
                "Reset settings",
                "Cancel",
                rgb(0x8b3340),
            ),
            SettingsConfirmation::ResetShortcut(command_id) => (
                Cow::Borrowed("Reset this shortcut?"),
                Cow::Borrowed(command_id.as_str()),
                "Reset shortcut",
                "Cancel",
                rgb(0x8b3340),
            ),
            SettingsConfirmation::ResetAllShortcuts => (
                Cow::Borrowed("Reset all shortcuts?"),
                Cow::Borrowed("Every customized shortcut will return to its default binding."),
                "Reset shortcuts",
                "Cancel",
                rgb(0x8b3340),
            ),
            SettingsConfirmation::ClearPreviewCache => (
                Cow::Borrowed("Clear preview cache?"),
                Cow::Borrowed(
                    "Generated thumbnails and preview artifacts will be removed and recreated when needed.",
                ),
                "Clear cache",
                "Cancel",
                rgb(0x8b3340),
            ),
            SettingsConfirmation::DeleteTheme(name) => (
                Cow::Borrowed("Delete saved theme?"),
                Cow::Borrowed(name.as_str()),
                "Delete theme",
                "Cancel",
                rgb(0x8b3340),
            ),
            SettingsConfirmation::RemoveCustomField(name) => (
                Cow::Borrowed("Remove custom field?"),
                Cow::Borrowed(name.as_str()),
                "Remove field",
                "Cancel",
                rgb(0x8b3340),
            ),
            SettingsConfirmation::InstallUpdate(update) => (
                Cow::Owned(format!("Install Explorie {}?", update.version)),
                Cow::Borrowed(
                    "The update will be downloaded, verified against the published release and platform security checks, then replace this installation. Explorie will clean up the update files and reopen automatically.",
                ),
                "Download and restart",
                "Later",
                self.palette.accent,
            ),
            SettingsConfirmation::CleanupInstallMedia(offer) => (
                Cow::Borrowed("Finish installing Explorie?"),
                Cow::Owned(format!(
                    "Eject the mounted installer and move {} to Trash. The installed app and your settings stay untouched.",
                    offer
                        .image_path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("the downloaded DMG")
                )),
                "Eject and move to Trash",
                "Keep",
                self.palette.accent,
            ),
            SettingsConfirmation::DiscardDraft(_) => (
                Cow::Borrowed("Discard unsaved changes?"),
                Cow::Borrowed(
                    "This draft has not been applied. Closing it will lose the changes you entered.",
                ),
                "Discard changes",
                "Cancel",
                rgb(0x8b3340),
            ),
        };
        div()
            .id("settings-confirmation-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .bg(with_alpha(rgb(0x000000), 0.64))
            .child(
                div()
                    .id("settings-confirmation")
                    .role(Role::AlertDialog)
                    .aria_label(title.to_string())
                    .key_context("settings-confirmation")
                    .tab_group()
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "tab" => {
                                let focus = if event.keystroke.modifiers.shift
                                    || this
                                        .settings_ui
                                        .confirmation_confirm_focus
                                        .is_focused(window)
                                {
                                    this.settings_ui.confirmation_cancel_focus.clone()
                                } else {
                                    this.settings_ui.confirmation_confirm_focus.clone()
                                };
                                window.focus(&focus, cx);
                            }
                            "escape" => this.cancel_settings_confirmation(window, cx),
                            "enter" => {
                                if this
                                    .settings_ui
                                    .confirmation_confirm_focus
                                    .is_focused(window)
                                {
                                    this.confirm_settings_confirmation(window, cx);
                                } else {
                                    this.cancel_settings_confirmation(window, cx);
                                }
                            }
                            _ => return,
                        }
                        cx.stop_propagation();
                    }))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w_full()
                    .max_w(px(460.0 * self.palette.scale))
                    .p_4()
                    .border_1()
                    .border_color(self.palette.border)
                    .rounded_lg()
                    .bg(self.palette.panel)
                    .shadow_lg()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title.to_string()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(self.palette.muted)
                            .child(message.to_string()),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                toolbar_button(
                                    "settings-confirmation-cancel",
                                    cancel_label,
                                    self.palette.control,
                                )
                                .track_focus(&self.settings_ui.confirmation_cancel_focus)
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.cancel_settings_confirmation(window, cx);
                                    },
                                )),
                            )
                            .child(
                                toolbar_button(
                                    "settings-confirmation-confirm",
                                    confirm_label,
                                    confirm_color,
                                )
                                .track_focus(&self.settings_ui.confirmation_confirm_focus)
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.confirm_settings_confirmation(window, cx);
                                    },
                                )),
                            ),
                    ),
            )
            .into_any_element()
    }
}

/// Routes keystrokes to an open shortcut recorder before the keymap sees them,
/// so chords that are already bound (or are menu key equivalents) can be
/// recorded instead of running their current command.
struct ShortcutRecorder {
    _interceptor: gpui::Subscription,
}

impl gpui::Global for ShortcutRecorder {}

fn install_shortcut_recorder(cx: &mut App) {
    if cx.has_global::<ShortcutRecorder>() {
        return;
    }
    let subscription = cx.intercept_keystrokes(|event, window, cx| {
        let Some(Some(view)) = window.root::<DirectoryWindow>() else {
            return;
        };
        if view.read(cx).settings_ui.shortcut_editor.is_none() {
            return;
        }
        let event = KeyDownEvent {
            keystroke: event.keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        };
        view.update(cx, |view, cx| view.handle_shortcut_editor_key(&event, cx));
        cx.stop_propagation();
    });
    cx.set_global(ShortcutRecorder {
        _interceptor: subscription,
    });
}
