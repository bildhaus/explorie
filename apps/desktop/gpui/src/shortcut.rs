use std::collections::{BTreeMap, BTreeSet};

use gpui::{KeyBinding, Keystroke};

use crate::app_menu::{
    HideApp, HideOtherApps, MENU_DISPLAY_CONTEXT, MenuCopy, MenuCut, MenuPaste, MenuSelectAll,
    MinimizeWindow, Quit, RunCommand,
};
use crate::*;

/// Which platform's keyboard conventions a keymap follows. The running build
/// always uses [`KeymapPlatform::CURRENT`]; the other variant exists so both
/// default keymaps can be validated from either host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeymapPlatform {
    /// Explorer conventions: Ctrl-based chords, Alt+arrows, F2, Delete.
    Windows,
    /// Finder conventions: Cmd-based chords, Cmd+[ ], Cmd+Backspace, Return.
    MacOs,
}

impl KeymapPlatform {
    pub(crate) const CURRENT: Self = if cfg!(target_os = "macos") {
        Self::MacOs
    } else {
        Self::Windows
    };
}

/// Key context for bindings that must only fire while the file browser itself
/// has focus. Bare keys (Return, `?`, Backspace…) and text-editing chords
/// (Cmd+Backspace, Cmd+Up…) would otherwise also fire inside a focused text
/// field or menu nested in the browser, stealing typing and Return from inline
/// prompts and the command palette.
pub(crate) const BROWSER_FOCUS_CONTEXT: &str = "browser && !NativeTextInput && !file-context-menu";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ShortcutDefinition {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) category: &'static str,
    /// The default for the platform this build targets.
    pub(crate) default_binding: &'static str,
    pub(crate) windows_binding: &'static str,
    pub(crate) macos_binding: &'static str,
}

impl ShortcutDefinition {
    pub(crate) const fn default_for(&self, platform: KeymapPlatform) -> &'static str {
        match platform {
            KeymapPlatform::Windows => self.windows_binding,
            KeymapPlatform::MacOs => self.macos_binding,
        }
    }
}

pub(crate) const EDITABLE_SHORTCUTS: &[ShortcutDefinition] = &[
    platform_definition(
        "nav-back",
        "Go back",
        "Navigation",
        "alt-left",
        "secondary-[",
    ),
    platform_definition(
        "nav-forward",
        "Go forward",
        "Navigation",
        "alt-right",
        "secondary-]",
    ),
    platform_definition(
        "nav-up",
        "Go up one directory",
        "Navigation",
        "alt-up",
        "secondary-up",
    ),
    platform_definition(
        "nav-go-to-folder",
        "Go to folder",
        "Navigation",
        "secondary-g",
        "secondary-shift-g",
    ),
    // Finder's Cmd+D duplicates, so macOS uses Finder's "Add to Sidebar" chord.
    platform_definition(
        "nav-toggle-favorite",
        "Toggle favorite",
        "Navigation",
        "secondary-d",
        "secondary-ctrl-t",
    ),
    definition(
        "search-focus",
        "Search filenames",
        "Navigation",
        "secondary-f",
    ),
    // Cmd+Shift+G is Finder's Go to Folder on macOS.
    platform_definition(
        "search-save-smart-folder",
        "Save smart folder",
        "Navigation",
        "secondary-shift-g",
        "secondary-alt-s",
    ),
    definition(
        "file-copy",
        "Copy selected items",
        "File operations",
        "secondary-c",
    ),
    definition(
        "file-cut",
        "Cut selected items",
        "File operations",
        "secondary-x",
    ),
    definition(
        "file-paste",
        "Paste items",
        "File operations",
        "secondary-v",
    ),
    platform_definition(
        "file-trash",
        "Move selected items to trash",
        "File operations",
        "delete",
        "secondary-backspace",
    ),
    platform_definition(
        "file-delete-permanently",
        "Delete permanently",
        "File operations",
        "shift-delete",
        "secondary-alt-backspace",
    ),
    definition(
        "file-new-folder",
        "New folder",
        "File operations",
        "secondary-shift-n",
    ),
    platform_definition(
        "file-rename",
        "Rename selected item",
        "File operations",
        "f2",
        "enter",
    ),
    definition("edit-undo", "Undo", "File operations", "secondary-z"),
    platform_definition(
        "edit-redo",
        "Redo",
        "File operations",
        "secondary-y",
        "secondary-shift-z",
    ),
    platform_definition("view-refresh", "Refresh", "View", "f5", "secondary-r"),
    // GPUI reports Cmd+Shift+. as Cmd+> on macOS.
    platform_definition(
        "view-toggle-hidden",
        "Toggle hidden files",
        "View",
        "secondary-h",
        "secondary->",
    ),
    // Finder orders its views Icons (Cmd+1), List (Cmd+2), Columns (Cmd+3).
    platform_definition(
        "view-list",
        "List view",
        "View",
        "secondary-1",
        "secondary-2",
    ),
    platform_definition(
        "view-grid",
        "Grid view",
        "View",
        "secondary-2",
        "secondary-1",
    ),
    definition("view-column", "Column view", "View", "secondary-3"),
    definition("window-new", "New window", "Tabs", "secondary-n"),
    definition("tab-new", "New tab", "Tabs", "secondary-t"),
    definition("tab-close", "Close tab or window", "Tabs", "secondary-w"),
    // Cmd+Tab never reaches macOS apps; Finder and Safari use Ctrl+Tab.
    platform_definition("tab-next", "Next tab", "Tabs", "secondary-tab", "ctrl-tab"),
    platform_definition(
        "tab-previous",
        "Previous tab",
        "Tabs",
        "secondary-shift-tab",
        "ctrl-shift-tab",
    ),
    definition("settings-open", "Open settings", "Settings", "secondary-,"),
    definition(
        "commands-open",
        "Open command palette",
        "Settings",
        "secondary-shift-p",
    ),
    definition(
        "workspace-manager",
        "Manage workspaces",
        "Settings",
        "secondary-shift-w",
    ),
    // Finder's Connect to Server.
    platform_definition(
        "remote-drives-manager",
        "Manage remote drives",
        "Settings",
        "secondary-shift-r",
        "secondary-k",
    ),
    // GPUI reports Shift+/ as `?` on macOS.
    platform_definition(
        "help-shortcuts",
        "Show keyboard shortcuts",
        "Help",
        "shift-/",
        "?",
    ),
    // Cmd+Option+D toggles Dock hiding system-wide on macOS.
    platform_definition(
        "help-diagnostics",
        "Show diagnostics",
        "Help",
        "secondary-alt-d",
        "secondary-alt-shift-d",
    ),
];

const fn definition(
    id: &'static str,
    label: &'static str,
    category: &'static str,
    binding: &'static str,
) -> ShortcutDefinition {
    platform_definition(id, label, category, binding, binding)
}

const fn platform_definition(
    id: &'static str,
    label: &'static str,
    category: &'static str,
    windows_binding: &'static str,
    macos_binding: &'static str,
) -> ShortcutDefinition {
    ShortcutDefinition {
        id,
        label,
        category,
        default_binding: if cfg!(target_os = "macos") {
            macos_binding
        } else {
            windows_binding
        },
        windows_binding,
        macos_binding,
    }
}

fn shortcut_definition(id: &str) -> Option<&'static ShortcutDefinition> {
    EDITABLE_SHORTCUTS.iter().find(|shortcut| shortcut.id == id)
}

fn effective_binding_for(
    overrides: &BTreeMap<String, String>,
    shortcut: &ShortcutDefinition,
    platform: KeymapPlatform,
) -> String {
    overrides
        .get(shortcut.id)
        .cloned()
        .unwrap_or_else(|| shortcut.default_for(platform).to_string())
}

pub(crate) fn binding_for(overrides: &BTreeMap<String, String>, id: &str) -> Option<String> {
    shortcut_definition(id)
        .map(|shortcut| effective_binding_for(overrides, shortcut, KeymapPlatform::CURRENT))
}

/// Non-editable bindings for commands that also appear in the command palette.
fn fixed_command_bindings(platform: KeymapPlatform) -> &'static [(&'static str, &'static str)] {
    match platform {
        KeymapPlatform::Windows => &[("file-open", "enter"), ("file-compress", "secondary-alt-a")],
        KeymapPlatform::MacOs => &[
            ("file-open", "secondary-o"),
            ("file-compress", "secondary-alt-a"),
            ("go-home", "secondary-shift-h"),
            ("go-desktop", "secondary-shift-d"),
            ("go-documents", "secondary-shift-o"),
        ],
    }
}

/// The binding shown next to a command: its editable shortcut, else a fixed one.
pub(crate) fn command_binding(overrides: &BTreeMap<String, String>, id: &str) -> Option<String> {
    binding_for(overrides, id).or_else(|| {
        fixed_command_bindings(KeymapPlatform::CURRENT)
            .iter()
            .find(|(command, _)| *command == id)
            .map(|(_, binding)| (*binding).to_string())
    })
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ParsedBinding<'a> {
    secondary: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
    function: bool,
    key: &'a str,
}

impl<'a> ParsedBinding<'a> {
    fn parse(binding: &'a str, platform: KeymapPlatform) -> Self {
        let mut parsed = Self::default();
        // A trailing `-` is the minus key itself (`secondary--`).
        let (modifiers, key) = match binding.strip_suffix("--") {
            Some(modifiers) => (modifiers, "-"),
            None => binding.rsplit_once('-').unwrap_or(("", binding)),
        };
        parsed.key = if binding == "-" { "-" } else { key };
        for modifier in modifiers.split('-').filter(|part| !part.is_empty()) {
            match modifier.to_ascii_lowercase().as_str() {
                "secondary" | "cmd" | "super" | "win" | "platform" => parsed.secondary = true,
                "ctrl" | "control" => match platform {
                    KeymapPlatform::MacOs => parsed.ctrl = true,
                    KeymapPlatform::Windows => parsed.secondary = true,
                },
                "alt" | "option" => parsed.alt = true,
                "shift" => parsed.shift = true,
                "fn" | "function" => parsed.function = true,
                _ => {}
            }
        }
        parsed
    }

    fn canonical(&self) -> String {
        let mut parts = Vec::with_capacity(6);
        for (enabled, name) in [
            (self.secondary, "secondary"),
            (self.ctrl, "ctrl"),
            (self.alt, "alt"),
            (self.shift, "shift"),
            (self.function, "fn"),
        ] {
            if enabled {
                parts.push(name);
            }
        }
        let key = self.key.to_ascii_lowercase();
        parts.push(&key);
        parts.join("-")
    }

    fn has_command_modifier(&self) -> bool {
        self.secondary || self.ctrl || self.alt
    }

    /// Chords that the platform's text fields use for editing or caret movement.
    fn edits_text(&self, platform: KeymapPlatform) -> bool {
        let key = self.key.to_ascii_lowercase();
        match platform {
            KeymapPlatform::MacOs => {
                (self.secondary || self.alt)
                    && !self.ctrl
                    && matches!(
                        key.as_str(),
                        "backspace" | "delete" | "left" | "right" | "up" | "down"
                    )
            }
            KeymapPlatform::Windows => {
                self.secondary
                    && !self.alt
                    && matches!(
                        key.as_str(),
                        "backspace" | "delete" | "left" | "right" | "home" | "end"
                    )
            }
        }
    }

    fn is_function_key(&self) -> bool {
        let key = self.key.to_ascii_lowercase();
        key.len() > 1
            && key.starts_with('f')
            && key[1..].chars().all(|character| character.is_ascii_digit())
    }
}

/// Bindings are stored as GPUI keystroke strings; this spells them the same way
/// the shortcut recorder does so equivalent spellings compare equal.
fn canonical_binding(binding: &str, platform: KeymapPlatform) -> String {
    ParsedBinding::parse(binding, platform).canonical()
}

/// A binding without Cmd/Ctrl/Alt (optionally with Shift) types or edits text,
/// so it must not be offered to AppKit as a menu key equivalent.
pub(crate) fn is_plain_key_binding(binding: &str) -> bool {
    let parsed = ParsedBinding::parse(binding, KeymapPlatform::CURRENT);
    !parsed.has_command_modifier() && !parsed.is_function_key()
}

/// Whether the paste shortcut is left to AppKit's Edit ▸ Paste key
/// equivalent instead of a GPUI key binding. macOS lets an app read files
/// another app copied without asking only when the paste arrives through the
/// Edit menu's `paste:` item (clicked or by its key equivalent); a GPUI
/// binding would consume the key before AppKit sees it. AppKit can only use
/// single chords with a modifier (or function keys) as key equivalents.
pub(crate) fn paste_shortcut_belongs_to_menu(binding: &str, platform: KeymapPlatform) -> bool {
    if platform != KeymapPlatform::MacOs || binding.trim().contains(' ') {
        return false;
    }
    let parsed = ParsedBinding::parse(binding, platform);
    parsed.has_command_modifier() || parsed.is_function_key()
}

/// Whether Cmd+V in a text field must also go through the Edit menu, because
/// the menu's Paste uses that key and routes it to the focused text field.
pub(crate) fn text_paste_belongs_to_menu(overrides: &BTreeMap<String, String>) -> bool {
    text_paste_belongs_to_menu_for(overrides, KeymapPlatform::CURRENT)
}

pub(crate) fn text_paste_belongs_to_menu_for(
    overrides: &BTreeMap<String, String>,
    platform: KeymapPlatform,
) -> bool {
    let paste = effective_binding_for(
        overrides,
        shortcut_definition("file-paste").expect("editable shortcut is defined"),
        platform,
    );
    paste_shortcut_belongs_to_menu(&paste, platform)
        && canonical_binding(&paste, platform) == canonical_binding("secondary-v", platform)
}

/// The key context a browser shortcut should use so it never competes with
/// typing or caret movement in a focused text field.
fn browser_binding_context(binding: &str, platform: KeymapPlatform) -> &'static str {
    let parsed = ParsedBinding::parse(binding, platform);
    if (!parsed.has_command_modifier() && !parsed.is_function_key()) || parsed.edits_text(platform)
    {
        BROWSER_FOCUS_CONTEXT
    } else {
        "browser"
    }
}

fn load_binding(binding: &str, action: Box<dyn gpui::Action>, context: &str) -> KeyBinding {
    let predicate = gpui::KeyBindingContextPredicate::parse(context)
        .expect("static key context parses")
        .into();
    KeyBinding::load(
        binding,
        action,
        Some(predicate),
        false,
        None,
        &gpui::DummyKeyboardMapper,
    )
    .expect("validated shortcut parses")
}

pub(crate) fn display_binding(binding: &str) -> String {
    display_binding_for(binding, KeymapPlatform::CURRENT)
}

pub(crate) fn display_binding_for(binding: &str, platform: KeymapPlatform) -> String {
    let parsed = ParsedBinding::parse(binding, platform);
    let macos = platform == KeymapPlatform::MacOs;
    let key = match parsed.key.to_ascii_lowercase().as_str() {
        "left" if macos => "←".to_string(),
        "left" => "Left".to_string(),
        "right" if macos => "→".to_string(),
        "right" => "Right".to_string(),
        "up" if macos => "↑".to_string(),
        "up" => "Up".to_string(),
        "down" if macos => "↓".to_string(),
        "down" => "Down".to_string(),
        "delete" if macos => "⌦".to_string(),
        "delete" => "Delete".to_string(),
        "backspace" if macos => "⌫".to_string(),
        "backspace" => "Backspace".to_string(),
        "tab" if macos => "⇥".to_string(),
        "tab" => "Tab".to_string(),
        "enter" if macos => "↩".to_string(),
        "enter" => "Enter".to_string(),
        "escape" if macos => "⎋".to_string(),
        "escape" => "Esc".to_string(),
        "space" => "Space".to_string(),
        key if key.chars().count() == 1 => key.to_uppercase(),
        key if parsed.is_function_key() => key.to_ascii_uppercase(),
        key => key.to_string(),
    };
    if macos {
        // Apple's canonical modifier order: Control, Option, Shift, Command.
        let mut display = String::new();
        for (enabled, symbol) in [
            (parsed.ctrl, "⌃"),
            (parsed.alt, "⌥"),
            (parsed.shift, "⇧"),
            (parsed.secondary, "⌘"),
            (parsed.function, "fn "),
        ] {
            if enabled {
                display.push_str(symbol);
            }
        }
        display.push_str(&key);
        display
    } else {
        let mut parts = Vec::with_capacity(5);
        for (enabled, name) in [
            (parsed.secondary, "Ctrl"),
            (parsed.alt, "Alt"),
            (parsed.shift, "Shift"),
            (parsed.function, "Fn"),
        ] {
            if enabled {
                parts.push(name.to_string());
            }
        }
        parts.push(key);
        parts.join(" + ")
    }
}

pub(crate) fn binding_from_keystroke(keystroke: &Keystroke) -> Option<String> {
    binding_from_keystroke_for(keystroke, KeymapPlatform::CURRENT)
}

pub(crate) fn binding_from_keystroke_for(
    keystroke: &Keystroke,
    platform: KeymapPlatform,
) -> Option<String> {
    if matches!(
        keystroke.key.as_str(),
        "control" | "shift" | "alt" | "platform" | "function"
    ) {
        return None;
    }
    let modifiers = keystroke.modifiers;
    let parsed = ParsedBinding {
        // macOS keeps Control distinct from Command so Finder chords such as
        // Ctrl+Tab and Ctrl+Cmd+T can be recorded.
        secondary: match platform {
            KeymapPlatform::MacOs => modifiers.platform,
            KeymapPlatform::Windows => modifiers.control || modifiers.platform,
        },
        ctrl: platform == KeymapPlatform::MacOs && modifiers.control,
        alt: modifiers.alt,
        shift: modifiers.shift,
        function: false,
        key: &keystroke.key,
    };
    Some(parsed.canonical())
}

pub(crate) fn validate_shortcut_overrides(
    overrides: &BTreeMap<String, String>,
) -> Result<(), String> {
    validate_shortcut_overrides_for(overrides, KeymapPlatform::CURRENT)
}

pub(crate) fn validate_shortcut_overrides_for(
    overrides: &BTreeMap<String, String>,
    platform: KeymapPlatform,
) -> Result<(), String> {
    let known: BTreeSet<_> = EDITABLE_SHORTCUTS.iter().map(|item| item.id).collect();
    for (id, binding) in overrides {
        if !known.contains(id.as_str()) {
            return Err(format!("unknown shortcut command: {id}"));
        }
        if binding.len() > 64 || binding.chars().any(char::is_control) {
            return Err(format!(
                "shortcut for {id} is not a bounded printable value"
            ));
        }
        Keystroke::parse(binding).map_err(|_| format!("shortcut for {id} is invalid"))?;
    }

    let mut used = BTreeMap::<String, String>::new();
    for (binding, label) in fixed_browser_bindings_for(platform) {
        used.insert(canonical_binding(binding, platform), label.to_string());
    }
    for shortcut in EDITABLE_SHORTCUTS {
        let binding = canonical_binding(
            &effective_binding_for(overrides, shortcut, platform),
            platform,
        );
        if matches!(binding.as_str(), "space" | "escape") {
            return Err(format!(
                "{} is reserved for native preview/dismiss behavior",
                display_binding_for(&binding, platform)
            ));
        }
        if let Some(existing) = used.insert(binding.clone(), shortcut.label.to_string()) {
            return Err(format!(
                "{} conflicts with {existing}",
                display_binding_for(&binding, platform)
            ));
        }
    }
    Ok(())
}

/// Drops persisted overrides that collide with a default or fixed binding of
/// the running platform, so settings saved before a default changed still load
/// with every other override intact. Returns the ids that were dropped.
pub(crate) fn retain_compatible_overrides(overrides: &mut BTreeMap<String, String>) -> Vec<String> {
    retain_compatible_overrides_for(overrides, KeymapPlatform::CURRENT)
}

pub(crate) fn retain_compatible_overrides_for(
    overrides: &mut BTreeMap<String, String>,
    platform: KeymapPlatform,
) -> Vec<String> {
    let mut dropped = Vec::new();
    loop {
        // Dropping an override restores that command's default, which can in
        // turn collide with another override.
        let mut claimed: BTreeSet<String> = fixed_browser_bindings_for(platform)
            .iter()
            .map(|(binding, _)| canonical_binding(binding, platform))
            .collect();
        claimed.extend(
            EDITABLE_SHORTCUTS
                .iter()
                .filter(|shortcut| !overrides.contains_key(shortcut.id))
                .map(|shortcut| canonical_binding(shortcut.default_for(platform), platform)),
        );
        let Some(id) = overrides
            .iter()
            .find(|(_, binding)| claimed.contains(&canonical_binding(binding, platform)))
            .map(|(id, _)| id.clone())
        else {
            return dropped;
        };
        overrides.remove(&id);
        dropped.push(id);
    }
}

pub(crate) fn fixed_browser_bindings() -> &'static [(&'static str, &'static str)] {
    fixed_browser_bindings_for(KeymapPlatform::CURRENT)
}

pub(crate) fn fixed_browser_bindings_for(
    platform: KeymapPlatform,
) -> &'static [(&'static str, &'static str)] {
    match platform {
        KeymapPlatform::Windows => &[
            ("down", "file navigation"),
            ("up", "file navigation"),
            ("shift-down", "range selection"),
            ("shift-up", "range selection"),
            ("secondary-a", "select all"),
            ("enter", "open selected item"),
            ("space", "preview selected item"),
            ("secondary-space", "retry preview"),
            ("secondary-shift-space", "close preview"),
            ("escape", "dismiss or clear selection"),
            ("left", "column/grid navigation"),
            ("right", "column/grid navigation"),
            ("secondary-shift-left", "move tab left"),
            ("secondary-shift-right", "move tab right"),
            ("secondary-shift-f", "cycle file filter"),
            ("secondary-shift-s", "toggle folder sizes"),
            ("secondary-+", "increase UI scale"),
            ("secondary--", "decrease UI scale"),
            ("secondary-0", "reset UI scale"),
            ("secondary-alt-shift-p", "clear preview cache"),
            ("secondary-alt-p", "refresh preview helpers"),
            ("secondary-alt-a", "create archive"),
            ("secondary-alt-e", "extract archive"),
            ("secondary-alt-i", "inspect archive"),
            ("secondary-alt-shift-i", "close archive inspection"),
            ("secondary-alt-f", "cycle archive format"),
            ("secondary-alt-c", "cycle archive compression"),
            ("secondary-escape", "cancel operation"),
            ("secondary-shift-backspace", "clear completed operations"),
            ("secondary-shift-c", "cycle conflict policy"),
            ("secondary-alt-n", "new note"),
            ("secondary-alt-l", "new website link"),
            ("secondary-alt-r", "retry operation"),
        ],
        KeymapPlatform::MacOs => &[
            ("down", "file navigation"),
            ("up", "file navigation"),
            ("shift-down", "range selection"),
            ("shift-up", "range selection"),
            ("secondary-a", "select all"),
            ("secondary-o", "open selected item"),
            ("secondary-down", "open selected item"),
            ("secondary-shift-h", "go to home folder"),
            ("secondary-shift-d", "go to desktop"),
            ("secondary-shift-o", "go to documents"),
            ("space", "preview selected item"),
            ("secondary-space", "retry preview"),
            ("secondary-shift-space", "close preview"),
            ("escape", "dismiss or clear selection"),
            ("left", "column/grid navigation"),
            ("right", "column/grid navigation"),
            ("secondary-shift-left", "move tab left"),
            ("secondary-shift-right", "move tab right"),
            ("secondary-shift-f", "cycle file filter"),
            ("secondary-shift-s", "toggle folder sizes"),
            ("secondary-+", "increase UI scale"),
            ("secondary--", "decrease UI scale"),
            ("secondary-0", "reset UI scale"),
            ("secondary-alt-shift-p", "clear preview cache"),
            ("secondary-alt-p", "refresh preview helpers"),
            ("secondary-alt-a", "create archive"),
            ("secondary-alt-e", "extract archive"),
            ("secondary-alt-i", "inspect archive"),
            ("secondary-alt-shift-i", "close archive inspection"),
            ("secondary-alt-f", "cycle archive format"),
            ("secondary-alt-c", "cycle archive compression"),
            ("secondary-escape", "cancel operation"),
            ("secondary-shift-backspace", "clear completed operations"),
            ("secondary-shift-c", "cycle conflict policy"),
            ("secondary-alt-n", "new note"),
            ("secondary-alt-l", "new website link"),
            ("secondary-alt-r", "retry operation"),
            // Owned by the app menu or macOS itself; never rebindable.
            ("secondary-q", "quit explorie"),
            ("secondary-h", "hide explorie"),
            ("secondary-alt-h", "hide other apps"),
            ("secondary-m", "minimize window"),
            ("secondary-`", "cycle windows"),
            ("secondary-tab", "switch apps"),
        ],
    }
}

/// Plain Backspace navigates up in Explorer; in Finder it does nothing.
pub(crate) const BACKSPACE_GOES_UP: bool =
    matches!(KeymapPlatform::CURRENT, KeymapPlatform::Windows);

pub fn application_key_bindings(overrides: &BTreeMap<String, String>) -> Vec<KeyBinding> {
    application_key_bindings_for(overrides, KeymapPlatform::CURRENT)
}

pub(crate) fn application_key_bindings_for(
    overrides: &BTreeMap<String, String>,
    platform: KeymapPlatform,
) -> Vec<KeyBinding> {
    let editable = |id: &str, action: Box<dyn gpui::Action>| {
        let shortcut = shortcut_definition(id).expect("editable shortcut is defined");
        let binding = effective_binding_for(overrides, shortcut, platform);
        load_binding(
            &binding,
            action,
            browser_binding_context(&binding, platform),
        )
    };
    let text_safe = |binding: &str, action: Box<dyn gpui::Action>| {
        load_binding(binding, action, browser_binding_context(binding, platform))
    };
    let mut bindings = vec![
        editable("nav-back", Box::new(GoBack)),
        editable("nav-forward", Box::new(GoForward)),
        editable("nav-up", Box::new(GoUp)),
        editable("nav-go-to-folder", Box::new(GoToFolder)),
        KeyBinding::new("alt-up", MoveFavoriteUp, Some("favorite")),
        KeyBinding::new("alt-down", MoveFavoriteDown, Some("favorite")),
        editable("view-refresh", Box::new(Refresh)),
        editable("view-toggle-hidden", Box::new(ToggleHidden)),
        KeyBinding::new("secondary-shift-f", CycleFilter, Some("browser")),
        KeyBinding::new("down", SelectNext, Some("browser")),
        KeyBinding::new("up", SelectPrevious, Some("browser")),
        KeyBinding::new("shift-down", SelectNextRange, Some("browser")),
        KeyBinding::new("shift-up", SelectPreviousRange, Some("browser")),
        KeyBinding::new("secondary-a", SelectAll, Some("browser")),
        KeyBinding::new("secondary-space", RetryPreview, Some("browser")),
        KeyBinding::new("secondary-shift-space", ClosePreview, Some("browser")),
        KeyBinding::new("secondary-alt-shift-p", ClearPreviewCache, Some("browser")),
        KeyBinding::new("secondary-alt-p", RefreshPreviewHelpers, Some("browser")),
        KeyBinding::new("escape", ClearSelection, Some("browser")),
        editable("view-list", Box::new(ShowListView)),
        editable("view-grid", Box::new(ShowGridView)),
        editable("view-column", Box::new(ShowColumnView)),
        KeyBinding::new("left", ColumnLeft, Some("browser")),
        KeyBinding::new("right", ColumnRight, Some("browser")),
        editable("search-focus", Box::new(FocusSearch)),
        editable("search-save-smart-folder", Box::new(SaveSearch)),
        KeyBinding::new("secondary-shift-s", ToggleFolderSizes, Some("browser")),
        KeyBinding::new("secondary-+", IncreaseUiScale, None),
        KeyBinding::new("secondary--", DecreaseUiScale, None),
        KeyBinding::new("secondary-0", ResetUiScale, None),
        editable("window-new", Box::new(NewWindow)),
        editable("tab-new", Box::new(NewTab)),
        editable("tab-close", Box::new(CloseTab)),
        editable("tab-next", Box::new(NextTab)),
        editable("tab-previous", Box::new(PreviousTab)),
        text_safe("secondary-shift-left", Box::new(MoveTabLeft)),
        text_safe("secondary-shift-right", Box::new(MoveTabRight)),
        editable("nav-toggle-favorite", Box::new(ToggleFavorite)),
        editable("file-copy", Box::new(CopySelected)),
        editable("file-cut", Box::new(CutSelected)),
        editable("file-trash", Box::new(TrashSelected)),
        editable("file-delete-permanently", Box::new(PermanentDeleteSelected)),
        KeyBinding::new("secondary-alt-a", CreateArchive, Some("browser")),
        KeyBinding::new("secondary-alt-e", ExtractArchive, Some("browser")),
        KeyBinding::new("secondary-alt-i", InspectArchive, Some("browser")),
        KeyBinding::new(
            "secondary-alt-shift-i",
            CloseArchiveInspection,
            Some("browser"),
        ),
        KeyBinding::new("secondary-alt-f", CycleArchiveFormat, Some("browser")),
        KeyBinding::new("secondary-alt-c", CycleArchiveCompression, Some("browser")),
        KeyBinding::new("secondary-escape", CancelOperation, Some("browser")),
        text_safe(
            "secondary-shift-backspace",
            Box::new(ClearCompletedOperations),
        ),
        KeyBinding::new("secondary-shift-c", CycleConflictPolicy, Some("browser")),
        editable("file-new-folder", Box::new(NewFolder)),
        KeyBinding::new("secondary-alt-n", NewNote, Some("browser")),
        editable("file-rename", Box::new(RenameSelected)),
        KeyBinding::new("secondary-alt-l", NewWebsiteLink, Some("browser")),
        KeyBinding::new("secondary-alt-r", RetryOperation, Some("browser")),
        editable("edit-undo", Box::new(Undo)),
        editable("edit-redo", Box::new(Redo)),
        editable("settings-open", Box::new(ToggleSettingsPanel)),
        editable("commands-open", Box::new(OpenCommandPalette)),
        editable("workspace-manager", Box::new(ToggleWorkspaceManager)),
        editable("remote-drives-manager", Box::new(ToggleRemoteDriveManager)),
        editable("help-shortcuts", Box::new(ToggleShortcutsOverlay)),
        editable("help-diagnostics", Box::new(ToggleDiagnostics)),
    ];
    let paste = effective_binding_for(
        overrides,
        shortcut_definition("file-paste").expect("editable shortcut is defined"),
        platform,
    );
    if !paste_shortcut_belongs_to_menu(&paste, platform) {
        bindings.push(editable("file-paste", Box::new(Paste)));
    }
    match platform {
        KeymapPlatform::Windows => {
            bindings.push(KeyBinding::new(
                "enter",
                OpenSelected,
                Some(BROWSER_FOCUS_CONTEXT),
            ));
        }
        KeymapPlatform::MacOs => {
            // Finder opens with Cmd+O / Cmd+Down; Return renames.
            bindings.push(KeyBinding::new(
                "secondary-o",
                OpenSelected,
                Some("browser"),
            ));
            bindings.push(text_safe("secondary-down", Box::new(OpenSelected)));
            for (binding, command) in [
                ("secondary-shift-h", CommandId::GoHome),
                ("secondary-shift-d", CommandId::GoDesktop),
                ("secondary-shift-o", CommandId::GoDocuments),
            ] {
                bindings.push(KeyBinding::new(
                    binding,
                    RunCommand(command),
                    Some("browser"),
                ));
            }
            // Standard application menu chords. macOS apps provide these
            // themselves, so they work from any focus, like the menu bar.
            bindings.extend([
                KeyBinding::new("secondary-q", Quit, None),
                KeyBinding::new("secondary-h", HideApp, None),
                KeyBinding::new("secondary-alt-h", HideOtherApps, None),
                KeyBinding::new("secondary-m", MinimizeWindow, None),
            ]);
        }
    }
    // Display-only: the Edit menu shows the file shortcuts for its text-aware
    // actions; keyboard input keeps using the actions bound above, except
    // for a macOS paste shortcut left to the menu (see
    // `paste_shortcut_belongs_to_menu`), which reaches it as the key
    // equivalent of Edit ▸ Paste.
    let display_only = |binding: &str, action: Box<dyn gpui::Action>| {
        load_binding(binding, action, MENU_DISPLAY_CONTEXT)
    };
    let effective = |id: &str| {
        effective_binding_for(
            overrides,
            shortcut_definition(id).expect("editable shortcut is defined"),
            platform,
        )
    };
    bindings.extend([
        display_only(&effective("file-cut"), Box::new(MenuCut)),
        display_only(&effective("file-copy"), Box::new(MenuCopy)),
        display_only(&effective("file-paste"), Box::new(MenuPaste)),
        display_only("secondary-a", Box::new(MenuSelectAll)),
    ]);
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLATFORMS: [KeymapPlatform; 2] = [KeymapPlatform::Windows, KeymapPlatform::MacOs];

    #[test]
    fn shortcut_validation_refuses_conflicts_reserved_keys_and_unknown_commands() {
        assert!(validate_shortcut_overrides(&BTreeMap::new()).is_ok());
        assert!(
            validate_shortcut_overrides(&BTreeMap::from([(
                "nav-back".to_string(),
                "space".to_string()
            )]))
            .unwrap_err()
            .contains("reserved")
        );
        assert!(
            validate_shortcut_overrides(&BTreeMap::from([(
                "nav-back".to_string(),
                "secondary-c".to_string()
            )]))
            .unwrap_err()
            .contains("conflicts")
        );
        assert!(
            validate_shortcut_overrides(&BTreeMap::from([(
                "unknown".to_string(),
                "secondary-k".to_string()
            )]))
            .unwrap_err()
            .contains("unknown")
        );
    }

    #[test]
    fn both_default_keymaps_are_conflict_free() {
        for platform in PLATFORMS {
            assert_eq!(
                validate_shortcut_overrides_for(&BTreeMap::new(), platform),
                Ok(()),
                "{platform:?}"
            );
            for shortcut in EDITABLE_SHORTCUTS {
                assert!(
                    Keystroke::parse(shortcut.default_for(platform)).is_ok(),
                    "{} on {platform:?}",
                    shortcut.id
                );
            }
        }
    }

    #[test]
    fn windows_defaults_keep_explorer_conventions() {
        let defaults = |id| {
            shortcut_definition(id)
                .unwrap()
                .default_for(KeymapPlatform::Windows)
        };
        assert_eq!(defaults("nav-back"), "alt-left");
        assert_eq!(defaults("nav-forward"), "alt-right");
        assert_eq!(defaults("nav-up"), "alt-up");
        assert_eq!(defaults("nav-go-to-folder"), "secondary-g");
        assert_eq!(defaults("file-trash"), "delete");
        assert_eq!(defaults("file-rename"), "f2");
        assert_eq!(defaults("edit-redo"), "secondary-y");
        assert_eq!(defaults("view-toggle-hidden"), "secondary-h");
        assert!(
            fixed_browser_bindings_for(KeymapPlatform::Windows)
                .contains(&("enter", "open selected item"))
        );
    }

    #[test]
    fn macos_defaults_follow_finder_and_leave_system_chords_alone() {
        let defaults = |id| {
            shortcut_definition(id)
                .unwrap()
                .default_for(KeymapPlatform::MacOs)
        };
        assert_eq!(defaults("nav-back"), "secondary-[");
        assert_eq!(defaults("nav-forward"), "secondary-]");
        assert_eq!(defaults("nav-up"), "secondary-up");
        assert_eq!(defaults("nav-go-to-folder"), "secondary-shift-g");
        assert_eq!(defaults("view-toggle-hidden"), "secondary->");
        assert_eq!(defaults("file-trash"), "secondary-backspace");
        assert_eq!(defaults("file-rename"), "enter");
        assert_eq!(defaults("edit-redo"), "secondary-shift-z");
        assert_eq!(defaults("file-new-folder"), "secondary-shift-n");
        assert_ne!(defaults("search-save-smart-folder"), "secondary-shift-g");

        let system = [
            "secondary-h",
            "secondary-q",
            "secondary-m",
            "secondary-`",
            "secondary-tab",
        ];
        for shortcut in EDITABLE_SHORTCUTS {
            let binding = canonical_binding(shortcut.macos_binding, KeymapPlatform::MacOs);
            assert!(!system.contains(&binding.as_str()), "{}", shortcut.id);
            assert_ne!(binding, "backspace", "{}", shortcut.id);
        }
        for binding in system {
            let overrides = BTreeMap::from([("nav-back".to_string(), binding.to_string())]);
            assert!(
                validate_shortcut_overrides_for(&overrides, KeymapPlatform::MacOs)
                    .unwrap_err()
                    .contains("conflicts"),
                "{binding}"
            );
        }
        let fixed = fixed_browser_bindings_for(KeymapPlatform::MacOs);
        assert!(!fixed.iter().any(|(binding, _)| *binding == "backspace"));
        assert!(!fixed.iter().any(|(binding, _)| *binding == "enter"));
        assert!(fixed.contains(&("secondary-o", "open selected item")));
        assert!(fixed.contains(&("secondary-down", "open selected item")));
    }

    #[test]
    fn overrides_stay_keyed_by_id_and_conflicts_use_platform_defaults() {
        // Cmd+Shift+G is free on Windows (smart folders own it there) only when
        // the smart-folder shortcut moves; on macOS it already belongs to Go to Folder.
        let overrides = BTreeMap::from([(
            "search-save-smart-folder".to_string(),
            "secondary-shift-g".to_string(),
        )]);
        assert!(validate_shortcut_overrides_for(&overrides, KeymapPlatform::Windows).is_ok());
        assert!(
            validate_shortcut_overrides_for(&overrides, KeymapPlatform::MacOs)
                .unwrap_err()
                .contains("Go to folder")
        );
        // Equivalent spellings of the same chord conflict.
        let overrides = BTreeMap::from([("nav-back".to_string(), "cmd-shift-n".to_string())]);
        assert!(
            validate_shortcut_overrides_for(&overrides, KeymapPlatform::MacOs)
                .unwrap_err()
                .contains("conflicts")
        );
        let overrides = BTreeMap::from([("nav-back".to_string(), "secondary-alt-j".to_string())]);
        for platform in PLATFORMS {
            let bindings = application_key_bindings_for(&overrides, platform);
            let keystroke = Keystroke::parse("secondary-alt-j").unwrap();
            assert!(bindings.iter().any(|binding| {
                binding.action().partial_eq(&GoBack)
                    && binding.match_keystrokes(std::slice::from_ref(&keystroke)) == Some(false)
            }));
        }
    }

    #[test]
    fn overrides_that_collide_with_new_platform_defaults_are_dropped_on_load() {
        // Saved on macOS before Cmd+[ became Go back and Cmd+R became Refresh.
        let mut overrides = BTreeMap::from([
            ("view-list".to_string(), "secondary-[".to_string()),
            ("settings-open".to_string(), "secondary-alt-k".to_string()),
            ("tab-new".to_string(), "secondary-r".to_string()),
            // Frees Go to folder's old Cmd+G default for nothing else.
            ("nav-go-to-folder".to_string(), "secondary-g".to_string()),
        ]);
        let dropped = retain_compatible_overrides_for(&mut overrides, KeymapPlatform::MacOs);
        assert_eq!(dropped, ["tab-new", "view-list"]);
        assert_eq!(
            overrides,
            BTreeMap::from([
                ("nav-go-to-folder".to_string(), "secondary-g".to_string()),
                ("settings-open".to_string(), "secondary-alt-k".to_string()),
            ])
        );
        assert!(validate_shortcut_overrides_for(&overrides, KeymapPlatform::MacOs).is_ok());

        // Dropping one override can restore a default another override uses.
        let mut overrides = BTreeMap::from([
            ("view-list".to_string(), "secondary-r".to_string()),
            ("tab-new".to_string(), "secondary-2".to_string()),
            ("settings-open".to_string(), "secondary-alt-k".to_string()),
        ]);
        let dropped = retain_compatible_overrides_for(&mut overrides, KeymapPlatform::MacOs);
        assert_eq!(dropped, ["view-list", "tab-new"]);
        assert_eq!(overrides.len(), 1);
        assert!(validate_shortcut_overrides_for(&overrides, KeymapPlatform::MacOs).is_ok());

        // Windows overrides that were valid stay untouched.
        let mut overrides = BTreeMap::from([("nav-back".to_string(), "backspace".to_string())]);
        assert!(
            retain_compatible_overrides_for(&mut overrides, KeymapPlatform::Windows).is_empty()
        );
    }

    #[test]
    fn plain_key_shortcuts_never_fire_inside_text_fields_or_menus() {
        for platform in PLATFORMS {
            for binding in application_key_bindings_for(&BTreeMap::new(), platform) {
                let keystrokes = binding.keystrokes();
                let Some(keystroke) = keystrokes.first().map(|keystroke| keystroke.inner()) else {
                    continue;
                };
                let text_key = !keystroke.modifiers.control
                    && !keystroke.modifiers.platform
                    && !keystroke.modifiers.alt
                    && (keystroke.key.chars().count() == 1
                        || matches!(keystroke.key.as_str(), "enter" | "backspace"));
                if text_key {
                    assert_eq!(
                        binding.predicate().map(|predicate| predicate.to_string()),
                        Some(
                            gpui::KeyBindingContextPredicate::parse(BROWSER_FOCUS_CONTEXT)
                                .unwrap()
                                .to_string()
                        ),
                        "{keystroke} on {platform:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn text_editing_chords_never_fire_inside_text_fields() {
        let gated = gpui::KeyBindingContextPredicate::parse(BROWSER_FOCUS_CONTEXT)
            .unwrap()
            .to_string();
        let context_of = |platform, keys: &str, action: &dyn gpui::Action| {
            let keystroke = Keystroke::parse(keys).unwrap();
            application_key_bindings_for(&BTreeMap::new(), platform)
                .into_iter()
                .find(|binding| {
                    binding.action().partial_eq(action)
                        && binding.match_keystrokes(std::slice::from_ref(&keystroke)) == Some(false)
                })
                .and_then(|binding| binding.predicate())
                .map(|predicate| predicate.to_string())
        };
        let macos = KeymapPlatform::MacOs;
        assert_eq!(
            context_of(macos, "secondary-backspace", &TrashSelected),
            Some(gated.clone())
        );
        assert_eq!(
            context_of(macos, "secondary-up", &GoUp),
            Some(gated.clone())
        );
        assert_eq!(
            context_of(macos, "secondary-down", &OpenSelected),
            Some(gated.clone())
        );
        assert_eq!(
            context_of(macos, "secondary-o", &OpenSelected).as_deref(),
            Some("browser")
        );
        assert_eq!(
            context_of(KeymapPlatform::Windows, "alt-left", &GoBack).as_deref(),
            Some("browser")
        );
        assert_eq!(
            context_of(
                KeymapPlatform::Windows,
                "secondary-shift-left",
                &MoveTabLeft
            ),
            Some(gated)
        );
    }

    #[test]
    fn binding_display_and_capture_are_canonical() {
        let key = Keystroke::parse("ctrl-alt-b").unwrap();
        assert_eq!(
            binding_from_keystroke_for(&key, KeymapPlatform::Windows).as_deref(),
            Some("secondary-alt-b")
        );
        assert_eq!(
            binding_from_keystroke_for(&key, KeymapPlatform::MacOs).as_deref(),
            Some("ctrl-alt-b")
        );
        let tab = Keystroke {
            modifiers: gpui::Modifiers {
                control: true,
                ..Default::default()
            },
            key: "tab".to_string(),
            key_char: None,
        };
        assert_eq!(
            binding_from_keystroke_for(&tab, KeymapPlatform::MacOs).as_deref(),
            Some("ctrl-tab")
        );
        assert_eq!(
            display_binding_for("secondary-alt-b", KeymapPlatform::MacOs),
            "⌥⌘B"
        );
        assert_eq!(
            display_binding_for("secondary-shift-g", KeymapPlatform::MacOs),
            "⇧⌘G"
        );
        assert_eq!(
            display_binding_for("secondary-ctrl-t", KeymapPlatform::MacOs),
            "⌃⌘T"
        );
        assert_eq!(
            display_binding_for("secondary-backspace", KeymapPlatform::MacOs),
            "⌘⌫"
        );
        assert_eq!(
            display_binding_for("secondary--", KeymapPlatform::MacOs),
            "⌘-"
        );
        assert_eq!(
            display_binding_for("secondary-alt-b", KeymapPlatform::Windows),
            "Ctrl + Alt + B"
        );
        assert_eq!(display_binding_for("f2", KeymapPlatform::Windows), "F2");
        assert_eq!(
            canonical_binding("alt-shift-cmd-k", KeymapPlatform::MacOs),
            "secondary-alt-shift-k"
        );
    }

    #[test]
    fn plain_space_is_owned_only_by_the_native_preview_handler() {
        let space = Keystroke::parse("space").unwrap();
        for platform in PLATFORMS {
            assert!(
                application_key_bindings_for(&BTreeMap::new(), platform)
                    .iter()
                    .all(
                        |binding| binding.match_keystrokes(std::slice::from_ref(&space))
                            != Some(false)
                    )
            );
            assert!(
                fixed_browser_bindings_for(platform).contains(&("space", "preview selected item"))
            );
        }
    }

    #[test]
    fn macos_leaves_the_paste_shortcut_to_the_edit_menu() {
        let binds_paste = |overrides: &BTreeMap<String, String>, platform| {
            application_key_bindings_for(overrides, platform)
                .iter()
                .any(|binding| binding.action().partial_eq(&Paste))
        };
        let menu_shows_paste = |overrides: &BTreeMap<String, String>, platform| {
            application_key_bindings_for(overrides, platform)
                .iter()
                .any(|binding| binding.action().partial_eq(&MenuPaste))
        };
        let defaults = BTreeMap::new();
        // macOS only treats a paste through Edit > Paste as user initiated,
        // so Cmd+V must reach AppKit as the menu's key equivalent.
        assert!(!binds_paste(&defaults, KeymapPlatform::MacOs));
        assert!(menu_shows_paste(&defaults, KeymapPlatform::MacOs));
        assert!(text_paste_belongs_to_menu_for(
            &defaults,
            KeymapPlatform::MacOs
        ));
        // Windows keeps binding Ctrl+V itself.
        assert!(binds_paste(&defaults, KeymapPlatform::Windows));
        assert!(!text_paste_belongs_to_menu_for(
            &defaults,
            KeymapPlatform::Windows
        ));

        // A rebound chord is still a key equivalent; text fields keep Cmd+V.
        let chord = BTreeMap::from([("file-paste".to_string(), "secondary-shift-v".to_string())]);
        assert!(!binds_paste(&chord, KeymapPlatform::MacOs));
        assert!(!text_paste_belongs_to_menu_for(
            &chord,
            KeymapPlatform::MacOs
        ));
        // AppKit cannot use a bare key or a key sequence as a key equivalent.
        for binding in ["p", "secondary-k v"] {
            let overrides = BTreeMap::from([("file-paste".to_string(), binding.to_string())]);
            assert!(binds_paste(&overrides, KeymapPlatform::MacOs), "{binding}");
            assert!(
                !text_paste_belongs_to_menu_for(&overrides, KeymapPlatform::MacOs),
                "{binding}"
            );
        }
        assert!(paste_shortcut_belongs_to_menu("f5", KeymapPlatform::MacOs));
        assert!(!paste_shortcut_belongs_to_menu(
            "secondary-v",
            KeymapPlatform::Windows
        ));
    }

    #[test]
    fn ui_scale_shortcuts_are_global_and_reserved_from_rebinding() {
        let bindings = application_key_bindings(&BTreeMap::new());
        let modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        for key in ["+", "-", "0"] {
            let key = Keystroke::parse(&format!("{modifier}-{key}")).unwrap();
            assert!(bindings.iter().any(|binding| {
                binding.match_keystrokes(std::slice::from_ref(&key)) == Some(false)
            }));
        }
        assert!(fixed_browser_bindings().contains(&("secondary-+", "increase UI scale")));
        assert!(fixed_browser_bindings().contains(&("secondary--", "decrease UI scale")));
        assert!(fixed_browser_bindings().contains(&("secondary-0", "reset UI scale")));
    }
}
