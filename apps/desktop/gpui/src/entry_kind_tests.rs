//! How the browser treats special entries: packages open like files, links
//! to folders are browsed in place, and so on.

use std::fs;
use std::sync::Mutex;

use explorie_native_services::{AppInfo, PlatformActionsBackend, ResourcePaths};
use gpui::TestAppContext;
use uuid::Uuid;

use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
enum Action {
    Open(PathBuf),
    OpenWith(PathBuf, String),
}

#[derive(Clone, Default)]
struct RecordingActions {
    actions: Arc<Mutex<Vec<Action>>>,
}

impl RecordingActions {
    fn actions(&self) -> Vec<Action> {
        self.actions.lock().unwrap().clone()
    }
}

impl PlatformActionsBackend for RecordingActions {
    fn open(&self, path: &Path) -> std::io::Result<()> {
        self.actions
            .lock()
            .unwrap()
            .push(Action::Open(path.to_path_buf()));
        Ok(())
    }

    fn reveal(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }

    fn open_with(&self, path: &Path, app_name: &str) -> std::io::Result<()> {
        self.actions
            .lock()
            .unwrap()
            .push(Action::OpenWith(path.to_path_buf(), app_name.to_string()));
        Ok(())
    }

    fn apps_for_file(&self, _path: &Path) -> std::io::Result<Vec<AppInfo>> {
        Ok(vec![
            AppInfo {
                name: "TextEdit".to_string(),
                path: PathBuf::from("/System/Applications/TextEdit.app"),
                bundle_id: Some("com.apple.TextEdit".to_string()),
                is_default: true,
            },
            AppInfo {
                name: "BBEdit".to_string(),
                path: PathBuf::from("/Applications/BBEdit.app"),
                bundle_id: Some("com.barebones.bbedit".to_string()),
                is_default: false,
            },
        ])
    }

    fn choose_application(&self) -> std::io::Result<Option<PathBuf>> {
        Ok(Some(PathBuf::from("/Applications/Chosen.app")))
    }
}

/// A folder with a plain folder, a package, a link to the folder and a file.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("explorie-entry-kinds-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        #[cfg(target_os = "macos")]
        let root = root.canonicalize().unwrap();
        fs::create_dir(root.join("real")).unwrap();
        fs::write(root.join("real").join("inside.txt"), "inside").unwrap();
        fs::create_dir_all(root.join("Tool.app").join("Contents")).unwrap();
        fs::write(root.join("notes.txt"), "notes").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("real"), root.join("real-link")).unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn entry(&self, name: &str) -> FileEntry {
        let path = self.path(name);
        let mut entry = FileEntry {
            id: Uuid::new_v4(),
            path: path.clone(),
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            hidden: false,
            is_dir: false,
            custom: Default::default(),
            is_symlink: false,
            is_junction: false,
            link_target: None,
            has_xattrs: false,
            is_package: false,
            link_target_is_dir: false,
            is_cloud_placeholder: false,
            tags: Vec::new(),
        };
        match name {
            "real" => entry.is_dir = true,
            "Tool.app" => {
                entry.is_dir = true;
                entry.is_package = true;
            }
            "real-link" => {
                entry.is_symlink = true;
                entry.link_target_is_dir = true;
                entry.link_target = Some(self.path("real").to_string_lossy().into_owned());
            }
            _ => entry.size = 5,
        }
        entry
    }

    fn entries(&self) -> Vec<FileEntry> {
        ["real", "Tool.app", "real-link", "notes.txt"]
            .into_iter()
            .map(|name| self.entry(name))
            .collect()
    }

    fn services(&self, actions: &RecordingActions) -> NativeServices {
        NativeServices::with_platform_actions_backend(
            ResourcePaths::test(&self.root),
            Arc::new(actions.clone()),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn wait_until(
    view: &Entity<DirectoryWindow>,
    cx: &mut gpui::VisualTestContext,
    what: &str,
    mut done: impl FnMut(&DirectoryWindow) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        cx.run_until_parked();
        if view.update(cx, |view, _| done(view)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn row_selector(view: &DirectoryWindow, path: &Path) -> &'static str {
    let index = view
        .browser
        .visible_entries()
        .iter()
        .position(|entry| entry.path == path)
        .unwrap();
    Box::leak(format!("entry-{index}").into_boxed_str())
}

#[gpui::test]
fn packages_open_with_the_system_and_folder_links_are_browsed(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let actions = RecordingActions::default();
    let services = fixture.services(&actions);
    let root = fixture.root.clone();
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(root.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });
    view.update(window, |view, cx| {
        view.browser.set_view_mode(ViewMode::List);
        view.browser.replace_entries(fixture.entries());
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    window.simulate_resize(gpui::size(px(1200.0), px(720.0)));
    window.run_until_parked();

    // Double-clicking a package launches it instead of browsing into it.
    let package = fixture.path("Tool.app");
    let selector = view.update(window, |view, _| row_selector(view, &package));
    let point = window.debug_bounds(selector).unwrap().center();
    window.simulate_click(point, gpui::Modifiers::default());
    window.simulate_event(gpui::MouseDownEvent {
        button: MouseButton::Left,
        position: point,
        modifiers: gpui::Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    window.simulate_event(gpui::MouseUpEvent {
        button: MouseButton::Left,
        position: point,
        modifiers: gpui::Modifiers::default(),
        click_count: 2,
    });
    wait_until(&view, window, "the package to open", |_| {
        actions.actions() == [Action::Open(package.clone())]
    });
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.root)
    });

    // Cmd+O / Cmd+Down / Return all run OpenSelected.
    view.update(window, |view, cx| {
        view.browser.select(package.clone());
        cx.notify();
    });
    window.dispatch_action(OpenSelected);
    wait_until(&view, window, "the package to open again", |_| {
        actions.actions().len() == 2
    });
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.root)
    });

    // A link to a folder is browsed under its own path, which the
    // breadcrumbs and history keep. The fixture only creates the link on Unix;
    // Windows needs a privilege to create symbolic links.
    #[cfg(unix)]
    {
        let link = fixture.path("real-link");
        view.update(window, |view, cx| {
            view.browser.select(link.clone());
            view.open_selected(cx);
        });
        wait_until(&view, window, "the linked folder to list", |view| {
            matches!(view.listing.state, ListingState::Ready) && !view.browser.entries().is_empty()
        });
        view.update(window, |view, _| {
            assert_eq!(view.browser.path(), link);
            assert_eq!(build_path_stack(view.browser.path()).last(), Some(&link));
            assert_eq!(
                view.browser.entries()[0].path,
                link.join("inside.txt"),
                "entries keep the link path"
            );
            assert_eq!(
                actions.actions().len(),
                2,
                "links are not opened externally"
            );
        });
    }
}

#[gpui::test]
fn package_context_menu_offers_show_package_contents(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let actions = RecordingActions::default();
    let services = fixture.services(&actions);
    let root = fixture.root.clone();
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(root.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });
    let package = fixture.path("Tool.app");
    view.update(window, |view, cx| {
        view.browser.replace_entries(fixture.entries());
        view.listing.state = ListingState::Ready;
        view.open_file_context_menu(package.clone(), true, gpui::point(px(10.0), px(10.0)), cx);
        let actions = view.context_menu_actions();
        assert!(actions.contains(&(ContextMenuAction::ShowPackageContents, false)));
        assert!(!actions.contains(&(ContextMenuAction::Preview, false)));
        assert!(!actions.contains(&(ContextMenuAction::ToggleFavorite, true)));

        view.close_context_menu(cx);
        view.open_file_context_menu(
            fixture.path("real-link"),
            false,
            gpui::point(px(10.0), px(10.0)),
            cx,
        );
        let actions = view.context_menu_actions();
        assert!(!actions.contains(&(ContextMenuAction::ShowPackageContents, false)));
        assert!(!actions.contains(&(ContextMenuAction::Preview, false)));
        assert!(actions.contains(&(ContextMenuAction::ToggleFavorite, true)));

        view.close_context_menu(cx);
        view.open_file_context_menu(package.clone(), true, gpui::point(px(10.0), px(10.0)), cx);
        view.execute_context_menu_action(ContextMenuAction::ShowPackageContents, cx);
        assert_eq!(view.browser.path(), package);
    });
    assert!(actions.actions().is_empty());
}

#[gpui::test]
fn column_view_treats_packages_as_files_and_expands_folder_links(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let actions = RecordingActions::default();
    let services = fixture.services(&actions);
    let root = fixture.root.clone();
    let entries = fixture.entries();
    let (view, window) = cx.add_window_view(|window, cx| {
        let mut view = DirectoryWindow::new(root.clone(), services, cx);
        view.browser.set_view_mode(ViewMode::Column);
        view.column_view.columns = ColumnState::new(&root);
        for path in view.column_view.columns.paths() {
            let listed = if path == root {
                entries.clone()
            } else {
                Vec::new()
            };
            assert!(view.column_view.columns.apply_listed(&path, listed));
        }
        view.browser.replace_entries(entries.clone());
        view.column_view.scroll_handles = view
            .column_view
            .columns
            .columns()
            .iter()
            .map(|_| UniformListScrollHandle::new())
            .collect();
        view.settings.view.show_preview_panel = true;
        view.listing.state = ListingState::Ready;
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(4000.0), px(720.0)));
    window.run_until_parked();
    let column_count = view.update(window, |view, _| view.column_view.columns.columns().len());
    let column_row = |view: &DirectoryWindow, path: &Path| -> &'static str {
        let column = column_count - 1;
        let row = view.column_view.columns.columns()[column]
            .visible_entries(&view.browser)
            .iter()
            .position(|entry| entry.path == path)
            .unwrap();
        Box::leak(format!("column-entry-{column}-{row}").into_boxed_str())
    };

    let package = fixture.path("Tool.app");
    let selector = view.update(window, |view, _| column_row(view, &package));
    let point = window.debug_bounds(selector).unwrap().center();
    window.simulate_click(point, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.root, "no child column opens");
        assert_eq!(view.column_view.columns.columns().len(), column_count);
        assert!(view.browser.is_selected(&package));
    });
    // The preview column shows the package as an item.
    assert!(window.debug_bounds("preview-summary-package").is_some());

    // Folder links exist in the fixture only on Unix (see Fixture::new).
    #[cfg(unix)]
    {
        let link = fixture.path("real-link");
        let selector = view.update(window, |view, _| column_row(view, &link));
        let point = window.debug_bounds(selector).unwrap().center();
        window.simulate_click(point, gpui::Modifiers::default());
        wait_until(&view, window, "the link column to list", |view| {
            view.column_view
                .columns
                .columns()
                .last()
                .is_some_and(|column| column.path() == link && !column.loading())
        });
        view.update(window, |view, _| {
            assert_eq!(view.browser.path(), link);
            assert_eq!(view.column_view.columns.columns().len(), column_count + 1);
            assert_eq!(
                view.column_view.columns.columns().last().unwrap().entries()[0].path,
                link.join("inside.txt")
            );
        });
    }
    assert!(actions.actions().is_empty());
}

#[cfg(unix)]
#[gpui::test]
fn files_dropped_on_a_folder_link_land_in_its_target(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let actions = RecordingActions::default();
    let services = fixture.services(&actions);
    let root = fixture.root.clone();
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(root.clone(), services, cx));
    let notes = fixture.entry("notes.txt");
    let link = fixture.path("real-link");
    let drag = FileDrag::from_entries(vec![notes]);
    view.update_in(window, |view, window, cx| {
        view.browser.replace_entries(fixture.entries());
        view.drop_files_to(&drag, link.clone(), false, window, cx);
    });
    let moved = fixture.path("real").join("notes.txt");
    wait_until(&view, window, "the move into the link target", |_| {
        moved.exists()
    });
    assert!(!fixture.path("notes.txt").exists());
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link itself is untouched"
    );
    view.update(window, |view, _| {
        let request = view.operations.latest().unwrap().request().clone();
        assert_eq!(request.destination, Some(fixture.path("real")));
    });
}

#[test]
fn operation_destinations_resolve_folder_links_only() {
    let fixture = Fixture::new();
    use crate::window::drag_drop::operation_destination;
    assert_eq!(
        operation_destination(&fixture.path("real")),
        fixture.path("real")
    );
    #[cfg(unix)]
    {
        assert_eq!(
            operation_destination(&fixture.path("real-link")),
            fixture.path("real")
        );
        std::os::unix::fs::symlink(fixture.path("notes.txt"), fixture.path("file-link")).unwrap();
        assert_eq!(
            operation_destination(&fixture.path("file-link")),
            fixture.path("file-link"),
            "links to files are left for the operation to refuse"
        );
        // Folders inside a linked folder are real folders already.
        assert_eq!(
            operation_destination(&fixture.path("real-link").join("..").join("real")),
            fixture.path("real-link").join("..").join("real")
        );
    }
}

#[gpui::test]
fn cloud_placeholders_show_a_badge_and_are_never_read_for_previews(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let actions = RecordingActions::default();
    let services = fixture.services(&actions);
    let root = fixture.root.clone();
    let photo = fixture.path("photo.jpg");
    fs::write(&photo, b"not really a jpeg").unwrap();
    let mut placeholder = fixture.entry("photo.jpg");
    placeholder.is_cloud_placeholder = true;
    placeholder.size = 4_200_000;
    let local = fixture.entry("notes.txt");
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(root.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });
    view.update(window, |view, cx| {
        view.browser.set_view_mode(ViewMode::List);
        view.browser
            .replace_entries(vec![local.clone(), placeholder.clone()]);
        view.settings.view.show_preview_panel = true;
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    window.simulate_resize(gpui::size(px(1400.0), px(720.0)));
    window.run_until_parked();
    let (local_index, photo_index) = view.update(window, |view, _| {
        let index = |path: &Path| {
            view.browser
                .visible_entries()
                .iter()
                .position(|entry| entry.path == path)
                .unwrap()
        };
        (index(&local.path), index(&photo))
    });
    assert!(
        window
            .debug_bounds(Box::leak(
                format!("entry-cloud-{photo_index}").into_boxed_str()
            ))
            .is_some()
    );
    assert!(
        window
            .debug_bounds(Box::leak(
                format!("entry-cloud-{local_index}").into_boxed_str()
            ))
            .is_none()
    );

    view.update(window, |view, cx| {
        view.browser.select(photo.clone());
        view.sync_pinned_preview(cx);
        assert!(matches!(
            &view.preview.state,
            PreviewState::Ready {
                content: PreviewContent::CloudPlaceholder,
                ..
            }
        ));
        assert!(view.preview.detection.is_none(), "no detection read");
        assert!(matches!(
            view.preview.photo_metadata,
            PhotoMetadataState::Unavailable
        ));
        assert!(view.preview.task.is_none());
    });
    window.run_until_parked();
    assert!(window.debug_bounds("preview-cloud-placeholder").is_some());
    view.update(window, |view, cx| {
        view.set_preview_tab(PreviewTab::Metadata, cx)
    });
    window.run_until_parked();
    assert!(window.debug_bounds("metadata-cloud-placeholder").is_some());

    view.update(window, |view, cx| {
        view.browser.set_view_mode(ViewMode::Grid);
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window
            .debug_bounds(Box::leak(
                format!("grid-entry-cloud-{photo_index}").into_boxed_str()
            ))
            .is_some()
    );
    assert!(
        window
            .debug_bounds(Box::leak(
                format!("grid-entry-cloud-{local_index}").into_boxed_str()
            ))
            .is_none()
    );
    assert!(actions.actions().is_empty());
}

#[derive(Clone, Default)]
struct MemoryTags(Arc<Mutex<Vec<String>>>);

impl explorie_native_services::FinderTagsBackend for MemoryTags {
    fn supported(&self) -> bool {
        true
    }

    fn get(&self, _path: &Path) -> std::io::Result<Vec<String>> {
        Ok(self.0.lock().unwrap().clone())
    }

    fn set(&self, _path: &Path, tags: &[String]) -> std::io::Result<()> {
        *self.0.lock().unwrap() = tags.to_vec();
        Ok(())
    }
}

#[gpui::test]
fn rows_show_finder_tag_dots_and_follow_tag_edits(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let tags = MemoryTags::default();
    let services = NativeServices::with_finder_tags_backend(
        ResourcePaths::test(&fixture.root),
        Arc::new(tags.clone()),
    );
    let root = fixture.root.clone();
    let notes = fixture.path("notes.txt");
    let mut tagged = fixture.entry("notes.txt");
    tagged.has_xattrs = true;
    tagged.tags = vec![
        explorie_core::FinderTag {
            name: "Plain".into(),
            color: 0,
        },
        explorie_core::FinderTag {
            name: "Urgent".into(),
            color: 6,
        },
    ];
    let folder = fixture.entry("real");
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(root.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });
    view.update(window, |view, cx| {
        view.browser.set_view_mode(ViewMode::List);
        view.browser.replace_entries(vec![folder.clone(), tagged]);
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    window.simulate_resize(gpui::size(px(1200.0), px(720.0)));
    window.run_until_parked();
    let (folder_index, notes_index) = view.update(window, |view, _| {
        let index = |path: &Path| {
            view.browser
                .visible_entries()
                .iter()
                .position(|entry| entry.path == path)
                .unwrap()
        };
        (index(&folder.path), index(&notes))
    });
    let selector = |prefix: &str, index: usize| -> &'static str {
        Box::leak(format!("{prefix}-{index}").into_boxed_str())
    };
    let dots = window
        .debug_bounds(selector("entry-tags", notes_index))
        .expect("tagged rows show dots");
    let name_row = window.debug_bounds(selector("entry", notes_index)).unwrap();
    assert!(dots.size.width < px(60.0) && name_row.contains(&dots.center()));
    assert!(
        window
            .debug_bounds(selector("entry-tags", folder_index))
            .is_none()
    );

    view.update(window, |view, cx| {
        view.browser.set_view_mode(ViewMode::Grid);
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window
            .debug_bounds(selector("grid-entry-tags", notes_index))
            .is_some()
    );
    assert!(
        window
            .debug_bounds(selector("grid-entry-tags", folder_index))
            .is_none()
    );

    // Saving tags from the inspector repaints the row right away.
    view.update(window, |view, cx| {
        view.browser.select(notes.clone());
        view.settings.view.show_preview_panel = true;
        view.sync_pinned_preview(cx);
    });
    wait_until(&view, window, "the inspector tags", |view| {
        view.preview.finder_tags.path.as_deref() == Some(notes.as_path())
            && !view.preview.finder_tags.loading
    });
    view.update(window, |view, cx| {
        view.persist_finder_tags(vec!["Review\n4".to_string()], false, cx)
    });
    wait_until(&view, window, "the saved tags", |view| {
        !view.preview.finder_tags.saving
    });
    view.update(window, |view, _| {
        let entry = view
            .browser
            .entries()
            .iter()
            .find(|entry| entry.path == notes)
            .unwrap()
            .clone();
        assert_eq!(
            entry.tags,
            vec![explorie_core::FinderTag {
                name: "Review".into(),
                color: 4
            }]
        );
    });
    assert_eq!(*tags.0.lock().unwrap(), vec!["Review\n4".to_string()]);
}

#[cfg(target_os = "macos")]
#[gpui::test]
fn open_with_lists_the_default_app_first_opens_by_path_and_offers_other(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let actions = RecordingActions::default();
    let services = fixture.services(&actions);
    let root = fixture.root.clone();
    let notes = fixture.path("notes.txt");
    let (view, window) = cx.add_window_view(|window, cx| {
        let view = DirectoryWindow::new(root.clone(), services, cx);
        window.focus(&view.focus_handle(cx), cx);
        view
    });
    let open_menu = |view: &mut DirectoryWindow, cx: &mut Context<DirectoryWindow>| {
        view.open_file_context_menu(notes.clone(), false, gpui::point(px(10.0), px(10.0)), cx);
    };
    view.update(window, |view, cx| {
        view.browser.replace_entries(fixture.entries());
        view.listing.state = ListingState::Ready;
        open_menu(view, cx);
    });
    wait_until(&view, window, "the Open With apps", |view| {
        view.context_menu
            .menu
            .as_ref()
            .is_some_and(|menu| !menu.open_with_apps.is_empty())
    });
    view.update(window, |view, cx| {
        view.execute_context_menu_action(ContextMenuAction::ToggleOpenWith, cx);
        let actions = view.context_menu_actions();
        let open_with: Vec<_> = actions
            .iter()
            .filter(|(action, _)| {
                matches!(
                    action,
                    ContextMenuAction::OpenWithApp(_) | ContextMenuAction::OpenWithOther
                )
            })
            .map(|(action, _)| action.label(1, false, true))
            .collect();
        assert_eq!(
            open_with,
            ["    TextEdit (default)", "    BBEdit", "    Other…"]
        );
        let bbedit = actions
            .into_iter()
            .find_map(|(action, _)| match action {
                ContextMenuAction::OpenWithApp(app) if app.name == "BBEdit" => {
                    Some(ContextMenuAction::OpenWithApp(app))
                }
                _ => None,
            })
            .unwrap();
        view.execute_context_menu_action(bbedit, cx);
    });
    wait_until(&view, window, "Open With BBEdit", |_| {
        actions.actions()
            == [Action::OpenWith(
                notes.clone(),
                "/Applications/BBEdit.app".to_string(),
            )]
    });

    view.update(window, |view, cx| {
        open_menu(view, cx);
        view.execute_context_menu_action(ContextMenuAction::OpenWithOther, cx);
        assert!(view.context_menu.menu.is_none());
    });
    wait_until(&view, window, "Open With the chosen app", |_| {
        actions.actions().len() == 2
    });
    assert_eq!(
        actions.actions()[1],
        Action::OpenWith(notes.clone(), "/Applications/Chosen.app".to_string())
    );
}
