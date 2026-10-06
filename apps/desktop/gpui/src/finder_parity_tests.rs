//! Behaviors found by testing the signed macOS build against Finder: the
//! operations history around conflict prompts, modal prompts drawn above the
//! operations panel, search, and the smaller Finder conventions.

use super::tests::{fixture_dir, kept_name, remove_fixture, secondary_keystroke};
use super::window::search::SUBFOLDER_SEARCH_DELAY;
use super::*;
use explorie_native_services::ResourcePaths;
use gpui::{Keystroke, Modifiers, TestAppContext, VisualTestContext};
use std::fs;

/// Run the executor until `done` holds, letting native jobs finish on their
/// own threads.
fn wait_until(
    view: &Entity<DirectoryWindow>,
    cx: &mut VisualTestContext,
    what: &str,
    mut done: impl FnMut(&DirectoryWindow) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        cx.run_until_parked();
        if view.update(cx, |view, _| done(view)) {
            return;
        }
        if Instant::now() >= deadline {
            view.update(cx, |view, _| {
                eprintln!(
                    "state: path={:?} mode={:?} names={:?} task={} subfolders={:?} status={:?} columns={:?} listing={:?}",
                    view.browser.path(),
                    view.browser.view_mode(),
                    visible_names(view),
                    view.search.task.is_some(),
                    view.search.subfolders,
                    view.status_message,
                    view.column_view.columns.columns().iter().map(|c| (c.path().to_path_buf(), c.loading(), c.entries().len())).collect::<Vec<_>>(),
                    view.listing.state,
                );
            });
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A destination folder already holding `report.txt`, and a source folder
/// with a newer `report.txt` to copy into it.
struct ConflictFixture {
    root: PathBuf,
    source: PathBuf,
    destination: PathBuf,
}

impl ConflictFixture {
    fn new() -> Self {
        let root = fixture_dir();
        let source_dir = root.join("source");
        let destination = root.join("destination");
        fs::create_dir(&source_dir).unwrap();
        fs::create_dir(&destination).unwrap();
        let source = source_dir.join("report.txt");
        fs::write(&source, "new").unwrap();
        fs::write(destination.join("report.txt"), "existing").unwrap();
        Self {
            root,
            source,
            destination,
        }
    }

    /// A window on the destination that started copying the source into
    /// it and is now asking what to do about the conflict.
    fn open_at_conflict<'a>(
        &self,
        cx: &'a mut TestAppContext,
    ) -> (Entity<DirectoryWindow>, &'a mut VisualTestContext) {
        let services = NativeServices::new(ResourcePaths::test(&self.root));
        let destination = self.destination.clone();
        let (view, window) =
            cx.add_window_view(|_, cx| DirectoryWindow::new(destination, services, cx));
        window.simulate_resize(gpui::size(px(800.0), px(600.0)));
        view.update(window, |view, cx| {
            view.start_service_events(cx);
            view.start_file_operation(
                FileOperationRequest {
                    kind: FileOperationKind::Copy,
                    sources: vec![self.source.clone()],
                    destination: Some(self.destination.clone()),
                    conflict_policy: ConflictPolicy::Error,
                },
                cx,
            );
        });
        wait_until(&view, window, "the conflict prompt", |view| {
            !view.operation_ui.conflict_prompts.is_empty()
        });
        (view, window)
    }
}

#[gpui::test]
fn a_pending_conflict_waits_for_a_decision_instead_of_failing(cx: &mut TestAppContext) {
    let fixture = ConflictFixture::new();
    let (view, window) = fixture.open_at_conflict(cx);

    view.update(window, |view, _| {
        let operations = view.operations.operations();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].status(), OperationStatus::NeedsDecision);
        assert_eq!(operations[0].error(), None);
        assert_eq!(
            view.status_message.as_deref(),
            Some("A destination conflict needs your decision")
        );
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds("operation-decision-0").is_some(),
        "the history row explains it is waiting for a decision"
    );
    assert!(window.debug_bounds("retry-operation-0").is_none());

    let keep_both = window.debug_bounds("conflict-keep-both").unwrap().center();
    window.simulate_click(keep_both, Modifiers::default());
    let kept = fixture.destination.join(kept_name("report.txt"));
    wait_until(&view, window, "the kept copy", |view| {
        kept.is_file() && view.operations.active_count() == 0
    });

    assert_eq!(fs::read_to_string(&kept).unwrap(), "new");
    assert_eq!(
        fs::read_to_string(fixture.destination.join("report.txt")).unwrap(),
        "existing"
    );
    view.update(window, |view, _| {
        // One entry, now showing the outcome of the decision.
        let operations = view.operations.operations();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].status(), OperationStatus::Completed);
        assert_eq!(operations[0].total_items(), 1);
        assert_eq!(operations[0].error(), None);
        assert!(view.operation_ui.conflict_prompts.is_empty());
    });
    window.run_until_parked();
    assert!(window.debug_bounds("operation-decision-0").is_none());
    remove_fixture(&fixture.root);
}

#[gpui::test]
fn skipping_the_only_conflict_records_a_skipped_operation(cx: &mut TestAppContext) {
    let fixture = ConflictFixture::new();
    let (view, window) = fixture.open_at_conflict(cx);

    let skip = window.debug_bounds("conflict-skip").unwrap().center();
    window.simulate_click(skip, Modifiers::default());
    window.run_until_parked();

    view.update(window, |view, _| {
        let operations = view.operations.operations();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].status(), OperationStatus::Skipped);
        assert_eq!(operations[0].skipped_items(), 1);
        assert_eq!(operations[0].retryable_count(), 0);
        assert!(view.operation_ui.conflict_prompts.is_empty());
    });
    assert!(!fixture.destination.join(kept_name("report.txt")).exists());
    remove_fixture(&fixture.root);
}

#[gpui::test]
fn cancel_all_records_the_waiting_operation_as_cancelled(cx: &mut TestAppContext) {
    let fixture = ConflictFixture::new();
    let (view, window) = fixture.open_at_conflict(cx);

    view.update(window, |view, cx| view.cancel_all_file_conflicts(cx));
    window.run_until_parked();

    view.update(window, |view, _| {
        let operations = view.operations.operations();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].status(), OperationStatus::Cancelled);
        assert_eq!(operations[0].error(), None);
    });
    remove_fixture(&fixture.root);
}

/// Fill the operations history with finished copies so the panel is tall
/// enough to reach any dialog drawn in the middle of the window.
fn fill_operation_history(view: &mut DirectoryWindow, destination: &Path) {
    for index in 0..8 {
        let id = format!("finished-{index}");
        view.operations.track(
            id.clone(),
            FileOperationRequest {
                kind: FileOperationKind::Copy,
                sources: vec![destination.join(format!("item-{index}.txt"))],
                destination: Some(destination.to_path_buf()),
                conflict_policy: ConflictPolicy::Error,
            },
        );
        view.operations.apply(FileOperationEvent {
            job_id: id,
            state: explorie_native_services::FileOperationState::Completed,
            progress: None,
            result: None,
            retryable_sources: Vec::new(),
            error: None,
        });
    }
    view.operation_ui.panel_hidden = false;
}

fn intersection(a: Bounds<Pixels>, b: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
    let left = a.left().max(b.left());
    let top = a.top().max(b.top());
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    (left < right && top < bottom)
        .then(|| Bounds::from_corners(point(left, top), point(right, bottom)))
}

#[gpui::test]
fn modal_prompts_draw_and_take_clicks_above_the_operations_panel(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let report = directory.join("report.txt");
    fs::write(&report, "report").unwrap();
    let services = NativeServices::new(ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    view.update(window, |view, cx| {
        fill_operation_history(view, &directory);
        view.prompt_rename_path(report.clone(), cx);
    });
    window.run_until_parked();

    let panel = window.debug_bounds("operation-panel").unwrap();
    let dialog = window.debug_bounds("mutation-prompt-dialog").unwrap();
    let submit = window.debug_bounds("mutation-prompt-submit").unwrap();
    let minimize = window.debug_bounds("minimize-operations").unwrap();
    let overlap = intersection(panel, submit)
        .expect("the test needs the panel to reach the dialog's Rename button");
    assert!(intersection(minimize, dialog).is_none());

    // The panel lies under the dialog's backdrop: its buttons can't be used
    // while the prompt is open.
    window.simulate_click(minimize.center(), Modifiers::default());
    view.update(window, |view, _| {
        assert!(!view.operation_ui.panel_minimized);
        assert!(view.mutation.prompt.is_some());
    });

    // The Rename button is fully usable even where the panel would cover it.
    view.update(window, |view, cx| {
        let prompt = view.mutation.prompt.as_mut().unwrap();
        prompt.input = "renamed.txt".to_string();
        prompt.replace_on_type = false;
        cx.notify();
    });
    window.run_until_parked();
    window.simulate_click(overlap.center(), Modifiers::default());
    let renamed = directory.join("renamed.txt");
    wait_until(&view, window, "the rename", |_| renamed.is_file());
    assert!(!report.exists());
    remove_fixture(&directory);
}

#[gpui::test]
fn the_conflict_prompt_is_not_covered_by_the_operations_panel(cx: &mut TestAppContext) {
    let fixture = ConflictFixture::new();
    let (view, window) = fixture.open_at_conflict(cx);
    view.update(window, |view, cx| {
        fill_operation_history(view, &fixture.destination);
        cx.notify();
    });
    window.run_until_parked();

    let panel = window.debug_bounds("operation-panel").unwrap();
    let cancel = window.debug_bounds("conflict-cancel-all").unwrap();
    let dialog = window.debug_bounds("file-conflict-dialog").unwrap();
    assert_eq!(f32::from(dialog.size.width), 560.0);
    let overlap = intersection(panel, cancel)
        .expect("the test needs the panel to reach the dialog's Cancel All button");
    window.simulate_click(overlap.center(), Modifiers::default());
    view.update(window, |view, _| {
        assert!(view.operation_ui.conflict_prompts.is_empty());
    });
    remove_fixture(&fixture.root);
}

/// A folder to search: `archive/` and `projects/` folders, files with and
/// without "notes" in their names, and one more notes file deeper down.
struct SearchFixture {
    root: PathBuf,
    resources: PathBuf,
}

impl SearchFixture {
    fn new() -> Self {
        let root = fixture_dir();
        fs::create_dir(root.join("archive")).unwrap();
        fs::create_dir_all(root.join("projects/2026")).unwrap();
        for name in ["notes-link.txt", "notes.md", "todo.txt"] {
            fs::write(root.join(name), name).unwrap();
        }
        fs::write(root.join("projects/2026/deep-notes.txt"), "deep").unwrap();
        Self {
            root,
            resources: fixture_dir(),
        }
    }

    fn remove(self) {
        remove_fixture(&self.root);
        remove_fixture(&self.resources);
    }

    /// A focused window on the folder, with the app's real key bindings and
    /// its listing loaded.
    fn open<'a>(
        &self,
        cx: &'a mut TestAppContext,
    ) -> (Entity<DirectoryWindow>, &'a mut VisualTestContext) {
        let services = NativeServices::new(ResourcePaths::test(&self.resources));
        let root = self.root.clone();
        let (view, window) = cx.add_window_view(|_, cx| {
            let view = DirectoryWindow::new(root, services, cx);
            view.install_shortcut_bindings(cx);
            view
        });
        window.simulate_resize(gpui::size(px(900.0), px(650.0)));
        view.update(window, |view, cx| view.start_listing(cx));
        wait_until(&view, window, "the folder listing", |view| {
            view.browser.visible_entries().len() == 5
        });
        focus_list(&view, window);
        (view, window)
    }
}

fn focus_list(view: &Entity<DirectoryWindow>, window: &mut VisualTestContext) {
    let focus = view.update(window, |view, _| view.focus_handle.clone());
    window.update(|window, cx| window.focus(&focus, cx));
    window.run_until_parked();
}

fn list_has_focus(view: &Entity<DirectoryWindow>, window: &mut VisualTestContext) -> bool {
    let focus = view.update(window, |view, _| view.focus_handle.clone());
    window.update(|window, _| focus.is_focused(window))
}

fn press(window: &mut VisualTestContext, keystroke: Keystroke) {
    window.update(|window, cx| {
        window.dispatch_keystroke(keystroke, cx);
    });
}

/// Type into whatever text field has the focus, as the IME delivers text.
fn type_text(window: &mut VisualTestContext, text: &str) {
    for character in text.chars() {
        press(
            window,
            Keystroke::parse(&character.to_string())
                .unwrap()
                .with_simulated_ime(),
        );
    }
    window.run_until_parked();
}

fn visible_names(view: &DirectoryWindow) -> Vec<String> {
    view.browser
        .visible_entries()
        .iter()
        .map(|entry| {
            entry
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

fn start_search(window: &mut VisualTestContext, query: &str) {
    press(window, secondary_keystroke("f"));
    window.run_until_parked();
    type_text(window, query);
}

#[gpui::test]
fn escape_in_the_search_field_clears_it_and_returns_to_the_list(cx: &mut TestAppContext) {
    let fixture = SearchFixture::new();
    let (view, window) = fixture.open(cx);

    start_search(window, "notes");
    view.update(window, |view, _| {
        assert!(view.search.active);
        assert_eq!(view.browser.search_query(), "notes");
        assert_eq!(visible_names(view), ["notes-link.txt", "notes.md"]);
    });
    assert!(!list_has_focus(&view, window));

    window.simulate_keystrokes("escape");
    view.update(window, |view, _| {
        assert!(!view.search.active);
        assert_eq!(view.browser.search_query(), "");
        assert_eq!(visible_names(view).len(), 5);
    });
    assert!(list_has_focus(&view, window));

    // The keyboard is back in the list: type-to-select works right away.
    window.simulate_keystrokes("t");
    view.update(window, |view, _| {
        assert_eq!(
            view.browser.selected_path(),
            Some(fixture.root.join("todo.txt").as_path())
        );
    });
    fixture.remove();
}

#[gpui::test]
fn the_clear_button_returns_the_keyboard_to_the_list(cx: &mut TestAppContext) {
    let fixture = SearchFixture::new();
    let (view, window) = fixture.open(cx);

    start_search(window, "notes");
    let clear = window.debug_bounds("clear-search").unwrap().center();
    window.simulate_click(clear, Modifiers::default());
    view.update(window, |view, _| {
        assert!(!view.search.active);
        assert_eq!(view.browser.search_query(), "");
        assert_eq!(view.text_input.target, None);
    });
    assert!(list_has_focus(&view, window));

    window.simulate_keystrokes("t");
    view.update(window, |view, _| {
        assert_eq!(
            view.browser.selected_path(),
            Some(fixture.root.join("todo.txt").as_path())
        );
    });
    fixture.remove();
}

#[gpui::test]
fn going_to_another_folder_ends_the_search(cx: &mut TestAppContext) {
    let fixture = SearchFixture::new();
    let (view, window) = fixture.open(cx);

    start_search(window, "proj");
    view.update(window, |view, cx| {
        assert_eq!(visible_names(view), ["projects"]);
        // As a double-click on the folder does.
        view.open_entry(fixture.root.join("projects"), true, cx);
    });
    wait_until(&view, window, "the subfolder listing", |view| {
        view.browser.visible_entries().len() == 1
    });
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.root.join("projects"));
        assert_eq!(view.browser.search_query(), "");
        assert!(!view.search.active);
        assert_eq!(visible_names(view), ["2026"]);
    });
    assert!(list_has_focus(&view, window));

    // Going back doesn't bring the old filter back either.
    view.update(window, |view, cx| {
        view.browser.set_search_query("2026".to_string());
        view.go_back(cx);
    });
    wait_until(&view, window, "the parent listing", |view| {
        view.browser.visible_entries().len() == 5
    });
    view.update(window, |view, _| {
        assert_eq!(view.browser.search_query(), "")
    });
    fixture.remove();
}

#[gpui::test]
fn items_hidden_by_the_search_cannot_be_opened(cx: &mut TestAppContext) {
    let fixture = SearchFixture::new();
    let (view, window) = fixture.open(cx);
    let archive = fixture.root.join("archive");
    view.update(window, |view, _| view.browser.select(archive.clone()));

    start_search(window, "notes");
    window.simulate_keystrokes("enter");
    assert!(list_has_focus(&view, window));
    view.update(window, |view, _| {
        assert_eq!(view.browser.selection_count(), 0);
        assert!(view.effective_selected_entry().is_none());
    });
    press(window, secondary_keystroke("down"));
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.root.as_path());
    });
    fixture.remove();
}

#[gpui::test]
fn column_view_selection_drops_items_the_search_hides(cx: &mut TestAppContext) {
    let fixture = SearchFixture::new();
    let (view, window) = fixture.open(cx);
    let archive = fixture.root.join("archive");
    let notes = fixture.root.join("notes.md");
    view.update(window, |view, cx| view.set_view_mode(ViewMode::Column, cx));
    wait_until(&view, window, "the column listing", |view| {
        view.column_view
            .columns
            .columns()
            .last()
            .is_some_and(|column| !column.loading())
    });
    view.update(window, |view, _| {
        view.column_view.selection = BTreeSet::from([archive.clone(), notes.clone()]);
    });

    start_search(window, "notes");
    view.update(window, |view, _| {
        assert_eq!(view.column_view.selection, BTreeSet::from([notes.clone()]));
        assert_eq!(
            view.effective_selected_paths(),
            std::slice::from_ref(&notes)
        );
    });
    fixture.remove();
}

#[gpui::test]
fn the_search_field_can_search_subfolders_like_finder(cx: &mut TestAppContext) {
    let fixture = SearchFixture::new();
    let (view, window) = fixture.open(cx);

    start_search(window, "notes");
    let bar = window.debug_bounds("search-scope-bar").unwrap();
    let subfolders = window.debug_bounds("search-scope-subfolders").unwrap();
    // Both choices stay on screen even for this long fixture folder name.
    assert!(subfolders.right() <= bar.right());
    window.simulate_click(subfolders.center(), Modifiers::default());
    wait_until(&view, window, "the subfolder results", |view| {
        view.search.task.is_none() && view.browser.visible_entries().len() == 3
    });
    view.update(window, |view, _| {
        assert_eq!(view.search.scope, SearchScope::Subfolders);
        let mut names = visible_names(view);
        names.sort();
        assert_eq!(names, ["deep-notes.txt", "notes-link.txt", "notes.md"]);
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|status| status.starts_with("3 results in ")),
            "{:?}",
            view.status_message
        );
        // The field keeps the keyboard so typing can refine the search.
        assert!(view.search.active);
    });

    // Refining waits for typing to pause, then searches again.
    type_text(window, "-l");
    window.executor().advance_clock(SUBFOLDER_SEARCH_DELAY);
    wait_until(&view, window, "the refined results", |view| {
        view.search.task.is_none()
            && view
                .search
                .subfolders
                .as_ref()
                .is_some_and(|search| search.query == "notes-l")
    });
    view.update(window, |view, _| {
        assert_eq!(visible_names(view), ["notes-link.txt"]);
    });

    // Back to this folder: its own listing, filtered by the query.
    let this_folder = window
        .debug_bounds("search-scope-this-folder")
        .unwrap()
        .center();
    window.simulate_click(this_folder, Modifiers::default());
    wait_until(&view, window, "the folder listing", |view| {
        view.browser.entries().len() == 5
    });
    view.update(window, |view, _| {
        assert!(view.search.subfolders.is_none());
        assert_eq!(visible_names(view), ["notes-link.txt"]);
    });
    fixture.remove();
}

#[gpui::test]
fn subfolder_results_show_as_a_list_and_column_view_returns_after(cx: &mut TestAppContext) {
    let fixture = SearchFixture::new();
    let (view, window) = fixture.open(cx);
    view.update(window, |view, cx| {
        view.set_view_mode(ViewMode::Column, cx);
        view.search.scope = SearchScope::Subfolders;
    });
    window.run_until_parked();

    start_search(window, "deep");
    window.executor().advance_clock(SUBFOLDER_SEARCH_DELAY);
    wait_until(&view, window, "the subfolder results", |view| {
        view.search.task.is_none() && view.browser.visible_entries().len() == 1
    });
    view.update(window, |view, _| {
        assert_eq!(visible_names(view), ["deep-notes.txt"]);
        assert_eq!(view.browser.view_mode(), ViewMode::List);
        assert_eq!(view.settings.view.view_mode, ViewMode::Column);
    });

    window.simulate_keystrokes("escape");
    wait_until(&view, window, "the column listing", |view| {
        view.column_view
            .columns
            .columns()
            .last()
            .is_some_and(|column| !column.loading() && column.entries().len() == 5)
    });
    view.update(window, |view, _| {
        assert!(view.search.subfolders.is_none());
        assert_eq!(view.browser.view_mode(), ViewMode::Column);
        assert_eq!(view.browser.search_query(), "");
    });
    fixture.remove();
}

fn listed_entry(directory: &Path, name: &str, is_dir: bool, is_package: bool) -> FileEntry {
    FileEntry {
        id: uuid::Uuid::new_v4(),
        path: directory.join(name),
        size: 0,
        modified: SystemTime::UNIX_EPOCH,
        hidden: false,
        is_dir,
        custom: std::collections::HashMap::new(),
        is_symlink: false,
        is_junction: false,
        link_target: None,
        has_xattrs: false,
        is_package,
        link_target_is_dir: false,
        is_cloud_placeholder: false,
        tags: Vec::new(),
    }
}

/// A window listing a `Tool.app` package and a `readme.txt` file, with the
/// file selected.
fn window_with_package(
    cx: &mut TestAppContext,
    view_mode: ViewMode,
) -> (Entity<DirectoryWindow>, &mut VisualTestContext, PathBuf) {
    let directory = PathBuf::from("sample");
    let (view, window) = cx.add_window_view(|_, cx| {
        DirectoryWindow::new(directory.clone(), NativeServices::default(), cx)
    });
    window.simulate_resize(gpui::size(px(900.0), px(650.0)));
    let package = directory.join("Tool.app");
    view.update(window, |view, cx| {
        view.browser.set_view_mode(view_mode);
        view.browser.replace_entries(vec![
            listed_entry(&directory, "readme.txt", false, false),
            listed_entry(&directory, "Tool.app", true, true),
        ]);
        view.listing.state = ListingState::Ready;
        view.browser.select(directory.join("readme.txt"));
        cx.notify();
    });
    window.run_until_parked();
    (view, window, package)
}

fn entry_index(
    view: &Entity<DirectoryWindow>,
    window: &mut VisualTestContext,
    path: &Path,
) -> usize {
    view.update(window, |view, _| {
        view.browser
            .visible_entries()
            .iter()
            .position(|entry| entry.path == path)
            .unwrap()
    })
}

fn assert_package_menu(
    view: &Entity<DirectoryWindow>,
    window: &mut VisualTestContext,
    package: &Path,
) {
    view.update(window, |view, _| {
        assert_eq!(view.browser.selected_paths(), [package.to_path_buf()]);
        let menu = view.context_menu.menu.as_ref().expect("a context menu");
        assert_eq!(menu.paths, [package.to_path_buf()]);
        assert!(
            view.context_menu_actions()
                .iter()
                .any(|(action, _)| *action == ContextMenuAction::ShowPackageContents)
        );
    });
    assert!(
        window
            .debug_bounds("context-menu-show-package-contents")
            .is_some()
    );
    assert!(window.debug_bounds("context-menu-preview").is_none());
}

#[gpui::test]
fn right_clicking_an_unselected_package_selects_it_for_the_menu(cx: &mut TestAppContext) {
    for view_mode in [ViewMode::List, ViewMode::Grid] {
        let (view, window, package) = window_with_package(cx, view_mode);
        let index = entry_index(&view, window, &package);
        let selector = match view_mode {
            ViewMode::Grid => format!("grid-entry-{index}"),
            _ => format!("entry-{index}"),
        };
        let row = window
            .debug_bounds(Box::leak(selector.into_boxed_str()))
            .unwrap();
        window.simulate_mouse_down(row.center(), MouseButton::Right, Modifiers::default());
        window.simulate_mouse_up(row.center(), MouseButton::Right, Modifiers::default());
        assert_package_menu(&view, window, &package);
    }
}

#[cfg(target_os = "macos")]
#[gpui::test]
fn control_clicking_a_package_opens_its_menu_like_finder(cx: &mut TestAppContext) {
    let (view, window, package) = window_with_package(cx, ViewMode::List);
    let index = entry_index(&view, window, &package);
    let row = window
        .debug_bounds(Box::leak(format!("entry-{index}").into_boxed_str()))
        .unwrap();
    let control = Modifiers {
        control: true,
        ..Modifiers::default()
    };
    window.simulate_mouse_down(row.center(), MouseButton::Left, control);
    window.simulate_mouse_up(row.center(), MouseButton::Left, control);
    assert_package_menu(&view, window, &package);
}

#[gpui::test]
fn rename_preselects_a_files_name_without_its_extension(cx: &mut TestAppContext) {
    let directory = fixture_dir();
    let resources = fixture_dir();
    fs::write(directory.join("report.final.pdf"), "pdf").unwrap();
    fs::create_dir(directory.join("Projects.v2")).unwrap();
    let services = NativeServices::new(ResourcePaths::test(&resources));
    let root = directory.clone();
    let (view, window) = cx.add_window_view(|_, cx| DirectoryWindow::new(root, services, cx));
    window.simulate_resize(gpui::size(px(900.0), px(650.0)));
    view.update(window, |view, cx| view.start_listing(cx));
    wait_until(&view, window, "the listing", |view| {
        view.browser.visible_entries().len() == 2
    });

    for (name, expected) in [
        ("report.final.pdf", "summary.pdf"),
        ("Projects.v2", "summary"),
    ] {
        view.update(window, |view, cx| {
            view.prompt_rename_path(directory.join(name), cx);
        });
        window.run_until_parked();
        // Typing replaces the selection.
        type_text(window, "summary");
        view.update(window, |view, cx| {
            assert_eq!(view.mutation.prompt.as_ref().unwrap().input, expected);
            view.cancel_mutation_prompt(cx);
        });
        window.run_until_parked();
    }
    remove_fixture(&directory);
    remove_fixture(&resources);
}

#[gpui::test]
fn escape_cancels_the_rename_prompt_and_keeps_the_selection(cx: &mut TestAppContext) {
    let fixture = SearchFixture::new();
    let (view, window) = fixture.open(cx);
    let todo = fixture.root.join("todo.txt");
    view.update(window, |view, cx| {
        view.browser.select(todo.clone());
        view.prompt_rename_path(todo.clone(), cx);
    });
    window.run_until_parked();
    type_text(window, "renamed");

    window.simulate_keystrokes("escape");
    view.update(window, |view, _| {
        assert!(view.mutation.prompt.is_none());
        assert_eq!(view.browser.selected_path(), Some(todo.as_path()));
    });
    assert!(todo.is_file());
    fixture.remove();
}

#[gpui::test]
fn escape_cancels_a_pending_conflict(cx: &mut TestAppContext) {
    let fixture = ConflictFixture::new();
    let (view, window) = fixture.open_at_conflict(cx);
    view.update(window, |view, cx| view.install_shortcut_bindings(cx));
    focus_list(&view, window);

    window.simulate_keystrokes("escape");
    wait_until(&view, window, "the cancelled copy", |view| {
        view.operation_ui.conflict_prompts.is_empty() && view.operations.active_count() == 0
    });
    assert_eq!(
        fs::read_to_string(fixture.destination.join("report.txt")).unwrap(),
        "existing"
    );
    remove_fixture(&fixture.root);
}

#[gpui::test]
fn the_keyboard_keeps_working_after_clicking_a_button_that_goes_away(cx: &mut TestAppContext) {
    let fixture = ConflictFixture::new();
    let (view, window) = fixture.open_at_conflict(cx);
    view.update(window, |view, cx| {
        view.install_shortcut_bindings(cx);
        view.resolve_file_conflict(FileConflictChoice::KeepBoth, cx);
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let close = loop {
        window.run_until_parked();
        if let Some(close) = window.debug_bounds("close-operations") {
            break close.center();
        }
        assert!(Instant::now() < deadline, "the finished copy never settled");
        std::thread::sleep(Duration::from_millis(10));
    };
    wait_until(&view, window, "both copies listed", |view| {
        view.browser.visible_entries().len() == 2
    });
    focus_list(&view, window);

    // The close button takes the focus, then disappears with the panel.
    window.simulate_click(close, Modifiers::default());
    window.run_until_parked();
    assert!(list_has_focus(&view, window));
    window.simulate_keystrokes("down");
    view.update(window, |view, _| {
        assert_eq!(view.browser.selection_count(), 1)
    });
    remove_fixture(&fixture.root);
}

#[gpui::test]
fn undo_in_a_text_field_edits_the_text_not_the_last_file_operation(cx: &mut TestAppContext) {
    let fixture = SearchFixture::new();
    let (view, window) = fixture.open(cx);
    let todo = fixture.root.join("todo.txt");
    let done = fixture.root.join("done.txt");

    // A rename the window could undo.
    view.update(window, |view, cx| view.prompt_rename_path(todo.clone(), cx));
    window.run_until_parked();
    type_text(window, "done");
    view.update(window, |view, cx| view.submit_mutation_prompt(cx));
    wait_until(&view, window, "the rename", |view| {
        done.is_file() && view.undo_ledger.can_undo(SystemTime::now())
    });

    // Cmd+Z in a text field undoes the typing, as one step, and leaves the
    // rename alone; with nothing left to undo it does nothing.
    view.update(window, |view, cx| view.prompt_rename_path(done.clone(), cx));
    window.run_until_parked();
    type_text(window, "final");
    let input = |view: &DirectoryWindow| view.mutation.prompt.as_ref().unwrap().input.clone();
    view.update(window, |view, _| assert_eq!(input(view), "final.txt"));
    press(window, secondary_keystroke("z"));
    window.run_until_parked();
    view.update(window, |view, _| assert_eq!(input(view), "done.txt"));
    press(window, secondary_keystroke("z"));
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(input(view), "done.txt");
        assert!(view.undo_ledger.can_undo(SystemTime::now()));
    });
    assert!(done.is_file() && !todo.exists());

    // Redo brings the typing back.
    let redo = if cfg!(target_os = "macos") {
        "shift-z"
    } else {
        "y"
    };
    press(window, secondary_keystroke(redo));
    window.run_until_parked();
    view.update(window, |view, _| assert_eq!(input(view), "final.txt"));
    fixture.remove();
}

#[gpui::test]
fn return_confirms_a_prompt_without_a_text_field(cx: &mut TestAppContext) {
    let fixture = SearchFixture::new();
    let (view, window) = fixture.open(cx);
    let todo = fixture.root.join("todo.txt");
    // A path that does not exist fails validation before reaching the Trash.
    let missing = fixture.root.join("missing.txt");
    view.update(window, |view, cx| {
        view.start_service_events(cx);
        view.browser.select(todo.clone());
        view.mutation.prompt = Some(MutationPrompt::new(
            MutationPromptKind::Trash {
                paths: vec![missing.clone()],
            },
            String::new(),
        ));
        cx.notify();
    });
    window.run_until_parked();
    focus_list(&view, window);

    // Return is Rename (macOS) or Open (Windows) in the list; with the
    // confirmation up it confirms instead.
    window.simulate_keystrokes("enter");
    view.update(window, |view, _| {
        assert!(
            view.mutation.prompt.is_none(),
            "{:?}",
            view.mutation.prompt.as_ref().map(|prompt| &prompt.kind)
        );
        let operations = view.operations.operations();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].request().kind, FileOperationKind::Trash);
        assert_eq!(operations[0].request().sources, vec![missing.clone()]);
    });
    assert!(todo.is_file());
    fixture.remove();
}

#[gpui::test]
fn a_locked_folder_handler_disables_the_toggle_and_says_why(cx: &mut TestAppContext) {
    let fixture = SearchFixture::new();
    let (view, window) = fixture.open(cx);
    let reason = "This version of macOS keeps Finder as the app that opens folders.";
    view.update(window, |view, cx| {
        view.system.integration_status = Some(explorie_native_services::SystemIntegrationStatus {
            supported: false,
            enabled: false,
            unavailable_reason: Some(reason.to_string()),
        });
        view.settings_ui.panel_open = true;
        view.settings_ui.tab = SettingsTab::Integration;
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("settings-folder-handler").is_some());

    // Clicking the disabled toggle changes nothing and explains instead of
    // claiming the platform is unsupported.
    view.update(window, |view, cx| view.toggle_system_integration(cx));
    view.update(window, |view, _| {
        assert!(!view.system.integration_pending);
        assert_eq!(
            view.toasts
                .current
                .as_ref()
                .map(|toast| toast.message.as_str()),
            Some(reason)
        );
    });
    fixture.remove();
}

#[gpui::test]
fn media_shortcut_hints_show_only_where_the_keys_work(cx: &mut TestAppContext) {
    use super::render_perf_tests::{Fixture, open_window, preview, wait_for};

    let fixture = Fixture::new();
    let (view, window) = open_window(&fixture, cx);
    preview(window, &view, &fixture.audio_path);
    wait_for(window, &view, "the inspector's audio player", |view, cx| {
        view.media.read(cx).audio_status.is_some()
    });
    window.run_until_parked();
    assert!(window.debug_bounds("audio-seek").is_some());
    // In the inspector J, K, L and M select files by name.
    assert!(window.debug_bounds("media-shortcut-hint").is_none());

    let audio_path = fixture.audio_path.clone();
    view.update(window, |view, cx| {
        view.open_quick_look(audio_path.clone(), vec![audio_path.clone()], cx);
    });
    wait_for(window, &view, "Quick Look's audio player", |view, cx| {
        view.quick_look.open && view.media.read(cx).audio_status.is_some()
    });
    window.run_until_parked();
    assert!(window.debug_bounds("media-shortcut-hint").is_some());
}
