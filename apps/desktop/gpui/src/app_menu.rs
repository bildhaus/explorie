//! The native application menu bar and the app-level actions behind it.
//!
//! Menu items reuse the browser's keyboard actions wherever one exists, so a
//! menu click runs exactly what the shortcut runs and GPUI derives each item's
//! key equivalent from the live (possibly user-overridden) keymap. Commands
//! without a keyboard action go through [`RunCommand`], which reaches the
//! command palette's execution path for the active window.

use gpui::{Menu, MenuItem, OsAction, SystemMenuType, WindowHandle};

use crate::*;

/// Runs a command-palette command in the active window.
#[derive(Clone, Copy, Debug, PartialEq, gpui::Action)]
#[action(namespace = explorie, no_json)]
pub(crate) struct RunCommand(pub(crate) CommandId);

gpui::actions!(
    explorie,
    [
        /// Quits after the same pending-work and remote-drive checks as
        /// closing the last window.
        Quit,
        HideApp,
        HideOtherApps,
        ShowAllApps,
        MinimizeWindow,
        ZoomWindow,
        /// Edit menu items: act on a focused text field, otherwise on the
        /// selected files.
        MenuCut,
        MenuCopy,
        MenuPaste,
        MenuSelectAll,
    ]
);

/// Key context no element declares. Bindings in it never fire, but GPUI still
/// uses them as menu key equivalents for the text-aware Edit menu actions.
pub(crate) const MENU_DISPLAY_CONTEXT: &str = "explorie-menu-display";

/// The window state the menu bar reflects (labels, check marks, and key
/// equivalents). Menus are rebuilt only when this changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MenuState {
    pub(crate) shortcut_overrides: BTreeMap<String, String>,
    pub(crate) view_mode: Option<ViewMode>,
    pub(crate) show_hidden: bool,
    pub(crate) show_preview: bool,
    pub(crate) show_status: bool,
    pub(crate) show_sidebar: bool,
    pub(crate) favorite: bool,
    pub(crate) multiple_tabs: bool,
}

pub(crate) fn app_menus(state: &MenuState) -> Vec<Menu> {
    let item = |label: &str, command: CommandId, action: Box<dyn gpui::Action>| {
        command_item(label, command, action, None, state)
    };
    let view_item = |label: &str, mode: ViewMode, command: CommandId, action| {
        item(label, command, action).checked(state.view_mode == Some(mode))
    };
    let show_hide =
        |shown: bool, subject: &str| format!("{} {subject}", if shown { "Hide" } else { "Show" });
    vec![
        Menu::new(APP_NAME).items([
            MenuItem::action(format!("About {APP_NAME}"), RunCommand(CommandId::About)),
            MenuItem::separator(),
            MenuItem::action("Settings…", ToggleSettingsPanel),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(format!("Hide {APP_NAME}"), HideApp),
            MenuItem::action("Hide Others", HideOtherApps),
            MenuItem::action("Show All", ShowAllApps),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {APP_NAME}"), Quit),
        ]),
        Menu::new("File").items([
            item("New Window", CommandId::NewWindow, Box::new(NewWindow)),
            item("New Tab", CommandId::NewTab, Box::new(NewTab)),
            item("New Folder", CommandId::NewFolder, Box::new(NewFolder)),
            MenuItem::separator(),
            item("Open", CommandId::OpenSelected, Box::new(OpenSelected)),
            item(
                if state.multiple_tabs {
                    "Close Tab"
                } else {
                    "Close Window"
                },
                CommandId::CloseTab,
                Box::new(CloseTab),
            ),
            MenuItem::separator(),
            item("Rename…", CommandId::Rename, Box::new(RenameSelected)),
            item("Compress", CommandId::Compress, Box::new(CreateArchive)),
            MenuItem::separator(),
            item("Move to Trash", CommandId::Trash, Box::new(TrashSelected)),
            item(
                "Delete Immediately…",
                CommandId::DeletePermanently,
                Box::new(PermanentDeleteSelected),
            ),
            MenuItem::separator(),
            item(
                if state.favorite {
                    "Remove from Favorites"
                } else {
                    "Add to Favorites"
                },
                CommandId::ToggleFavorite,
                Box::new(ToggleFavorite),
            ),
            MenuItem::separator(),
            item("Find", CommandId::Find, Box::new(FocusSearch)),
            item(
                "Save Search as Smart Folder…",
                CommandId::SaveSmartFolder,
                Box::new(SaveSearch),
            ),
        ]),
        Menu::new("Edit").items([
            command_item(
                "Undo",
                CommandId::Undo,
                Box::new(Undo),
                Some(OsAction::Undo),
                state,
            ),
            command_item(
                "Redo",
                CommandId::Redo,
                Box::new(Redo),
                Some(OsAction::Redo),
                state,
            ),
            MenuItem::separator(),
            MenuItem::os_action("Cut", MenuCut, OsAction::Cut),
            MenuItem::os_action("Copy", MenuCopy, OsAction::Copy),
            MenuItem::os_action("Paste", MenuPaste, OsAction::Paste),
            MenuItem::os_action("Select All", MenuSelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            // Finder's order and shortcuts: Icons, List, Columns.
            view_item(
                "as Grid",
                ViewMode::Grid,
                CommandId::GridView,
                Box::new(ShowGridView),
            ),
            view_item(
                "as List",
                ViewMode::List,
                CommandId::ListView,
                Box::new(ShowListView),
            ),
            view_item(
                "as Columns",
                ViewMode::Column,
                CommandId::ColumnView,
                Box::new(ShowColumnView),
            ),
            MenuItem::separator(),
            item(
                &show_hide(state.show_hidden, "Hidden Files"),
                CommandId::ToggleHidden,
                Box::new(ToggleHidden),
            ),
            MenuItem::separator(),
            MenuItem::action(
                show_hide(state.show_sidebar, "Sidebar"),
                RunCommand(CommandId::ToggleSidebar),
            ),
            item(
                &show_hide(state.show_preview, "Preview"),
                CommandId::TogglePreview,
                Box::new(TogglePreviewPanel),
            ),
            item(
                &show_hide(state.show_status, "Status Bar"),
                CommandId::ToggleStatus,
                Box::new(ToggleStatusBar),
            ),
            MenuItem::separator(),
            item("Refresh", CommandId::Refresh, Box::new(Refresh)),
            MenuItem::separator(),
            MenuItem::action("Zoom In", IncreaseUiScale),
            MenuItem::action("Zoom Out", DecreaseUiScale),
            MenuItem::action("Actual Size", ResetUiScale),
        ]),
        Menu::new("Go").items([
            item("Back", CommandId::GoBack, Box::new(GoBack)),
            item("Forward", CommandId::GoForward, Box::new(GoForward)),
            item("Enclosing Folder", CommandId::GoUp, Box::new(GoUp)),
            MenuItem::separator(),
            MenuItem::action("Home", RunCommand(CommandId::GoHome)),
            MenuItem::action("Desktop", RunCommand(CommandId::GoDesktop)),
            MenuItem::action("Documents", RunCommand(CommandId::GoDocuments)),
            MenuItem::action("Downloads", RunCommand(CommandId::GoDownloads)),
            MenuItem::separator(),
            item("Go to Folder…", CommandId::GoToFolder, Box::new(GoToFolder)),
            item(
                "Remote Drives…",
                CommandId::ManageRemoteDrives,
                Box::new(ToggleRemoteDriveManager),
            ),
            MenuItem::separator(),
            MenuItem::action("Clear History", RunCommand(CommandId::ClearHistory)),
        ]),
        // GPUI registers the menu titled "Window" as the macOS Windows menu,
        // so AppKit appends the window list and Bring All to Front.
        Menu::new("Window").items([
            MenuItem::action("Minimize", MinimizeWindow),
            MenuItem::action("Zoom", ZoomWindow),
            MenuItem::separator(),
            item(
                "Show Previous Tab",
                CommandId::PreviousTab,
                Box::new(PreviousTab),
            ),
            item("Show Next Tab", CommandId::NextTab, Box::new(NextTab)),
            item(
                "Move Tab to New Window",
                CommandId::MoveTabToNewWindow,
                Box::new(MoveTabToNewWindow),
            ),
            MenuItem::separator(),
            item(
                "Workspaces…",
                CommandId::ManageWorkspaces,
                Box::new(ToggleWorkspaceManager),
            ),
        ]),
        Menu::new("Help").items([
            item(
                "Keyboard Shortcuts",
                CommandId::ShowShortcuts,
                Box::new(ToggleShortcutsOverlay),
            ),
            MenuItem::action("Command Palette", OpenCommandPalette),
            item(
                "Diagnostics",
                CommandId::ShowDiagnostics,
                Box::new(ToggleDiagnostics),
            ),
        ]),
    ]
}

/// A menu item for a command with a keyboard action. Bare-key shortcuts such as
/// Finder's Return-to-rename are routed through [`RunCommand`] instead, which
/// has no binding, so AppKit never treats a bare key as a menu key equivalent.
fn command_item(
    label: &str,
    command: CommandId,
    action: Box<dyn gpui::Action>,
    os_action: Option<OsAction>,
    state: &MenuState,
) -> MenuItem {
    let bare_key = crate::shortcut::command_binding(&state.shortcut_overrides, command.as_str())
        .is_some_and(|binding| crate::shortcut::is_plain_key_binding(&binding));
    MenuItem::Action {
        name: label.to_string().into(),
        action: if bare_key {
            Box::new(RunCommand(command))
        } else {
            action
        },
        os_action,
        checked: false,
        disabled: false,
    }
}

/// Opens a window when an app-level request (Quit) arrives with none open.
pub type OpenWindowFn = dyn Fn(&mut App) -> Option<WindowHandle<DirectoryWindow>>;

#[derive(Default)]
struct AppMenus {
    last_state: Option<MenuState>,
}

impl gpui::Global for AppMenus {}

/// Installs the menu bar, the app-level action handlers, and the observers that
/// keep menu labels and key equivalents in sync with the active window.
pub fn install_app_menus(
    cx: &mut App,
    services: NativeServices,
    open_window: impl Fn(&mut App) -> Option<WindowHandle<DirectoryWindow>> + 'static,
) {
    let open_window: Rc<OpenWindowFn> = Rc::new(open_window);
    cx.set_global(AppMenus::default());
    cx.on_action(|action: &RunCommand, cx| {
        let command = action.0;
        update_active_window(cx, move |view, _, cx| view.run_command(command, cx));
    });
    cx.on_action(|_: &MenuCut, cx| {
        route_edit_action(cx, Box::new(native_text_input::Cut), Box::new(CutSelected))
    });
    cx.on_action(|_: &MenuCopy, cx| {
        route_edit_action(
            cx,
            Box::new(native_text_input::Copy),
            Box::new(CopySelected),
        )
    });
    cx.on_action(|_: &MenuPaste, cx| {
        route_edit_action(cx, Box::new(native_text_input::Paste), Box::new(Paste))
    });
    cx.on_action(|_: &MenuSelectAll, cx| {
        route_edit_action(
            cx,
            Box::new(native_text_input::SelectAll),
            Box::new(SelectAll),
        )
    });
    cx.on_action(move |_: &Quit, cx| request_quit(&services, open_window.clone(), cx));
    cx.on_action(|_: &HideApp, cx| cx.hide());
    cx.on_action(|_: &HideOtherApps, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApps, cx| cx.unhide_other_apps());
    cx.on_action(|_: &MinimizeWindow, cx| {
        update_active_window(cx, |_, window, _| window.minimize_window())
    });
    cx.on_action(|_: &ZoomWindow, cx| {
        update_active_window(cx, |_, window, _| window.zoom_window())
    });
    cx.observe_new::<DirectoryWindow>(|view, window, cx| {
        let Some(window) = window else {
            return;
        };
        let handle = window.window_handle();
        cx.observe_window_activation(window, |view, window, cx| {
            if window.is_window_active() {
                refresh_app_menus(view.menu_state(), cx);
            }
        })
        .detach();
        cx.observe_self(move |view, cx| {
            if cx.active_window() == Some(handle) {
                refresh_app_menus(view.menu_state(), cx);
            }
        })
        .detach();
        refresh_app_menus(view.menu_state(), cx);
    })
    .detach();
}

/// Rebuilds the menu bar when the reflected state changed. GPUI reads key
/// equivalents from the keymap at this point, so callers run after
/// `install_shortcut_bindings` has applied the same overrides.
fn refresh_app_menus(state: MenuState, cx: &mut App) {
    let Some(menus) = cx.try_global::<AppMenus>() else {
        return;
    };
    if menus.last_state.as_ref() == Some(&state) {
        return;
    }
    cx.set_menus(app_menus(&state));
    cx.global_mut::<AppMenus>().last_state = Some(state);
}

#[cfg(test)]
pub(crate) fn current_menu_state(cx: &App) -> Option<MenuState> {
    cx.try_global::<AppMenus>()?.last_state.clone()
}

fn active_directory_window(cx: &App) -> Option<WindowHandle<DirectoryWindow>> {
    cx.active_window()
        .and_then(|window| window.downcast::<DirectoryWindow>())
}

/// Global action handlers run while the dispatching window is borrowed, so the
/// window update is deferred to the end of the current effect cycle.
fn update_active_window(
    cx: &mut App,
    update: impl FnOnce(&mut DirectoryWindow, &mut Window, &mut Context<DirectoryWindow>) + 'static,
) {
    let Some(handle) = active_directory_window(cx) else {
        return;
    };
    cx.defer(move |cx| {
        let _ = handle.update(cx, update);
    });
}

fn route_edit_action(
    cx: &mut App,
    text_action: Box<dyn gpui::Action>,
    file_action: Box<dyn gpui::Action>,
) {
    update_active_window(cx, move |_, window, cx| {
        let text_field_focused = window
            .context_stack()
            .iter()
            .any(|context| context.contains("NativeTextInput"));
        window.dispatch_action(
            if text_field_focused {
                text_action
            } else {
                file_action
            },
            cx,
        );
    });
}

fn request_quit(services: &NativeServices, open_window: Rc<OpenWindowFn>, cx: &mut App) {
    let window = active_directory_window(cx).or_else(|| {
        cx.windows()
            .into_iter()
            .find_map(|window| window.downcast::<DirectoryWindow>())
    });
    if let Some(window) = window {
        cx.defer(move |cx| {
            let _ = window.update(cx, |view, _, cx| view.request_app_exit(cx));
        });
        return;
    }
    // macOS keeps running with every window closed; remote drives can still
    // hold unsynced uploads. Quit directly only when nothing needs attention,
    // otherwise reopen a window so the usual prompts can explain why.
    if services.mutations.request_exit() {
        reopen_and_request_exit(open_window, cx);
        return;
    }
    let check = services.remotes.disconnect_all_if_clean_task();
    cx.spawn(async move |cx| {
        let clean = matches!(check.await, Ok(true));
        cx.update(|cx| {
            if clean {
                cx.quit();
            } else {
                reopen_and_request_exit(open_window, cx);
            }
        });
    })
    .detach();
}

fn reopen_and_request_exit(open_window: Rc<OpenWindowFn>, cx: &mut App) {
    if let Some(window) = open_window(cx) {
        cx.defer(move |cx| {
            let _ = window.update(cx, |view, _, cx| view.request_app_exit(cx));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Keymap, Keystroke};

    fn items(menus: &[Menu], name: &str) -> Vec<(String, Box<dyn gpui::Action>)> {
        menus
            .iter()
            .find(|menu| menu.name.as_ref() == name)
            .unwrap_or_else(|| panic!("{name} menu"))
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, action, .. } => {
                    Some((name.to_string(), action.boxed_clone()))
                }
                _ => None,
            })
            .collect()
    }

    fn action_for(menus: &[Menu], menu: &str, label: &str) -> Box<dyn gpui::Action> {
        items(menus, menu)
            .into_iter()
            .find(|(name, _)| name == label)
            .unwrap_or_else(|| panic!("{menu} > {label}"))
            .1
    }

    /// Mirrors how GPUI's macOS menu picks a key equivalent: the first binding
    /// whose context always matches, otherwise the first binding.
    fn key_equivalent(keymap: &Keymap, action: &dyn gpui::Action) -> Option<Keystroke> {
        let binding = keymap
            .bindings_for_action(action)
            .find(|binding| binding.predicate().is_none())
            .or_else(|| keymap.bindings_for_action(action).next())?;
        (binding.keystrokes().len() == 1).then(|| binding.keystrokes()[0].inner().clone())
    }

    fn keystroke(source: &str) -> Option<Keystroke> {
        Some(Keystroke::parse(source).unwrap())
    }

    /// Windows binds Move to Trash to the bare Delete key, and bare-key
    /// commands run through `RunCommand` so they don't show as menu key
    /// equivalents; macOS binds it to Cmd+Backspace.
    fn trash_action() -> Box<dyn gpui::Action> {
        if cfg!(target_os = "macos") {
            Box::new(TrashSelected)
        } else {
            Box::new(RunCommand(CommandId::Trash))
        }
    }

    #[test]
    fn menu_bar_follows_the_finder_layout() {
        let menus = app_menus(&MenuState::default());
        assert_eq!(
            menus
                .iter()
                .map(|menu| menu.name.to_string())
                .collect::<Vec<_>>(),
            [APP_NAME, "File", "Edit", "View", "Go", "Window", "Help"]
        );
        assert!(menus[0].items.iter().any(|item| matches!(
            item,
            MenuItem::SystemMenu(menu) if menu.menu_type == SystemMenuType::Services
        )));
        for (menu, label, action) in [
            (
                APP_NAME,
                "Quit explorie",
                Box::new(Quit) as Box<dyn gpui::Action>,
            ),
            (APP_NAME, "Hide explorie", Box::new(HideApp)),
            (APP_NAME, "Settings…", Box::new(ToggleSettingsPanel)),
            ("File", "New Folder", Box::new(NewFolder)),
            ("File", "Move to Trash", trash_action()),
            ("File", "Close Window", Box::new(CloseTab)),
            ("Edit", "Copy", Box::new(MenuCopy)),
            ("View", "Show Hidden Files", Box::new(ToggleHidden)),
            ("Go", "Enclosing Folder", Box::new(GoUp)),
            ("Go", "Home", Box::new(RunCommand(CommandId::GoHome))),
            ("Go", "Go to Folder…", Box::new(GoToFolder)),
            ("Window", "Minimize", Box::new(MinimizeWindow)),
        ] {
            assert!(
                action_for(&menus, menu, label).partial_eq(action.as_ref()),
                "{menu} > {label}"
            );
        }
        let edit = &menus[2].items;
        assert!(edit.iter().any(|item| matches!(
            item,
            MenuItem::Action {
                os_action: Some(OsAction::Paste),
                ..
            }
        )));
    }

    #[test]
    fn menu_labels_and_checks_follow_the_active_window() {
        let state = MenuState {
            view_mode: Some(ViewMode::Column),
            show_hidden: true,
            show_sidebar: true,
            favorite: true,
            multiple_tabs: true,
            ..MenuState::default()
        };
        let menus = app_menus(&state);
        let labels = |menu| {
            items(&menus, menu)
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        };
        assert!(labels("View").contains(&"Hide Hidden Files".to_string()));
        assert!(labels("View").contains(&"Hide Sidebar".to_string()));
        assert!(labels("View").contains(&"Show Preview".to_string()));
        assert!(labels("File").contains(&"Close Tab".to_string()));
        assert!(labels("File").contains(&"Remove from Favorites".to_string()));
        let checked = menus[3]
            .items
            .iter()
            .filter(|item| item.is_checked())
            .count();
        assert_eq!(checked, 1);
        assert!(menus[3].items[2].is_checked(), "as Columns");
    }

    #[test]
    fn key_equivalents_follow_effective_shortcuts() {
        let keymap = Keymap::new(application_key_bindings(&BTreeMap::new()));
        let menus = app_menus(&MenuState::default());
        let equivalent =
            |menu, label| key_equivalent(&keymap, action_for(&menus, menu, label).as_ref());
        assert_eq!(equivalent("Edit", "Copy"), keystroke("secondary-c"));
        // On macOS this key equivalent is how Cmd+V pastes at all: the
        // keymap leaves it to AppKit so the paste counts as user initiated.
        assert_eq!(equivalent("Edit", "Paste"), keystroke("secondary-v"));
        assert_eq!(equivalent("Edit", "Select All"), keystroke("secondary-a"));
        assert_eq!(
            equivalent("File", "New Folder"),
            keystroke("secondary-shift-n")
        );
        if cfg!(target_os = "macos") {
            assert_eq!(equivalent(APP_NAME, "Quit explorie"), keystroke("cmd-q"));
            assert_eq!(equivalent(APP_NAME, "Hide explorie"), keystroke("cmd-h"));
            assert_eq!(equivalent("Window", "Minimize"), keystroke("cmd-m"));
            assert_eq!(equivalent("File", "Open"), keystroke("cmd-o"));
            assert_eq!(
                equivalent("File", "Move to Trash"),
                keystroke("cmd-backspace")
            );
            assert_eq!(equivalent("Go", "Back"), keystroke("cmd-["));
            assert_eq!(equivalent("Go", "Home"), keystroke("cmd-shift-h"));
            assert_eq!(equivalent("Go", "Go to Folder…"), keystroke("cmd-shift-g"));
            assert_eq!(equivalent("View", "Show Hidden Files"), keystroke("cmd->"));
            // Return renames in Finder, but a bare key is never a menu equivalent.
            assert!(
                action_for(&menus, "File", "Rename…").partial_eq(&RunCommand(CommandId::Rename))
            );
            assert_eq!(equivalent("File", "Rename…"), None);
        }

        let overrides = BTreeMap::from([
            ("file-copy".to_string(), "secondary-alt-shift-c".to_string()),
            (
                "file-rename".to_string(),
                "secondary-alt-shift-r".to_string(),
            ),
            ("nav-back".to_string(), "secondary-alt-j".to_string()),
        ]);
        let keymap = Keymap::new(application_key_bindings(&overrides));
        let menus = app_menus(&MenuState {
            shortcut_overrides: overrides,
            ..MenuState::default()
        });
        let equivalent =
            |menu, label| key_equivalent(&keymap, action_for(&menus, menu, label).as_ref());
        assert_eq!(
            equivalent("Edit", "Copy"),
            keystroke("secondary-alt-shift-c")
        );
        assert_eq!(equivalent("Go", "Back"), keystroke("secondary-alt-j"));
        assert!(action_for(&menus, "File", "Rename…").partial_eq(&RenameSelected));
    }
}
