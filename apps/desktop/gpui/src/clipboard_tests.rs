//! Copy, Cut and Paste through the system file clipboard (an in-memory
//! stand-in here, so tests never replace what the user copied).

use std::fs;
use std::sync::atomic::Ordering;

use explorie_native_services::clipboard::ClipboardFiles;
use explorie_native_services::{FileOperationState, ResourcePaths, ServiceEvent};
use gpui::TestAppContext;
use uuid::Uuid;

use super::*;
use crate::file_clipboard::MemoryFileClipboard;

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    destination: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("explorie-clipboard-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        #[cfg(target_os = "macos")]
        let root = root.canonicalize().unwrap();
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("report.txt"), "report").unwrap();
        fs::write(source.join("notes.txt"), "notes").unwrap();
        Self {
            root,
            source,
            destination,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.source.join(name)
    }

    fn entry(&self, name: &str) -> FileEntry {
        FileEntry {
            id: Uuid::new_v4(),
            path: self.file(name),
            size: 6,
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
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

type TestWindow = (Entity<DirectoryWindow>, Rc<MemoryFileClipboard>);

/// A window on `folder` using a private in-memory clipboard.
fn open_window<'a>(
    cx: &'a mut TestAppContext,
    services: NativeServices,
    folder: &Path,
) -> (TestWindow, &'a mut gpui::VisualTestContext) {
    let clipboard = Rc::new(MemoryFileClipboard::default());
    let shared = Rc::clone(&clipboard);
    let folder = folder.to_path_buf();
    let (view, cx) = cx.add_window_view(move |_, cx| {
        let mut view = DirectoryWindow::new(folder, services, cx);
        view.clipboard.files = shared;
        view
    });
    ((view, clipboard), cx)
}

/// Feed file-operation events to the window until one finishes; true when
/// it completed.
fn finish_operation(
    view: &Entity<DirectoryWindow>,
    cx: &mut gpui::VisualTestContext,
    events: &explorie_native_services::ServiceSubscription,
) -> bool {
    for _ in 0..50 {
        let ServiceEvent::FileOperation(event) = pollster::block_on(events.next()) else {
            continue;
        };
        let state = event.state;
        view.update(cx, |view, cx| view.apply_file_operation_event(event, cx));
        if !matches!(state, FileOperationState::Running) {
            return matches!(state, FileOperationState::Completed);
        }
    }
    panic!("the file operation did not finish");
}

#[gpui::test]
fn copy_and_cut_write_the_system_clipboard_and_keep_the_in_app_state(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let services = NativeServices::new(ResourcePaths::test(&fixture.root));
    let ((view, clipboard), cx) = open_window(cx, services, &fixture.source);
    view.update(cx, |view, cx| {
        view.browser.replace_entries(vec![
            fixture.entry("notes.txt"),
            fixture.entry("report.txt"),
        ]);
        view.browser.select(fixture.file("report.txt"));
        view.copy_selected(cx);
        assert_eq!(
            clipboard.contents(),
            Some(ClipboardFiles {
                paths: vec![fixture.file("report.txt")],
                cut: false,
            })
        );
        assert_eq!(
            view.clipboard.state.as_ref().unwrap().kind,
            ClipboardKind::Copy
        );

        view.browser.select_all();
        view.cut_selected(cx);
        let contents = clipboard.contents().unwrap();
        assert!(contents.cut);
        assert_eq!(contents.paths.len(), 2);
        let in_app = view.clipboard.state.as_ref().unwrap();
        assert_eq!(in_app.kind, ClipboardKind::Cut);
        assert_eq!(in_app.paths, contents.paths);
        assert_eq!(view.paste_count(), Some(2));
    });
}

#[gpui::test]
fn files_copied_elsewhere_paste_as_copies(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let services = NativeServices::new(ResourcePaths::test(&fixture.root));
    let events = services.subscribe_async();
    let ((view, clipboard), cx) = open_window(cx, services, &fixture.destination);
    // Finder (or another explorie window) copied the file.
    clipboard.set(Some(ClipboardFiles {
        paths: vec![fixture.file("report.txt")],
        cut: false,
    }));
    view.update(cx, |view, cx| {
        assert!(view.clipboard.state.is_none());
        view.paste(cx);
        let request = view.operations.latest().unwrap().request().clone();
        assert_eq!(request.kind, FileOperationKind::Copy);
        assert_eq!(
            request.destination.as_deref(),
            Some(fixture.destination.as_path())
        );
    });
    assert!(finish_operation(&view, cx, &events));
    assert_eq!(
        fs::read_to_string(fixture.destination.join("report.txt")).unwrap(),
        "report"
    );
    assert!(
        fixture.file("report.txt").exists(),
        "a copy keeps the source"
    );
    assert!(
        clipboard.contents().is_some(),
        "a copy stays on the clipboard"
    );
}

#[gpui::test]
fn an_explorie_cut_pastes_as_a_move_and_then_clears_the_clipboard(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let services = NativeServices::new(ResourcePaths::test(&fixture.root));
    let events = services.subscribe_async();
    let ((source_view, clipboard), cx) = open_window(cx, services.clone(), &fixture.source);
    source_view.update(cx, |view, cx| {
        view.browser
            .replace_entries(vec![fixture.entry("notes.txt")]);
        view.browser.select(fixture.file("notes.txt"));
        view.cut_selected(cx);
    });
    let contents = clipboard.contents().unwrap();
    assert!(contents.cut);

    // Another window reads the same clipboard and moves the files.
    let destination = fixture.destination.clone();
    let shared = Rc::clone(&clipboard);
    let target_view = cx.new(|cx| {
        let mut view = DirectoryWindow::new(destination, services, cx);
        view.clipboard.files = shared;
        view
    });
    target_view.update(cx, |view, cx| {
        view.paste(cx);
        let request = view.operations.latest().unwrap().request().clone();
        assert_eq!(request.kind, FileOperationKind::Move);
        assert_eq!(request.sources, vec![fixture.file("notes.txt")]);
    });
    assert!(finish_operation(&target_view, cx, &events));
    assert!(fixture.destination.join("notes.txt").exists());
    assert!(!fixture.file("notes.txt").exists());
    assert_eq!(clipboard.contents(), None, "the cut is used up");
    target_view.update(cx, |view, _| {
        assert_eq!(view.paste_count(), None);
    });

    // The window that cut drops its stale state once it sees the clipboard.
    source_view.update(cx, |view, _| {
        assert!(view.clipboard.state.is_some());
        view.refresh_system_clipboard();
        assert!(view.clipboard.state.is_none());
        assert_eq!(view.paste_count(), None);
    });
}

#[gpui::test]
fn a_move_leaves_a_clipboard_that_changed_meanwhile_alone(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let services = NativeServices::new(ResourcePaths::test(&fixture.root));
    let events = services.subscribe_async();
    let ((view, clipboard), cx) = open_window(cx, services, &fixture.destination);
    clipboard.set(Some(ClipboardFiles {
        paths: vec![fixture.file("notes.txt")],
        cut: true,
    }));
    view.update(cx, |view, cx| view.paste(cx));
    // The user copies something else before the move finishes.
    let other = ClipboardFiles {
        paths: vec![fixture.file("report.txt")],
        cut: false,
    };
    clipboard.set(Some(other.clone()));
    assert!(finish_operation(&view, cx, &events));
    assert_eq!(clipboard.contents(), Some(other));
}

#[gpui::test]
fn text_on_the_clipboard_means_nothing_to_paste(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let services = NativeServices::new(ResourcePaths::test(&fixture.root));
    let ((view, clipboard), cx) = open_window(cx, services, &fixture.source);
    view.update(cx, |view, cx| {
        view.browser
            .replace_entries(vec![fixture.entry("notes.txt")]);
        view.browser.select(fixture.file("notes.txt"));
        view.copy_selected(cx);
    });
    // Copying text elsewhere replaces the files (reads report no files).
    clipboard.set(None);
    view.update(cx, |view, cx| {
        view.paste(cx);
        assert!(view.operations.latest().is_none(), "nothing was pasted");
        assert_eq!(view.status_message.as_deref(), Some("Nothing to paste"));
        assert!(
            view.clipboard.state.is_none(),
            "stale in-app state is dropped"
        );

        view.open_empty_context_menu(gpui::point(px(10.0), px(10.0)), cx);
        assert!(view.context_menu.menu.is_none(), "no Paste to offer");
    });
}

#[gpui::test]
fn paste_falls_back_to_the_in_app_state_when_the_clipboard_is_unreadable(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let services = NativeServices::new(ResourcePaths::test(&fixture.root));
    let events = services.subscribe_async();
    let ((view, clipboard), cx) = open_window(cx, services, &fixture.source);
    view.update(cx, |view, cx| {
        view.browser
            .replace_entries(vec![fixture.entry("report.txt")]);
        view.browser.select(fixture.file("report.txt"));
        view.copy_selected(cx);
        view.navigate_to(fixture.destination.clone(), cx);
    });
    clipboard.unreadable.store(true, Ordering::Release);
    view.update(cx, |view, cx| {
        view.open_empty_context_menu(gpui::point(px(10.0), px(10.0)), cx);
        assert_eq!(
            view.context_menu_actions(),
            vec![(ContextMenuAction::Paste, false)]
        );
        view.close_context_menu(cx);
        view.paste(cx);
        let request = view.operations.latest().unwrap().request().clone();
        assert_eq!(request.kind, FileOperationKind::Copy);
        assert_eq!(request.sources, vec![fixture.file("report.txt")]);
    });
    assert!(finish_operation(&view, cx, &events));
    assert!(fixture.destination.join("report.txt").exists());
}

#[gpui::test]
fn paste_affordances_follow_the_system_clipboard(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let services = NativeServices::new(ResourcePaths::test(&fixture.root));
    let ((view, clipboard), cx) = open_window(cx, services, &fixture.destination);
    view.update(cx, |view, cx| {
        view.browser.set_view_mode(ViewMode::List);
        view.listing.state = ListingState::Ready;
        view.open_empty_context_menu(gpui::point(px(10.0), px(10.0)), cx);
        assert!(view.context_menu.menu.is_none());
    });
    cx.simulate_resize(gpui::size(px(1000.0), px(700.0)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("empty-paste").is_none());
    clipboard.set(Some(ClipboardFiles {
        paths: vec![fixture.file("report.txt"), fixture.file("notes.txt")],
        cut: false,
    }));
    view.update(cx, |view, cx| {
        view.open_empty_context_menu(gpui::point(px(10.0), px(10.0)), cx);
        assert_eq!(
            view.context_menu_actions(),
            vec![(ContextMenuAction::Paste, false)]
        );
        assert!(
            view.clipboard.state.is_none(),
            "foreign copies stay out of the panel"
        );
        view.close_context_menu(cx);
    });
    cx.simulate_resize(gpui::size(px(1000.0), px(701.0)));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("empty-paste").is_some(),
        "the empty folder offers Paste"
    );
}

#[gpui::test]
fn edit_menu_paste_uses_the_system_clipboard(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let services = NativeServices::new(ResourcePaths::test(&fixture.root));
    let menu_services = services.clone();
    cx.update(|cx| install_app_menus(cx, menu_services, |_| None));
    let clipboard = Rc::new(MemoryFileClipboard::default());
    let shared = Rc::clone(&clipboard);
    let destination = fixture.destination.clone();
    let (view, window) = cx.add_window_view(move |window, cx| {
        let mut view = DirectoryWindow::new(destination, services, cx);
        view.clipboard.files = shared;
        view.install_shortcut_bindings(cx);
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(800.0), px(600.0)));
    window.update(|window, _| window.activate_window());
    window.run_until_parked();

    // With nothing on the clipboard, Edit > Paste only reports it.
    window.dispatch_action(crate::app_menu::MenuPaste);
    window.run_until_parked();
    view.update(window, |view, _| {
        assert!(view.operations.latest().is_none());
        assert_eq!(view.status_message.as_deref(), Some("Nothing to paste"));
    });

    clipboard.set(Some(ClipboardFiles {
        paths: vec![fixture.file("report.txt")],
        cut: false,
    }));
    window.dispatch_action(crate::app_menu::MenuPaste);
    window.run_until_parked();
    view.update(window, |view, _| {
        let request = view.operations.latest().unwrap().request().clone();
        assert_eq!(request.kind, FileOperationKind::Copy);
        assert_eq!(request.sources, vec![fixture.file("report.txt")]);
    });
}

#[gpui::test]
fn window_activation_refreshes_the_paste_affordances(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let services = NativeServices::new(ResourcePaths::test(&fixture.root));
    let ((view, clipboard), cx) = open_window(cx, services, &fixture.destination);
    cx.simulate_resize(gpui::size(px(800.0), px(600.0)));
    cx.run_until_parked();
    view.update(cx, |view, _| assert_eq!(view.paste_count(), None));

    clipboard.set(Some(ClipboardFiles {
        paths: vec![fixture.file("notes.txt")],
        cut: false,
    }));
    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    view.update(cx, |view, _| assert_eq!(view.paste_count(), Some(1)));
    assert_eq!(
        clipboard.reads(),
        0,
        "activation only peeks, so macOS never asks to read another app's copy"
    );
}

#[gpui::test]
fn files_macos_withholds_explain_how_to_paste_them(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let services = NativeServices::new(ResourcePaths::test(&fixture.root));
    let ((view, clipboard), cx) = open_window(cx, services, &fixture.source);
    view.update(cx, |view, cx| {
        view.browser
            .replace_entries(vec![fixture.entry("notes.txt")]);
        view.browser.select(fixture.file("notes.txt"));
        view.copy_selected(cx);
        view.navigate_to(fixture.destination.clone(), cx);
    });
    // Another app copies files, and macOS refuses a read it did not see the
    // user start from Edit > Paste.
    clipboard.set(Some(ClipboardFiles {
        paths: vec![fixture.file("report.txt")],
        cut: false,
    }));
    clipboard.withheld.store(true, Ordering::Release);
    view.update(cx, |view, cx| {
        view.open_empty_context_menu(gpui::point(px(10.0), px(10.0)), cx);
        assert_eq!(
            view.context_menu_actions(),
            vec![(ContextMenuAction::Paste, false)],
            "a peek still sees the files"
        );
        assert!(
            view.clipboard.state.is_none(),
            "this window's copy is no longer on the clipboard"
        );
        view.close_context_menu(cx);

        view.paste(cx);
        assert!(view.operations.latest().is_none(), "nothing was pasted");
        assert_eq!(
            view.status_message.as_deref(),
            Some(crate::file_clipboard::WITHHELD_MESSAGE),
            "guidance instead of \"Nothing to paste\""
        );
    });
}

#[gpui::test]
fn a_move_never_reads_a_clipboard_another_app_replaced(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let services = NativeServices::new(ResourcePaths::test(&fixture.root));
    let events = services.subscribe_async();
    let ((view, clipboard), cx) = open_window(cx, services, &fixture.destination);
    clipboard.set(Some(ClipboardFiles {
        paths: vec![fixture.file("notes.txt")],
        cut: true,
    }));
    view.update(cx, |view, cx| view.paste(cx));
    let reads = clipboard.reads();
    // Another app copies files before the move finishes.
    let other = ClipboardFiles {
        paths: vec![fixture.file("report.txt")],
        cut: false,
    };
    clipboard.set(Some(other.clone()));
    assert!(finish_operation(&view, cx, &events));
    assert_eq!(clipboard.contents(), Some(other));
    assert_eq!(clipboard.reads(), reads, "only a peek after the move");
}
