//! Resolved UI colors and tooltip helpers.

use crate::*;

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct UiPalette {
    pub(crate) dark: bool,
    pub(crate) density: Density,
    pub(crate) scale: f32,
    pub(crate) radius: f32,
    pub(crate) icon_size: f32,
    pub(crate) window: Rgba,
    pub(crate) topbar: Rgba,
    pub(crate) panel: Rgba,
    pub(crate) surface: Rgba,
    pub(crate) control: Rgba,
    pub(crate) disabled: Rgba,
    pub(crate) hover: Rgba,
    pub(crate) border: Rgba,
    pub(crate) text: Rgba,
    pub(crate) muted: Rgba,
    pub(crate) tertiary: Rgba,
    pub(crate) accent: Rgba,
    pub(crate) selected: Rgba,
}

impl UiPalette {
    pub(crate) fn for_settings(settings: &AppSettings, appearance: WindowAppearance) -> Self {
        let system_dark = matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let dark = match settings.appearance.theme {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::System => system_dark,
        };
        let accent = match settings.appearance.accent {
            AccentColor::Blue => rgb(0x7cc7ff),
            AccentColor::Green => rgb(0x9ad1a8),
            AccentColor::Purple => rgb(0xb39ddb),
            AccentColor::Orange => rgb(0xffb86b),
            AccentColor::Pink => rgb(0xff8aa0),
            AccentColor::Custom => {
                parse_hex_color(&settings.appearance.accent_custom).unwrap_or_else(|| rgb(0x7cc7ff))
            }
        };
        let density = settings.appearance.density;
        let scale = settings.appearance.ui_scale;
        let radius = f32::from(settings.appearance.border_radius);
        let icon_size = f32::from(settings.appearance.icon_size);
        if settings.appearance.high_contrast {
            let window = if dark { rgb(0x000000) } else { rgb(0xffffff) };
            return Self {
                dark,
                density,
                scale,
                radius,
                icon_size,
                window,
                topbar: if dark { rgb(0x050505) } else { rgb(0xfafafa) },
                panel: if dark { rgb(0x0a0a0a) } else { rgb(0xf5f5f5) },
                surface: if dark { rgb(0x111111) } else { rgb(0xffffff) },
                control: if dark { rgb(0x141414) } else { rgb(0xebebeb) },
                disabled: if dark { rgb(0x1a1a1a) } else { rgb(0xe0e0e0) },
                hover: window.blend(with_alpha(
                    if dark { rgb(0xffffff) } else { rgb(0x000000) },
                    0.12,
                )),
                border: if dark { rgb(0x404040) } else { rgb(0x808080) },
                text: if dark { rgb(0xffffff) } else { rgb(0x000000) },
                muted: if dark { rgb(0xe0e0e0) } else { rgb(0x1a1a1a) },
                tertiary: if dark { rgb(0xb0b0b0) } else { rgb(0x333333) },
                accent,
                selected: window.blend(with_alpha(accent, 0.16)),
            };
        }
        if dark {
            Self {
                dark,
                density,
                scale,
                radius,
                icon_size,
                window: rgb(0x0f0f0f),
                topbar: rgb(0x111111),
                panel: rgb(0x151515),
                surface: rgb(0x181818),
                control: rgb(0x1b1b1b),
                disabled: rgb(0x202020),
                hover: rgb(0x0f0f0f).blend(with_alpha(rgb(0xffffff), 0.045)),
                border: rgb(0x252525),
                text: rgb(0xdddddd),
                muted: rgb(0xaaaaaa),
                tertiary: rgb(0x8a8a8a),
                accent,
                selected: rgb(0x0f0f0f).blend(with_alpha(accent, 0.16)),
            }
        } else {
            Self {
                dark,
                density,
                scale,
                radius,
                icon_size,
                window: rgb(0xf6f6f6),
                topbar: rgb(0xf3f3f3),
                panel: rgb(0xefefef),
                surface: rgb(0xffffff),
                control: rgb(0xe8e8e8),
                disabled: rgb(0xe2e2e2),
                hover: rgb(0xf6f6f6).blend(with_alpha(rgb(0x000000), 0.045)),
                border: rgb(0xd0d0d0),
                text: rgb(0x222222),
                muted: rgb(0x555555),
                tertiary: rgb(0x707070),
                accent,
                selected: rgb(0xf6f6f6).blend(with_alpha(accent, 0.16)),
            }
        }
    }
}

pub(crate) fn parse_hex_color(value: &str) -> Option<Rgba> {
    let value = value.strip_prefix('#')?;
    (value.len() == 6)
        .then(|| u32::from_str_radix(value, 16).ok())
        .flatten()
        .map(rgb)
}

pub(crate) fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}

pub(crate) fn adaptive_hover(color: Rgba) -> Rgba {
    let luminance = 0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b;
    let contrast = if luminance < 0.5 {
        rgb(0xffffff)
    } else {
        rgb(0x000000)
    };
    color.blend(with_alpha(contrast, 0.08))
}

pub(crate) fn contrasting_text(color: Rgba) -> Rgba {
    let luminance = 0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b;
    if luminance > 0.56 {
        rgb(0x171717)
    } else {
        rgb(0xffffff)
    }
}

pub(crate) struct AppTooltip {
    pub(crate) text: SharedString,
    pub(crate) palette: UiPalette,
}

impl Render for AppTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(320.0 * self.palette.scale))
            .px_2()
            .py_1()
            .rounded(px(self.palette.radius.min(6.0)))
            .border_1()
            .border_color(self.palette.border)
            .bg(self.palette.panel)
            .text_xs()
            .text_color(self.palette.text)
            .child(self.text.clone())
    }
}

pub(crate) fn app_tooltip(
    text: impl Into<SharedString>,
    palette: UiPalette,
    cx: &mut App,
) -> gpui::AnyView {
    let text = text.into();
    cx.new(|_| AppTooltip { text, palette }).into()
}

pub(crate) fn control_tooltip_text(label: &str) -> String {
    let shortcut = match label {
        "Close settings" | "Close preview" | "Close Quick Look" => Some("Esc"),
        _ => None,
    };
    shortcut.map_or_else(
        || label.to_string(),
        |shortcut| format!("{label}  •  {shortcut}"),
    )
}

pub(crate) fn shortcut_tooltip_text(
    label: &str,
    overrides: &BTreeMap<String, String>,
    shortcut_id: &str,
) -> String {
    binding_for(overrides, shortcut_id).map_or_else(
        || label.to_string(),
        |binding| format!("{label}  •  {}", display_binding(&binding)),
    )
}
