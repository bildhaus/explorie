use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};

use explorie_native_services::{RemoteDriveProfile, validate_remote_drive_profile};

use crate::{EntryFilter, SortDirection, SortKey, ViewMode};

const SETTINGS_VERSION: u32 = 4;
const LEGACY_EXPORT_FILE: &str = "legacy-local-storage.json";
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Dark,
    Light,
    System,
}

impl ThemeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::System => "System",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::System,
            Self::System => Self::Dark,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AccentColor {
    Blue,
    Green,
    Purple,
    Orange,
    Pink,
    Custom,
}

impl AccentColor {
    pub fn label(self) -> &'static str {
        match self {
            Self::Blue => "Blue",
            Self::Green => "Green",
            Self::Purple => "Purple",
            Self::Orange => "Orange",
            Self::Pink => "Pink",
            Self::Custom => "Custom",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Blue => Self::Green,
            Self::Green => Self::Purple,
            Self::Purple => Self::Orange,
            Self::Orange => Self::Pink,
            Self::Pink => Self::Custom,
            Self::Custom => Self::Blue,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Comfortable,
    Compact,
}

impl Density {
    pub fn label(self) -> &'static str {
        match self {
            Self::Comfortable => "Comfortable",
            Self::Compact => "Compact",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Comfortable => Self::Compact,
            Self::Compact => Self::Comfortable,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FontChoice {
    Mono,
    System,
    Serif,
    Custom,
}

impl FontChoice {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mono => "Mono",
            Self::System => "System",
            Self::Serif => "Serif",
            Self::Custom => "Custom",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Mono => Self::System,
            Self::System => Self::Serif,
            Self::Serif => Self::Custom,
            Self::Custom => Self::Mono,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewSettings {
    pub view_mode: ViewMode,
    pub show_hidden: bool,
    pub show_system_files: bool,
    pub filter_mode: EntryFilter,
    pub show_folder_sizes: bool,
    pub show_preview_panel: bool,
    pub show_status_bar: bool,
    pub sort_key: SortKey,
    pub sort_direction: SortDirection,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    #[serde(default = "default_preview_panel_width")]
    pub preview_panel_width: f32,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            view_mode: ViewMode::List,
            show_hidden: false,
            show_system_files: false,
            filter_mode: EntryFilter::All,
            show_folder_sizes: false,
            show_preview_panel: false,
            show_status_bar: true,
            sort_key: SortKey::Name,
            sort_direction: SortDirection::Ascending,
            sidebar_width: default_sidebar_width(),
            preview_panel_width: default_preview_panel_width(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceSettings {
    pub theme: ThemeMode,
    pub accent: AccentColor,
    pub accent_custom: String,
    pub density: Density,
    pub ui_scale: f32,
    pub list_row_height: u16,
    pub grid_min_width: u16,
    pub font: FontChoice,
    pub font_custom: String,
    pub border_radius: u8,
    pub icon_size: u8,
    pub reduce_motion: bool,
    pub high_contrast: bool,
    /// The OS preference `reduce_motion` was last adopted from; `None` until
    /// the OS first asked for a different value than the current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reduce_motion_system_default: Option<bool>,
    /// The OS preference `high_contrast` was last adopted from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high_contrast_system_default: Option<bool>,
}

/// The operating system's accessibility display preferences.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SystemAccessibility {
    pub reduce_motion: bool,
    pub high_contrast: bool,
}

impl AppearanceSettings {
    /// Follows the OS for accessibility preferences the user has not set in
    /// explorie. A value still at its default, or still equal to the value last
    /// adopted from the OS, tracks the OS; any other value is the user's choice
    /// and is kept.
    pub fn adopt_system_accessibility(&mut self, system: SystemAccessibility) {
        adopt_system_preference(
            &mut self.reduce_motion,
            &mut self.reduce_motion_system_default,
            system.reduce_motion,
        );
        adopt_system_preference(
            &mut self.high_contrast,
            &mut self.high_contrast_system_default,
            system.high_contrast,
        );
    }
}

fn adopt_system_preference(value: &mut bool, adopted: &mut Option<bool>, system: bool) {
    let follows_system = adopted.map_or(!*value, |adopted| *value == adopted);
    if follows_system && *value != system {
        *value = system;
        *adopted = Some(system);
    }
}

/// Reads the OS accessibility preferences. Unit tests always see the defaults
/// so their results do not depend on the machine's settings.
pub(crate) fn system_accessibility() -> SystemAccessibility {
    if cfg!(test) {
        return SystemAccessibility::default();
    }
    query_system_accessibility()
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn query_system_accessibility() -> SystemAccessibility {
    use objc::runtime::{BOOL, NO, Object};
    use objc::{class, msg_send, sel, sel_impl};

    // SAFETY: NSWorkspace's shared instance and these read-only properties
    // (macOS 10.10+ / 10.12+) are safe to query from the main thread.
    unsafe {
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return SystemAccessibility::default();
        }
        let reduce_motion: BOOL = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
        let high_contrast: BOOL = msg_send![workspace, accessibilityDisplayShouldIncreaseContrast];
        SystemAccessibility {
            reduce_motion: reduce_motion != NO,
            high_contrast: high_contrast != NO,
        }
    }
}

#[cfg(windows)]
fn query_system_accessibility() -> SystemAccessibility {
    use windows_sys::Win32::Foundation::BOOL;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SystemParametersInfoW,
    };

    /// `HIGHCONTRASTW`, declared here to avoid another windows-sys feature.
    #[repr(C)]
    struct HighContrast {
        size: u32,
        flags: u32,
        default_scheme: *mut u16,
    }
    const HCF_HIGHCONTRASTON: u32 = 0x1;

    let mut animations: BOOL = 1;
    // SAFETY: both calls write only into the correctly sized buffers passed.
    let animations_known = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut animations as *mut BOOL).cast(),
            0,
        )
    } != 0;
    let mut contrast = HighContrast {
        size: std::mem::size_of::<HighContrast>() as u32,
        flags: 0,
        default_scheme: std::ptr::null_mut(),
    };
    let contrast_known = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.size,
            (&mut contrast as *mut HighContrast).cast(),
            0,
        )
    } != 0;
    SystemAccessibility {
        reduce_motion: animations_known && animations == 0,
        high_contrast: contrast_known && contrast.flags & HCF_HIGHCONTRASTON != 0,
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn query_system_accessibility() -> SystemAccessibility {
    SystemAccessibility::default()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeSpec {
    pub theme: ThemeMode,
    pub accent: AccentColor,
    pub accent_custom: String,
    pub density: Density,
    pub ui_scale: f32,
    pub list_row_height: u16,
    pub grid_min_width: u16,
    pub font: FontChoice,
    #[serde(default)]
    pub font_custom: String,
    pub border_radius: u8,
    pub icon_size: u8,
    pub reduce_motion: bool,
}

impl ThemeSpec {
    pub fn from_appearance(appearance: &AppearanceSettings) -> Self {
        Self {
            theme: appearance.theme,
            accent: appearance.accent,
            accent_custom: appearance.accent_custom.clone(),
            density: appearance.density,
            ui_scale: appearance.ui_scale,
            list_row_height: appearance.list_row_height,
            grid_min_width: appearance.grid_min_width,
            font: appearance.font,
            font_custom: appearance.font_custom.clone(),
            border_radius: appearance.border_radius,
            icon_size: appearance.icon_size,
            reduce_motion: appearance.reduce_motion,
        }
    }

    pub fn apply_to(&self, appearance: &mut AppearanceSettings) {
        appearance.theme = self.theme;
        appearance.accent = self.accent;
        appearance.accent_custom.clone_from(&self.accent_custom);
        appearance.density = self.density;
        appearance.ui_scale = self.ui_scale;
        appearance.list_row_height = self.list_row_height;
        appearance.grid_min_width = self.grid_min_width;
        appearance.font = self.font;
        appearance.font_custom.clone_from(&self.font_custom);
        appearance.border_radius = self.border_radius;
        appearance.icon_size = self.icon_size;
        appearance.reduce_motion = self.reduce_motion;
    }

    fn validate(mut self) -> Result<Self, String> {
        self.ui_scale = self.ui_scale.clamp(0.9, 1.4);
        self.list_row_height = self.list_row_height.clamp(26, 52);
        self.grid_min_width = self.grid_min_width.clamp(120, 260);
        self.icon_size = self.icon_size.clamp(10, 24);
        if !matches!(self.border_radius, 0 | 4 | 8) {
            return Err("border radius must be 0, 4, or 8".to_string());
        }
        if !valid_hex_color(&self.accent_custom) {
            return Err("custom accent must use #RRGGBB".to_string());
        }
        if self.font_custom.len() > 1_024 || self.font_custom.chars().any(char::is_control) {
            return Err("custom font must contain at most 1024 printable characters".to_string());
        }
        Ok(self)
    }
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            accent: AccentColor::Blue,
            accent_custom: "#7cc7ff".to_string(),
            density: Density::Comfortable,
            ui_scale: 1.0,
            list_row_height: 34,
            grid_min_width: 140,
            font: FontChoice::Mono,
            font_custom: String::new(),
            border_radius: 0,
            icon_size: 14,
            reduce_motion: false,
            high_contrast: false,
            reduce_motion_system_default: None,
            high_contrast_system_default: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorSettings {
    pub preview_executable_scripts: bool,
    pub confirm_before_delete: bool,
    pub remote_drives_enabled: bool,
    pub enable_error_reporting: bool,
    pub undo_timeout_minutes: u32,
}

impl Default for BehaviorSettings {
    fn default() -> Self {
        Self {
            preview_executable_scripts: false,
            confirm_before_delete: true,
            remote_drives_enabled: false,
            enable_error_reporting: false,
            undo_timeout_minutes: 5,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyImportRecord {
    pub source: PathBuf,
    pub imported_keys: usize,
    pub invalid_keys: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowPlacementSettings {
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub x: Option<f32>,
    pub y: Option<f32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    version: u32,
    #[serde(default)]
    pub integrations_onboarding_complete: bool,
    pub view: ViewSettings,
    pub appearance: AppearanceSettings,
    pub behavior: BehaviorSettings,
    #[serde(default)]
    pub window_placement: WindowPlacementSettings,
    #[serde(default)]
    pub recent_commands: Vec<String>,
    #[serde(default)]
    pub remote_profiles: Vec<RemoteDriveProfile>,
    #[serde(default)]
    pub named_themes: BTreeMap<String, ThemeSpec>,
    #[serde(default)]
    pub shortcut_bindings: BTreeMap<String, String>,
    #[serde(default)]
    pub legacy_values: BTreeMap<String, String>,
    #[serde(default)]
    pub legacy_import: Option<LegacyImportRecord>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            integrations_onboarding_complete: false,
            view: ViewSettings::default(),
            appearance: AppearanceSettings::default(),
            behavior: BehaviorSettings::default(),
            window_placement: WindowPlacementSettings::default(),
            recent_commands: Vec::new(),
            remote_profiles: Vec::new(),
            named_themes: BTreeMap::new(),
            shortcut_bindings: BTreeMap::new(),
            legacy_values: BTreeMap::new(),
            legacy_import: None,
        }
    }
}

impl AppSettings {
    pub fn reset_preserving_legacy(&mut self) {
        let legacy_values = std::mem::take(&mut self.legacy_values);
        let legacy_import = self.legacy_import.take();
        let remote_profiles = std::mem::take(&mut self.remote_profiles);
        *self = Self {
            legacy_values,
            legacy_import,
            remote_profiles,
            ..Self::default()
        };
        self.appearance
            .adopt_system_accessibility(system_accessibility());
    }

    fn validate(mut self) -> Result<Self, String> {
        if self.version != SETTINGS_VERSION {
            return Err(format!(
                "unsupported settings version {}; expected {SETTINGS_VERSION}",
                self.version
            ));
        }
        self.appearance.ui_scale = self.appearance.ui_scale.clamp(0.9, 1.4);
        self.view.sidebar_width = if self.view.sidebar_width.is_finite() {
            self.view.sidebar_width.clamp(160.0, 480.0)
        } else {
            default_sidebar_width()
        };
        self.view.preview_panel_width = if self.view.preview_panel_width.is_finite() {
            self.view.preview_panel_width.clamp(280.0, 640.0)
        } else {
            default_preview_panel_width()
        };
        self.window_placement.width = self
            .window_placement
            .width
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(520.0, 10_000.0));
        self.window_placement.height = self
            .window_placement
            .height
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(360.0, 10_000.0));
        self.window_placement.x = self.window_placement.x.filter(|value| value.is_finite());
        self.window_placement.y = self.window_placement.y.filter(|value| value.is_finite());
        self.appearance.list_row_height = self.appearance.list_row_height.clamp(26, 52);
        self.appearance.grid_min_width = self.appearance.grid_min_width.clamp(120, 260);
        self.appearance.icon_size = self.appearance.icon_size.clamp(10, 24);
        if !matches!(self.appearance.border_radius, 0 | 4 | 8) {
            self.appearance.border_radius = 0;
        }
        self.behavior.undo_timeout_minutes = self.behavior.undo_timeout_minutes.clamp(1, 1_440);
        if !valid_hex_color(&self.appearance.accent_custom) {
            self.appearance.accent_custom = "#7cc7ff".to_string();
        }
        normalize_recent_commands(&mut self.recent_commands);
        validate_remote_profiles(&self.remote_profiles)?;
        self.named_themes = validate_theme_map(std::mem::take(&mut self.named_themes))?;
        for id in crate::shortcut::retain_compatible_overrides(&mut self.shortcut_bindings) {
            eprintln!("dropped the {id} shortcut override: its keys now belong to a default");
        }
        crate::shortcut::validate_shortcut_overrides(&self.shortcut_bindings)?;
        Ok(self)
    }

    fn import_legacy(path: &Path, values: BTreeMap<String, String>) -> Self {
        let mut settings = Self {
            legacy_values: values.clone(),
            ..Self::default()
        };
        let mut invalid_keys = Vec::new();

        import_enum(&values, "explorie:viewMode", &mut invalid_keys, |value| {
            settings.view.view_mode = match value {
                "list" => ViewMode::List,
                "grid" => ViewMode::Grid,
                "column" => ViewMode::Column,
                _ => return false,
            };
            true
        });
        import_bool(&values, "explorie:showHidden", &mut invalid_keys, |value| {
            settings.view.show_hidden = value
        });
        import_bool(
            &values,
            "explorie:showSystemFiles",
            &mut invalid_keys,
            |value| settings.view.show_system_files = value,
        );
        import_enum(&values, "explorie:filterMode", &mut invalid_keys, |value| {
            settings.view.filter_mode = match value {
                "all" => EntryFilter::All,
                "folders" => EntryFilter::Folders,
                "files" => EntryFilter::Files,
                _ => return false,
            };
            true
        });
        for (key, target) in [
            (
                "explorie:showFolderSizes",
                &mut settings.view.show_folder_sizes,
            ),
            (
                "explorie:showPreviewPanel",
                &mut settings.view.show_preview_panel,
            ),
            ("explorie:showStatusBar", &mut settings.view.show_status_bar),
        ] {
            import_bool(&values, key, &mut invalid_keys, |value| *target = value);
        }
        import_enum(&values, "explorie:sortKey", &mut invalid_keys, |value| {
            settings.view.sort_key = match value {
                "name" => SortKey::Name,
                "size" => SortKey::Size,
                "modified" => SortKey::Modified,
                custom => match SortKey::custom(custom.to_string()) {
                    Ok(key) => key,
                    Err(_) => return false,
                },
            };
            true
        });
        import_enum(&values, "explorie:sortDir", &mut invalid_keys, |value| {
            settings.view.sort_direction = match value {
                "asc" => SortDirection::Ascending,
                "desc" => SortDirection::Descending,
                _ => return false,
            };
            true
        });
        import_number(
            &values,
            "explorie:sidebarWidth",
            160.0,
            480.0,
            &mut invalid_keys,
            |value| settings.view.sidebar_width = value as f32,
        );
        import_number(
            &values,
            "explorie:previewPanelWidth",
            280.0,
            640.0,
            &mut invalid_keys,
            |value| settings.view.preview_panel_width = value as f32,
        );

        import_enum(&values, "explorie:theme", &mut invalid_keys, |value| {
            settings.appearance.theme = match value {
                "dark" => ThemeMode::Dark,
                "light" => ThemeMode::Light,
                "system" => ThemeMode::System,
                _ => return false,
            };
            true
        });
        import_enum(&values, "explorie:accent", &mut invalid_keys, |value| {
            settings.appearance.accent = match value {
                "blue" => AccentColor::Blue,
                "green" => AccentColor::Green,
                "purple" => AccentColor::Purple,
                "orange" => AccentColor::Orange,
                "pink" => AccentColor::Pink,
                "custom" => AccentColor::Custom,
                _ => return false,
            };
            true
        });
        if let Some(value) = values.get("explorie:accentCustom") {
            if valid_hex_color(value) {
                settings.appearance.accent_custom = value.clone();
            } else {
                invalid_keys.push("explorie:accentCustom".to_string());
            }
        }
        import_enum(&values, "explorie:density", &mut invalid_keys, |value| {
            settings.appearance.density = match value {
                "comfortable" => Density::Comfortable,
                "compact" => Density::Compact,
                _ => return false,
            };
            true
        });
        import_number(
            &values,
            "explorie:uiScale",
            0.9,
            1.4,
            &mut invalid_keys,
            |value| settings.appearance.ui_scale = value as f32,
        );
        import_number(
            &values,
            "explorie:listRowHeight",
            26.0,
            52.0,
            &mut invalid_keys,
            |value| settings.appearance.list_row_height = value.round() as u16,
        );
        import_number(
            &values,
            "explorie:gridMinWidth",
            120.0,
            260.0,
            &mut invalid_keys,
            |value| settings.appearance.grid_min_width = value.round() as u16,
        );
        import_enum(&values, "explorie:font", &mut invalid_keys, |value| {
            settings.appearance.font = match value {
                "mono" => FontChoice::Mono,
                "system" => FontChoice::System,
                "serif" => FontChoice::Serif,
                "custom" => FontChoice::Custom,
                _ => return false,
            };
            true
        });
        if let Some(value) = values.get("explorie:fontCustom") {
            settings.appearance.font_custom = value.clone();
        }
        if let Some(value) = values.get("explorie:borderRadius") {
            match value.parse::<u8>() {
                Ok(value @ (0 | 4 | 8)) => settings.appearance.border_radius = value,
                _ => invalid_keys.push("explorie:borderRadius".to_string()),
            }
        }
        import_number(
            &values,
            "explorie:iconSize",
            10.0,
            24.0,
            &mut invalid_keys,
            |value| settings.appearance.icon_size = value.round() as u8,
        );

        for (key, target) in [
            (
                "explorie:reduceMotion",
                &mut settings.appearance.reduce_motion,
            ),
            (
                "explorie:highContrast",
                &mut settings.appearance.high_contrast,
            ),
            (
                "explorie:previewExecutableScripts",
                &mut settings.behavior.preview_executable_scripts,
            ),
            (
                "explorie:confirmBeforeDelete",
                &mut settings.behavior.confirm_before_delete,
            ),
            (
                "explorie:remoteDrivesEnabled",
                &mut settings.behavior.remote_drives_enabled,
            ),
            (
                "explorie:enableErrorReporting",
                &mut settings.behavior.enable_error_reporting,
            ),
        ] {
            import_bool(&values, key, &mut invalid_keys, |value| *target = value);
        }
        if !values.contains_key("explorie:remoteDrivesEnabled") {
            settings.behavior.remote_drives_enabled = values
                .get("explorie:remoteDrives")
                .and_then(|value| serde_json::from_str::<Vec<serde_json::Value>>(value).ok())
                .is_some_and(|profiles| !profiles.is_empty());
        }
        if let Some(value) = values.get("explorie:remoteDrives") {
            match serde_json::from_str::<Vec<serde_json::Value>>(value) {
                Ok(profiles) => {
                    let mut invalid = false;
                    let mut seen = HashSet::new();
                    for value in profiles {
                        let profile = serde_json::from_value::<RemoteDriveProfile>(value);
                        if let Ok(profile) = profile
                            && validate_remote_drive_profile(&profile).is_ok()
                            && seen.insert(profile.id.clone())
                        {
                            settings.remote_profiles.push(profile);
                        } else {
                            invalid = true;
                        }
                    }
                    if invalid {
                        invalid_keys.push("explorie:remoteDrives".to_string());
                    }
                }
                Err(_) => invalid_keys.push("explorie:remoteDrives".to_string()),
            }
            // Remote profiles are fully represented by `remote_profiles`. Do not retain the
            // legacy payload because older builds could have placed credentials beside them.
            settings.legacy_values.remove("explorie:remoteDrives");
        }
        import_number(
            &values,
            "explorie:undoTimeoutMinutes",
            1.0,
            1_440.0,
            &mut invalid_keys,
            |value| settings.behavior.undo_timeout_minutes = value.round() as u32,
        );
        if let Some(value) = values.get("explorie:recentCommands") {
            match serde_json::from_str::<Vec<String>>(value) {
                Ok(commands) => {
                    settings.recent_commands = commands;
                    normalize_recent_commands(&mut settings.recent_commands);
                }
                Err(_) => invalid_keys.push("explorie:recentCommands".to_string()),
            }
        }
        if let Some(value) = values.get("explorie:themes") {
            match serde_json::from_str::<BTreeMap<String, ThemeSpec>>(value)
                .map_err(|error| error.to_string())
                .and_then(validate_theme_map)
            {
                Ok(themes) => settings.named_themes = themes,
                Err(_) => invalid_keys.push("explorie:themes".to_string()),
            }
        }

        invalid_keys.sort();
        invalid_keys.dedup();
        settings.legacy_import = Some(LegacyImportRecord {
            source: path.to_path_buf(),
            imported_keys: values.len().saturating_sub(invalid_keys.len()),
            invalid_keys,
        });
        settings
    }
}

pub fn validate_theme_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a theme name".to_string());
    }
    if name.eq_ignore_ascii_case("default") {
        return Err("Name “Default” is reserved".to_string());
    }
    if name.len() > 80 || name.chars().any(char::is_control) {
        return Err("Theme names must be 1–80 printable characters".to_string());
    }
    Ok(name.to_string())
}

pub fn validate_theme_map(
    themes: BTreeMap<String, ThemeSpec>,
) -> Result<BTreeMap<String, ThemeSpec>, String> {
    if themes.len() > 100 {
        return Err("theme count exceeds 100".to_string());
    }
    let mut normalized = BTreeMap::new();
    for (name, theme) in themes {
        let name = validate_theme_name(&name)?;
        if normalized.contains_key(&name) {
            return Err("theme names are not unique".to_string());
        }
        normalized.insert(name, theme.validate()?);
    }
    Ok(normalized)
}

fn validate_remote_profiles(profiles: &[RemoteDriveProfile]) -> Result<(), String> {
    if profiles.len() > 100 {
        return Err("remote drive profile count exceeds 100".to_string());
    }
    let mut ids = HashSet::with_capacity(profiles.len());
    let mut targets = HashSet::with_capacity(profiles.len());
    for profile in profiles {
        validate_remote_drive_profile(profile).map_err(|error| error.to_string())?;
        if !ids.insert(profile.id.clone()) {
            return Err("remote drive profile identifiers are not unique".to_string());
        }
        if !targets.insert(profile.mount_target.to_ascii_lowercase()) {
            return Err("remote drive mount targets are not unique".to_string());
        }
    }
    Ok(())
}

fn normalize_recent_commands(commands: &mut Vec<String>) {
    let mut normalized = Vec::with_capacity(commands.len().min(5));
    for command in commands.drain(..) {
        if command.len() <= 128 && !normalized.contains(&command) {
            normalized.push(command);
            if normalized.len() == 5 {
                break;
            }
        }
    }
    *commands = normalized;
}

pub(crate) fn load_window_placement(config_dir: &Path) -> Option<WindowPlacementSettings> {
    let bytes = fs::read(config_dir.join("settings-v1.json")).ok()?;
    let (settings, _) = decode_settings(&bytes).ok()?;
    let placement = settings.window_placement;
    (placement.width.is_some()
        || placement.height.is_some()
        || placement.x.is_some()
        || placement.y.is_some())
    .then_some(placement)
}

fn valid_hex_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value[1..]
            .chars()
            .all(|character| character.is_ascii_hexdigit())
}

fn default_sidebar_width() -> f32 {
    220.0
}

fn default_preview_panel_width() -> f32 {
    360.0
}

fn import_bool(
    values: &BTreeMap<String, String>,
    key: &str,
    invalid: &mut Vec<String>,
    apply: impl FnOnce(bool),
) {
    let Some(value) = values.get(key) else {
        return;
    };
    match value.as_str() {
        "true" => apply(true),
        "false" => apply(false),
        _ => invalid.push(key.to_string()),
    }
}

fn import_enum(
    values: &BTreeMap<String, String>,
    key: &str,
    invalid: &mut Vec<String>,
    apply: impl FnOnce(&str) -> bool,
) {
    if let Some(value) = values.get(key)
        && !apply(value)
    {
        invalid.push(key.to_string());
    }
}

fn import_number(
    values: &BTreeMap<String, String>,
    key: &str,
    min: f64,
    max: f64,
    invalid: &mut Vec<String>,
    apply: impl FnOnce(f64),
) {
    let Some(value) = values.get(key) else {
        return;
    };
    match value.parse::<f64>() {
        Ok(value) if value.is_finite() && (min..=max).contains(&value) => apply(value),
        _ => invalid.push(key.to_string()),
    }
}

enum StoreMessage {
    Save(Vec<u8>),
    Flush(mpsc::SyncSender<()>),
}

pub(crate) struct SettingsStore {
    sender: Option<mpsc::Sender<StoreMessage>>,
    worker: Option<JoinHandle<()>>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl SettingsStore {
    pub fn open(config_dir: &Path) -> (Self, AppSettings, Option<String>) {
        Self::open_with_legacy_profile(config_dir, find_legacy_profile())
    }

    fn open_with_legacy_profile(
        config_dir: &Path,
        legacy_profile: Option<PathBuf>,
    ) -> (Self, AppSettings, Option<String>) {
        let path = config_dir.join("settings-v1.json");
        let legacy_path = config_dir.join(LEGACY_EXPORT_FILE);
        let mut save_initial = false;
        let (mut settings, warning) = match fs::read(&path) {
            Ok(bytes) => {
                let source_version = settings_schema_version(&bytes).unwrap_or(1);
                match decode_settings(&bytes) {
                    Ok((settings, false)) => (settings, None),
                    Ok((settings, true)) => {
                        let backup = migration_copy_path(&path, source_version);
                        match fs::copy(&path, &backup) {
                            Ok(_) => {
                                save_initial = true;
                                (
                                    settings,
                                    Some(format!(
                                        "Settings were migrated to schema v{SETTINGS_VERSION}; the v{source_version} source was preserved at {}",
                                        backup.display()
                                    )),
                                )
                            }
                            Err(error) => (
                                settings,
                                Some(format!(
                                    "Settings are using the v{SETTINGS_VERSION} schema in memory, but the v{source_version} backup could not be created ({error}); the source file was left unchanged"
                                )),
                            ),
                        }
                    }
                    Err(error) => {
                        let backup = preserved_copy_path(&path);
                        let preservation = fs::copy(&path, &backup);
                        let warning = match &preservation {
                            Ok(_) => format!(
                                "Settings recovery used defaults; the invalid file was preserved at {}: {error}",
                                backup.display()
                            ),
                            Err(copy_error) => format!(
                                "Settings recovery used defaults; preserving the invalid file failed ({copy_error}): {error}"
                            ),
                        };
                        save_initial = preservation.is_ok();
                        (AppSettings::default(), Some(warning))
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match read_legacy(&legacy_path) {
                    Ok(Some(values)) => {
                        let settings = AppSettings::import_legacy(&legacy_path, values);
                        let import = settings
                            .legacy_import
                            .as_ref()
                            .expect("legacy import recorded");
                        let warning = Some(if import.invalid_keys.is_empty() {
                            format!(
                                "Imported {} legacy setting(s) from {}; the source was preserved",
                                import.imported_keys,
                                legacy_path.display()
                            )
                        } else {
                            format!(
                                "Imported {} legacy setting(s) from {}; {} invalid value(s) kept their defaults and the source was preserved",
                                import.imported_keys,
                                legacy_path.display(),
                                import.invalid_keys.len()
                            )
                        });
                        save_initial = true;
                        (settings, warning)
                    }
                    Ok(None) => {
                        save_initial = true;
                        let warning = legacy_profile.map(|profile| {
                        format!(
                            "A pre-GPUI WebView profile exists at {}, but no settings export was found. The profile was preserved; export settings from an installed pre-GPUI release or place a recovered export at {}, then relaunch Explorie",
                            profile.display(),
                            legacy_path.display()
                        )
                    });
                        (AppSettings::default(), warning)
                    }
                    Err(error) => {
                        save_initial = true;
                        (
                            AppSettings::default(),
                            Some(format!(
                                "Legacy settings import was unavailable; {} was preserved: {error}",
                                legacy_path.display()
                            )),
                        )
                    }
                }
            }
            Err(error) => (
                AppSettings::default(),
                Some(format!("Settings recovery unavailable: {error}")),
            ),
        };

        settings
            .appearance
            .adopt_system_accessibility(system_accessibility());

        let (sender, receiver) = mpsc::channel();
        let last_error = Arc::new(Mutex::new(None));
        let worker_error = Arc::clone(&last_error);
        let worker = thread::spawn(move || {
            while let Ok(message) = receiver.recv() {
                match message {
                    StoreMessage::Save(bytes) => {
                        let result = atomic_write(&path, &bytes).map_err(|error| error.to_string());
                        *worker_error
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = result.err();
                    }
                    StoreMessage::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        let store = Self {
            sender: Some(sender),
            worker: Some(worker),
            last_error,
        };
        if save_initial {
            let _ = store.save(&settings);
        }
        (store, settings, warning)
    }

    pub fn save(&self, settings: &AppSettings) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
        self.sender
            .as_ref()
            .ok_or_else(|| "settings writer is unavailable".to_string())?
            .send(StoreMessage::Save(bytes))
            .map_err(|_| "settings writer stopped unexpectedly".to_string())
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn flush(&self) {
        let (sender, receiver) = mpsc::sync_channel(0);
        if self
            .sender
            .as_ref()
            .is_some_and(|queue| queue.send(StoreMessage::Flush(sender)).is_ok())
        {
            let _ = receiver.recv();
        }
    }
}

fn decode_settings(bytes: &[u8]) -> Result<(AppSettings, bool), String> {
    let mut value =
        serde_json::from_slice::<serde_json::Value>(bytes).map_err(|error| error.to_string())?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "settings version is missing or invalid".to_string())?;
    let migrated = match version {
        1 | 2 => {
            value["version"] = serde_json::Value::from(SETTINGS_VERSION);
            true
        }
        3 => {
            #[cfg(target_os = "macos")]
            if value
                .pointer("/appearance/font")
                .and_then(serde_json::Value::as_str)
                == Some("system")
                && value
                    .pointer("/appearance/fontCustom")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(str::is_empty)
            {
                value["appearance"]["font"] = serde_json::Value::from("mono");
            }
            value["version"] = serde_json::Value::from(SETTINGS_VERSION);
            true
        }
        version if version == u64::from(SETTINGS_VERSION) => false,
        version => {
            return Err(format!(
                "unsupported settings version {version}; expected 1, 2, 3, or {SETTINGS_VERSION}"
            ));
        }
    };
    serde_json::from_value::<AppSettings>(value)
        .map_err(|error| error.to_string())
        .and_then(AppSettings::validate)
        .map(|settings| (settings, migrated))
}

fn settings_schema_version(bytes: &[u8]) -> Option<u32> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()?
        .get("version")?
        .as_u64()?
        .try_into()
        .ok()
}

fn migration_copy_path(path: &Path, version: u32) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    parent.join(format!("{stem}.pre-migration-v{version}.json"))
}

fn find_legacy_profile() -> Option<PathBuf> {
    let root = dirs::data_local_dir()?;
    [root.join("com.omershatz.explorie"), root.join("explorie")]
        .into_iter()
        .find(|path| path.join("EBWebView").is_dir())
}

impl Drop for SettingsStore {
    fn drop(&mut self) {
        self.flush();
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read_legacy(path: &Path) -> Result<Option<BTreeMap<String, String>>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn preserved_copy_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let mut candidate = parent.join(format!("{stem}.invalid.json"));
    let mut suffix = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{stem}.invalid-{suffix}.json"));
        suffix += 1;
    }
    candidate
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "settings path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy(),
        std::process::id(),
        counter
    ));
    let result = (|| {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        replace_file(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(temp: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temp, destination)?;
    File::open(destination.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "settings path has no parent")
    })?)?
    .sync_all()
}

#[cfg(windows)]
fn replace_file(temp: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let temp: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if unsafe {
        MoveFileExW(
            temp.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn accessibility_follows_the_os_until_the_user_chooses_otherwise() {
        let on = SystemAccessibility {
            reduce_motion: true,
            high_contrast: true,
        };
        let off = SystemAccessibility::default();

        // A fresh profile adopts the OS, and keeps tracking it both ways.
        let mut appearance = AppearanceSettings::default();
        appearance.adopt_system_accessibility(off);
        assert_eq!(appearance, AppearanceSettings::default());
        appearance.adopt_system_accessibility(on);
        assert!(appearance.reduce_motion && appearance.high_contrast);
        appearance.adopt_system_accessibility(off);
        assert!(!appearance.reduce_motion && !appearance.high_contrast);

        // Turning an OS-provided preference off in explorie is kept.
        let mut appearance = AppearanceSettings::default();
        appearance.adopt_system_accessibility(on);
        appearance.reduce_motion = false;
        appearance.adopt_system_accessibility(on);
        assert!(!appearance.reduce_motion);
        assert!(appearance.high_contrast);

        // Turning one on in explorie survives the OS having it off, including
        // settings saved before the OS preference was tracked.
        let mut appearance = AppearanceSettings {
            high_contrast: true,
            ..AppearanceSettings::default()
        };
        appearance.adopt_system_accessibility(off);
        assert!(appearance.high_contrast);
        let mut legacy: AppearanceSettings = serde_json::from_value({
            let mut value = serde_json::to_value(AppearanceSettings::default()).unwrap();
            value["reduceMotion"] = serde_json::Value::Bool(true);
            value
        })
        .unwrap();
        legacy.adopt_system_accessibility(off);
        assert!(legacy.reduce_motion);
    }

    #[test]
    fn adopted_accessibility_defaults_round_trip() {
        let mut appearance = AppearanceSettings::default();
        assert!(
            !serde_json::to_string(&appearance)
                .unwrap()
                .contains("SystemDefault")
        );
        appearance.adopt_system_accessibility(SystemAccessibility {
            reduce_motion: true,
            high_contrast: false,
        });
        let restored: AppearanceSettings =
            serde_json::from_str(&serde_json::to_string(&appearance).unwrap()).unwrap();
        assert_eq!(restored, appearance);
        assert_eq!(restored.reduce_motion_system_default, Some(true));
        assert_eq!(restored.high_contrast_system_default, None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_accessibility_preferences_are_readable() {
        // Values depend on the machine; this checks the AppKit calls resolve.
        let _ = query_system_accessibility();
    }

    fn fixture_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("explorie-settings-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        path
    }

    fn open_test(root: &Path) -> (SettingsStore, AppSettings, Option<String>) {
        SettingsStore::open_with_legacy_profile(root, None)
    }

    fn remote_profile() -> RemoteDriveProfile {
        RemoteDriveProfile {
            id: "672ce77a-b72d-4e16-a9e8-55e0ac5bc580".to_string(),
            name: "Archive".to_string(),
            remote: "cloud".to_string(),
            remote_path: "projects".to_string(),
            mount_target: if cfg!(windows) { "R:" } else { "Archive" }.to_string(),
        }
    }

    #[test]
    fn integration_onboarding_is_backward_compatible_and_persists() {
        let mut value = serde_json::to_value(AppSettings::default()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("integrationsOnboardingComplete");
        let old: AppSettings = serde_json::from_value(value).unwrap();
        assert!(!old.integrations_onboarding_complete);
        let root = fixture_dir();
        let (store, mut settings, _) = open_test(&root);
        settings.integrations_onboarding_complete = true;
        store.save(&settings).unwrap();
        drop(store);
        let (store, restored, warning) = open_test(&root);
        assert!(warning.is_none());
        assert!(restored.integrations_onboarding_complete);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn versioned_settings_round_trip_and_clamp_bounds() {
        let root = fixture_dir();
        let (store, mut settings, warning) = open_test(&root);
        assert!(warning.is_none());
        settings.view.view_mode = ViewMode::Grid;
        settings.view.show_hidden = true;
        settings.view.sort_key = SortKey::custom("status").unwrap();
        settings.appearance.ui_scale = 1.3;
        settings.behavior.confirm_before_delete = false;
        settings.remote_profiles.push(remote_profile());
        store.save(&settings).unwrap();
        drop(store);

        let (store, settings, warning) = open_test(&root);
        assert!(warning.is_none());
        assert_eq!(settings.view.view_mode, ViewMode::Grid);
        assert!(settings.view.show_hidden);
        assert_eq!(
            settings.view.sort_key,
            SortKey::Custom("status".to_string())
        );
        assert_eq!(settings.appearance.ui_scale, 1.3);
        assert!(!settings.behavior.confirm_before_delete);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn older_native_settings_default_and_clamp_resizable_panel_widths() {
        let mut value = serde_json::to_value(AppSettings::default()).unwrap();
        value["view"]
            .as_object_mut()
            .unwrap()
            .remove("sidebarWidth");
        value["view"]
            .as_object_mut()
            .unwrap()
            .remove("previewPanelWidth");
        let settings = serde_json::from_value::<AppSettings>(value)
            .unwrap()
            .validate()
            .unwrap();
        assert_eq!(settings.view.sidebar_width, 220.0);
        assert_eq!(settings.view.preview_panel_width, 360.0);

        let mut settings = AppSettings::default();
        settings.view.sidebar_width = 9_999.0;
        settings.view.preview_panel_width = 9_999.0;
        let settings = settings.validate().unwrap();
        assert_eq!(settings.view.sidebar_width, 480.0);
        assert_eq!(settings.view.preview_panel_width, 640.0);
    }

    #[test]
    fn appearance_defaults_to_mono_on_every_desktop() {
        assert_eq!(AppearanceSettings::default().font, FontChoice::Mono);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_v2_mono_font_stays_mono() {
        let mut value = serde_json::to_value(AppSettings::default()).unwrap();
        value["version"] = serde_json::Value::from(2);
        value["appearance"]["font"] = serde_json::Value::from("mono");
        value["appearance"]["fontCustom"] = serde_json::Value::from("");

        let (settings, migrated) = decode_settings(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(migrated);
        assert_eq!(settings.appearance.font, FontChoice::Mono);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_v3_system_default_migrates_to_mono() {
        let mut value = serde_json::to_value(AppSettings::default()).unwrap();
        value["version"] = serde_json::Value::from(3);
        value["appearance"]["font"] = serde_json::Value::from("system");
        value["appearance"]["fontCustom"] = serde_json::Value::from("");

        let (settings, migrated) = decode_settings(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(migrated);
        assert_eq!(settings.appearance.font, FontChoice::Mono);
    }

    #[test]
    fn shortcut_overrides_are_backward_compatible_validated_and_persisted() {
        let mut older = serde_json::to_value(AppSettings::default()).unwrap();
        older.as_object_mut().unwrap().remove("shortcutBindings");
        let older = serde_json::from_value::<AppSettings>(older)
            .unwrap()
            .validate()
            .unwrap();
        assert!(older.shortcut_bindings.is_empty());

        let root = fixture_dir();
        let (store, mut settings, warning) = open_test(&root);
        assert!(warning.is_none());
        settings
            .shortcut_bindings
            .insert("settings-open".to_string(), "secondary-alt-k".to_string());
        store.save(&settings).unwrap();
        drop(store);
        let (store, restored, warning) = open_test(&root);
        assert!(warning.is_none());
        assert_eq!(
            restored
                .shortcut_bindings
                .get("settings-open")
                .map(String::as_str),
            Some("secondary-alt-k")
        );
        drop(store);

        // An override that collides with another command's default (for
        // example after a platform default changed) is dropped, not fatal.
        let mut shadowed = AppSettings::default();
        shadowed
            .shortcut_bindings
            .insert("settings-open".to_string(), "secondary-c".to_string());
        shadowed.shortcut_bindings.insert(
            "workspace-manager".to_string(),
            "secondary-alt-k".to_string(),
        );
        let shadowed = shadowed.validate().unwrap();
        assert_eq!(
            shadowed.shortcut_bindings,
            BTreeMap::from([(
                "workspace-manager".to_string(),
                "secondary-alt-k".to_string()
            )])
        );

        let mut conflict = AppSettings::default();
        conflict
            .shortcut_bindings
            .insert("settings-open".to_string(), "secondary-alt-k".to_string());
        conflict.shortcut_bindings.insert(
            "workspace-manager".to_string(),
            "secondary-alt-k".to_string(),
        );
        assert!(conflict.validate().unwrap_err().contains("conflicts"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_export_imports_valid_values_and_preserves_every_raw_key() {
        let root = fixture_dir();
        let legacy = BTreeMap::from([
            ("explorie:viewMode".to_string(), "column".to_string()),
            ("explorie:showHidden".to_string(), "true".to_string()),
            ("explorie:filterMode".to_string(), "files".to_string()),
            ("explorie:sortKey".to_string(), "status".to_string()),
            ("explorie:sortDir".to_string(), "desc".to_string()),
            ("explorie:theme".to_string(), "light".to_string()),
            ("explorie:uiScale".to_string(), "1.25".to_string()),
            ("explorie:sidebarWidth".to_string(), "276".to_string()),
            (
                "explorie:confirmBeforeDelete".to_string(),
                "false".to_string(),
            ),
            (
                "explorie:workspaces".to_string(),
                "{\"one\":{}}".to_string(),
            ),
        ]);
        fs::write(
            root.join(LEGACY_EXPORT_FILE),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();

        let (store, settings, warning) = open_test(&root);
        assert!(
            warning
                .as_deref()
                .is_some_and(|warning| warning.contains("Imported 10"))
        );
        assert_eq!(settings.view.view_mode, ViewMode::Column);
        assert!(settings.view.show_hidden);
        assert_eq!(settings.view.filter_mode, EntryFilter::Files);
        assert_eq!(
            settings.view.sort_key,
            SortKey::Custom("status".to_string())
        );
        assert_eq!(settings.view.sort_direction, SortDirection::Descending);
        assert_eq!(settings.appearance.theme, ThemeMode::Light);
        assert_eq!(settings.appearance.ui_scale, 1.25);
        assert_eq!(settings.view.sidebar_width, 276.0);
        assert!(!settings.behavior.confirm_before_delete);
        assert_eq!(settings.legacy_values, legacy);
        assert!(root.join(LEGACY_EXPORT_FILE).is_file());
        drop(store);

        let (_, restored, warning) = open_test(&root);
        assert!(warning.is_none(), "legacy import repeated: {warning:?}");
        assert_eq!(restored.legacy_values, legacy);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_sources_are_reported_preserved_and_recoverable() {
        let root = fixture_dir();
        let legacy_path = root.join(LEGACY_EXPORT_FILE);
        fs::write(&legacy_path, b"{not-json").unwrap();
        let (store, settings, warning) = open_test(&root);
        assert_eq!(settings, AppSettings::default());
        assert!(
            warning
                .as_deref()
                .is_some_and(|warning| warning.contains("preserved"))
        );
        assert_eq!(fs::read(&legacy_path).unwrap(), b"{not-json");
        drop(store);

        let settings_path = root.join("settings-v1.json");
        fs::write(&settings_path, b"{still-not-json").unwrap();
        let (store, _, warning) = open_test(&root);
        let warning = warning.unwrap();
        assert!(warning.contains("invalid file was preserved"), "{warning}");
        assert_eq!(
            fs::read(root.join("settings-v1.invalid.json")).unwrap(),
            b"{still-not-json"
        );
        drop(store);
        assert!(serde_json::from_slice::<AppSettings>(&fs::read(settings_path).unwrap()).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_legacy_values_keep_defaults_but_remain_lossless() {
        let root = fixture_dir();
        let legacy = BTreeMap::from([
            ("explorie:viewMode".to_string(), "tiles".to_string()),
            ("explorie:uiScale".to_string(), "huge".to_string()),
            ("explorie:showHidden".to_string(), "sometimes".to_string()),
        ]);
        fs::write(
            root.join(LEGACY_EXPORT_FILE),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        let (store, settings, warning) = open_test(&root);
        assert_eq!(settings.view.view_mode, ViewMode::List);
        assert!(!settings.view.show_hidden);
        assert_eq!(settings.appearance.ui_scale, 1.0);
        assert_eq!(settings.legacy_values, legacy);
        assert_eq!(settings.legacy_import.unwrap().invalid_keys.len(), 3);
        assert!(
            warning
                .as_deref()
                .is_some_and(|warning| warning.contains("3 invalid"))
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reset_restores_defaults_without_discarding_migration_evidence() {
        let mut settings = AppSettings::default();
        settings.view.show_hidden = true;
        settings.appearance.theme = ThemeMode::Light;
        settings.behavior.confirm_before_delete = false;
        settings.legacy_values = BTreeMap::from([(
            "explorie:unknownFutureKey".to_string(),
            "preserve-me".to_string(),
        )]);
        settings.legacy_import = Some(LegacyImportRecord {
            source: PathBuf::from("legacy-local-storage.json"),
            imported_keys: 1,
            invalid_keys: Vec::new(),
        });
        let legacy_values = settings.legacy_values.clone();
        let legacy_import = settings.legacy_import.clone();
        let remote_profiles = settings.remote_profiles.clone();

        settings.reset_preserving_legacy();

        assert_eq!(settings.view, AppSettings::default().view);
        assert_eq!(settings.appearance, AppSettings::default().appearance);
        assert_eq!(settings.behavior, AppSettings::default().behavior);
        assert_eq!(settings.legacy_values, legacy_values);
        assert_eq!(settings.legacy_import, legacy_import);
        assert_eq!(settings.remote_profiles, remote_profiles);
    }

    #[test]
    fn legacy_remote_profiles_import_without_credentials_and_persist_natively() {
        let root = fixture_dir();
        let profile = remote_profile();
        let legacy_profiles = serde_json::json!([
            {
                "id": profile.id,
                "name": profile.name,
                "remote": profile.remote,
                "remotePath": profile.remote_path,
                "mountTarget": profile.mount_target,
                "password": "must-not-survive"
            },
            {"id": "invalid", "name": "", "remote": null}
        ]);
        let legacy = BTreeMap::from([(
            "explorie:remoteDrives".to_string(),
            legacy_profiles.to_string(),
        )]);
        fs::write(
            root.join(LEGACY_EXPORT_FILE),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();

        let (store, settings, warning) = open_test(&root);
        assert!(warning.is_some());
        assert_eq!(settings.remote_profiles, vec![remote_profile()]);
        assert!(settings.behavior.remote_drives_enabled);
        store.save(&settings).unwrap();
        store.flush();
        let saved = fs::read_to_string(root.join("settings-v1.json")).unwrap();
        assert!(!saved.contains("must-not-survive"));
        assert!(saved.contains("remoteProfiles"));
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recent_commands_import_deduplicates_and_stays_bounded() {
        let root = fixture_dir();
        let commands = serde_json::json!([
            "view-grid",
            "view-grid",
            "settings-open",
            "help-shortcuts",
            "help-diagnostics",
            "nav-back",
            "nav-forward"
        ]);
        let legacy =
            BTreeMap::from([("explorie:recentCommands".to_string(), commands.to_string())]);
        fs::write(
            root.join(LEGACY_EXPORT_FILE),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();

        let (store, settings, warning) = open_test(&root);
        assert!(warning.is_some());
        assert_eq!(
            settings.recent_commands,
            vec![
                "view-grid",
                "settings-open",
                "help-shortcuts",
                "help-diagnostics",
                "nav-back"
            ]
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_named_themes_import_atomically_and_round_trip() {
        let root = fixture_dir();
        let mut sunset = ThemeSpec::from_appearance(&AppearanceSettings::default());
        sunset.theme = ThemeMode::Light;
        sunset.accent = AccentColor::Custom;
        sunset.accent_custom = "#e05a33".to_string();
        sunset.font = FontChoice::System;
        let themes = BTreeMap::from([("Sunset".to_string(), sunset.clone())]);
        let legacy = BTreeMap::from([(
            "explorie:themes".to_string(),
            serde_json::to_string(&themes).unwrap(),
        )]);
        fs::write(
            root.join(LEGACY_EXPORT_FILE),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();

        let (store, settings, warning) = open_test(&root);
        assert!(warning.is_some());
        assert_eq!(settings.named_themes, themes);
        store.save(&settings).unwrap();
        drop(store);

        let (store, restored, warning) = open_test(&root);
        assert!(warning.is_none());
        assert_eq!(restored.named_themes, themes);
        drop(store);

        let invalid_root = fixture_dir();
        let invalid = serde_json::json!({
            "Valid": sunset,
            "Default": ThemeSpec::from_appearance(&AppearanceSettings::default())
        });
        let legacy = BTreeMap::from([("explorie:themes".to_string(), invalid.to_string())]);
        fs::write(
            invalid_root.join(LEGACY_EXPORT_FILE),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        let (store, settings, _) = open_test(&invalid_root);
        assert!(settings.named_themes.is_empty());
        assert_eq!(
            settings.legacy_import.unwrap().invalid_keys,
            vec!["explorie:themes"]
        );
        drop(store);

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(invalid_root).unwrap();
    }

    #[test]
    fn detected_legacy_profile_without_export_has_an_explicit_recovery_path() {
        let root = fixture_dir();
        let profile = root.join("legacy-profile");
        fs::create_dir(&profile).unwrap();
        let (store, settings, warning) =
            SettingsStore::open_with_legacy_profile(&root, Some(profile.clone()));
        assert_eq!(settings, AppSettings::default());
        let warning = warning.unwrap();
        assert!(
            warning.contains(&profile.display().to_string()),
            "{warning}"
        );
        assert!(warning.contains(LEGACY_EXPORT_FILE), "{warning}");
        assert!(warning.contains("installed pre-GPUI release"), "{warning}");
        assert!(!warning.contains("compatibility build"), "{warning}");
        assert!(profile.is_dir());
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
