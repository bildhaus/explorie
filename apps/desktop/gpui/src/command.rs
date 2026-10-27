use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandId {
    GoBack,
    GoForward,
    GoUp,
    GoToFolder,
    ClearHistory,
    Refresh,
    ListView,
    GridView,
    ColumnView,
    ToggleHidden,
    TogglePreview,
    ToggleStatus,
    NewWindow,
    MoveTabToNewWindow,
    NewTab,
    CloseTab,
    NewFolder,
    Rename,
    Copy,
    Cut,
    Paste,
    Trash,
    Undo,
    Redo,
    OpenSettings,
    ManageWorkspaces,
    SaveWorkspace,
    ManageRemoteDrives,
    ThemeDark,
    ThemeLight,
    ThemeSystem,
    ToggleFavorite,
    SaveSmartFolder,
    ShowShortcuts,
    ShowDiagnostics,
    OpenSelected,
    Compress,
    DeletePermanently,
    Find,
    GoHome,
    GoDesktop,
    GoDocuments,
    GoDownloads,
    ToggleSidebar,
    NextTab,
    PreviousTab,
    About,
}

impl CommandId {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::GoBack => "nav-back",
            Self::GoForward => "nav-forward",
            Self::GoUp => "nav-up",
            Self::GoToFolder => "nav-go-to-folder",
            Self::ClearHistory => "nav-clear-history",
            Self::Refresh => "view-refresh",
            Self::ListView => "view-list",
            Self::GridView => "view-grid",
            Self::ColumnView => "view-column",
            Self::ToggleHidden => "view-toggle-hidden",
            Self::TogglePreview => "view-toggle-preview",
            Self::ToggleStatus => "view-toggle-status",
            Self::NewWindow => "window-new",
            Self::MoveTabToNewWindow => "window-move-tab",
            Self::NewTab => "tab-new",
            Self::CloseTab => "tab-close",
            Self::NewFolder => "file-new-folder",
            Self::Rename => "file-rename",
            Self::Copy => "file-copy",
            Self::Cut => "file-cut",
            Self::Paste => "file-paste",
            Self::Trash => "file-trash",
            Self::Undo => "edit-undo",
            Self::Redo => "edit-redo",
            Self::OpenSettings => "settings-open",
            Self::ManageWorkspaces => "workspace-manager",
            Self::SaveWorkspace => "workspace-save",
            Self::ManageRemoteDrives => "remote-drives-manager",
            Self::ThemeDark => "settings-theme-dark",
            Self::ThemeLight => "settings-theme-light",
            Self::ThemeSystem => "settings-theme-system",
            Self::ToggleFavorite => "nav-toggle-favorite",
            Self::SaveSmartFolder => "search-save-smart-folder",
            Self::ShowShortcuts => "help-shortcuts",
            Self::ShowDiagnostics => "help-diagnostics",
            Self::OpenSelected => "file-open",
            Self::Compress => "file-compress",
            Self::DeletePermanently => "file-delete-permanently",
            Self::Find => "search-focus",
            Self::GoHome => "go-home",
            Self::GoDesktop => "go-desktop",
            Self::GoDocuments => "go-documents",
            Self::GoDownloads => "go-downloads",
            Self::ToggleSidebar => "view-toggle-sidebar",
            Self::NextTab => "tab-next",
            Self::PreviousTab => "tab-previous",
            Self::About => "help-about",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        all_commands(CommandContext::default())
            .into_iter()
            .find(|command| command.id.as_str() == value)
            .map(|command| command.id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandCategory {
    Navigation,
    File,
    View,
    Tabs,
    Settings,
    Help,
}

impl CommandCategory {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::File => "File operations",
            Self::View => "View",
            Self::Tabs => "Tabs",
            Self::Settings => "Settings",
            Self::Help => "Help",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandSpec {
    pub(crate) id: CommandId,
    pub(crate) name: String,
    pub(crate) shortcut: Option<String>,
    pub(crate) category: CommandCategory,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CommandContext {
    pub(crate) show_hidden: bool,
    pub(crate) show_preview: bool,
    pub(crate) show_status: bool,
    pub(crate) sidebar_collapsed: bool,
    pub(crate) favorite: bool,
}

pub(crate) fn all_commands(context: CommandContext) -> Vec<CommandSpec> {
    use CommandCategory::{File, Help, Navigation, Settings, Tabs, View};
    use CommandId::*;
    vec![
        command(GoBack, "Go back", Navigation),
        command(GoForward, "Go forward", Navigation),
        command(GoUp, "Go up one directory", Navigation),
        command(GoToFolder, "Go to folder…", Navigation),
        command(GoHome, "Go to Home folder", Navigation),
        command(GoDesktop, "Go to Desktop", Navigation),
        command(GoDocuments, "Go to Documents", Navigation),
        command(GoDownloads, "Go to Downloads", Navigation),
        command(Find, "Search filenames", Navigation),
        command(ClearHistory, "Clear navigation history", Navigation),
        command(
            ToggleFavorite,
            if context.favorite {
                "Remove current folder from favorites"
            } else {
                "Add current folder to favorites"
            },
            Navigation,
        ),
        command(
            SaveSmartFolder,
            "Save current search as smart folder",
            Navigation,
        ),
        command(NewFolder, "New folder", File),
        command(OpenSelected, "Open selected item", File),
        command(Rename, "Rename selected item", File),
        command(Compress, "Compress selected items", File),
        command(Copy, "Copy selected items", File),
        command(Cut, "Cut selected items", File),
        command(Paste, "Paste items", File),
        command(Trash, "Move selected items to trash", File),
        command(DeletePermanently, "Delete selected items permanently", File),
        command(Undo, "Undo", File),
        command(Redo, "Redo", File),
        command(NewWindow, "Open current folder in new window", Tabs),
        command(MoveTabToNewWindow, "Move current tab to new window", Tabs),
        command(Refresh, "Refresh", View),
        command(ListView, "Switch to list view", View),
        command(GridView, "Switch to grid view", View),
        command(ColumnView, "Switch to column view", View),
        command(
            ToggleHidden,
            if context.show_hidden {
                "Hide hidden files"
            } else {
                "Show hidden files"
            },
            View,
        ),
        command(
            TogglePreview,
            if context.show_preview {
                "Unpin preview panel"
            } else {
                "Pin preview panel"
            },
            View,
        ),
        command(
            ToggleStatus,
            if context.show_status {
                "Hide status bar"
            } else {
                "Show status bar"
            },
            View,
        ),
        command(
            ToggleSidebar,
            if context.sidebar_collapsed {
                "Show sidebar"
            } else {
                "Hide sidebar"
            },
            View,
        ),
        command(NewTab, "New tab", Tabs),
        command(CloseTab, "Close current tab", Tabs),
        command(NextTab, "Next tab", Tabs),
        command(PreviousTab, "Previous tab", Tabs),
        command(OpenSettings, "Open settings", Settings),
        command(ManageWorkspaces, "Manage workspaces", Settings),
        command(SaveWorkspace, "Save current workspace", Settings),
        command(ManageRemoteDrives, "Manage remote drives", Settings),
        command(ThemeDark, "Switch to dark theme", Settings),
        command(ThemeLight, "Switch to light theme", Settings),
        command(ThemeSystem, "Use system theme", Settings),
        command(ShowShortcuts, "Show keyboard shortcuts", Help),
        command(ShowDiagnostics, "Show native diagnostics", Help),
        command(About, "About explorie", Help),
    ]
}

/// Commands with the effective (platform default or user-overridden) shortcut
/// of each one, formatted for display.
pub(crate) fn all_commands_with_shortcuts(
    context: CommandContext,
    overrides: &BTreeMap<String, String>,
) -> Vec<CommandSpec> {
    let mut commands = all_commands(context);
    for command in &mut commands {
        command.shortcut = crate::shortcut::command_binding(overrides, command.id.as_str())
            .map(|binding| crate::shortcut::display_binding(&binding));
    }
    commands
}

fn command(id: CommandId, name: &str, category: CommandCategory) -> CommandSpec {
    CommandSpec {
        id,
        name: name.to_string(),
        shortcut: None,
        category,
    }
}

pub(crate) fn filtered_commands(query: &str, commands: &[CommandSpec]) -> Vec<CommandSpec> {
    let query = query.trim();
    if query.is_empty() {
        return commands.to_vec();
    }
    let mut matches: Vec<_> = commands
        .iter()
        .filter_map(|command| {
            let name_score = fuzzy_score(query, &command.name);
            let category_score =
                fuzzy_score(query, command.category.label()).map(|score| score / 2);
            name_score
                .max(category_score)
                .map(|score| (score, command.clone()))
        })
        .collect();
    matches.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.name.cmp(&right.name))
    });
    matches.into_iter().map(|(_, command)| command).collect()
}

fn fuzzy_score(query: &str, target: &str) -> Option<u32> {
    let query = query.to_lowercase();
    let target = target.to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let mut query_chars = query.chars();
    let mut expected = query_chars.next()?;
    let mut score = 0;
    let mut consecutive = 0;
    for character in target.chars() {
        if character == expected {
            score += 1 + consecutive;
            consecutive += 1;
            if let Some(next) = query_chars.next() {
                expected = next;
            } else {
                if target.starts_with(&query) {
                    score += 10;
                }
                return Some(score);
            }
        } else {
            consecutive = 0;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_filter_matches_in_order_and_ranks_prefixes_first() {
        let commands = all_commands(CommandContext::default());
        let filtered = filtered_commands("theme", &commands);
        assert_eq!(filtered.len(), 3);
        assert!(
            filtered
                .iter()
                .all(|command| command.name.contains("theme"))
        );

        let filtered = filtered_commands("diag", &commands);
        assert_eq!(filtered[0].id, CommandId::ShowDiagnostics);
        assert!(filtered_commands("not-a-command", &commands).is_empty());
    }

    #[test]
    fn command_ids_round_trip_for_persistent_recents() {
        for command in all_commands(CommandContext::default()) {
            assert_eq!(CommandId::from_str(command.id.as_str()), Some(command.id));
        }
    }

    #[test]
    fn command_palette_advertises_effective_platform_bindings() {
        let shortcut = |overrides: &BTreeMap<String, String>, id| {
            all_commands_with_shortcuts(CommandContext::default(), overrides)
                .into_iter()
                .find(|command| command.id == id)
                .and_then(|command| command.shortcut)
        };
        let defaults = BTreeMap::new();
        assert_eq!(
            shortcut(&defaults, CommandId::OpenSettings).as_deref(),
            Some(if cfg!(target_os = "macos") {
                "⌘,"
            } else {
                "Ctrl + ,"
            })
        );
        assert_eq!(
            shortcut(&defaults, CommandId::GoBack).as_deref(),
            Some(if cfg!(target_os = "macos") {
                "⌘["
            } else {
                "Alt + Left"
            })
        );
        let overrides = BTreeMap::from([("nav-back".to_string(), "secondary-alt-j".to_string())]);
        assert_eq!(
            shortcut(&overrides, CommandId::GoBack),
            Some(crate::shortcut::display_binding("secondary-alt-j"))
        );
        assert_eq!(shortcut(&defaults, CommandId::ClearHistory), None);
        let commands = all_commands(CommandContext::default());
        assert!(
            commands
                .iter()
                .all(|command| command.name != "Go to folder")
        );
    }
}
