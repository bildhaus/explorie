use std::fs;

use explorie_native_services::ResourcePaths;
use gpui::{KeyBinding, TestAppContext};
use uuid::Uuid;

use super::tests::POLL_TIMEOUT;
use super::*;

struct ColumnFixture {
    root: PathBuf,
    parent: PathBuf,
    child: PathBuf,
}

impl ColumnFixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("explorie-column-{}", Uuid::new_v4()));
        let parent = root.join("parent");
        let child = parent.join("child");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir(parent.join("other")).unwrap();
        fs::write(parent.join("alpha.txt"), "alpha").unwrap();
        fs::write(parent.join("beta.txt"), "beta").unwrap();
        fs::write(child.join("leaf.txt"), "leaf").unwrap();
        Self {
            root,
            parent,
            child,
        }
    }

    fn view(
        &self,
        active: &Path,
        window: &mut Window,
        cx: &mut Context<DirectoryWindow>,
    ) -> DirectoryWindow {
        let services = NativeServices::new(ResourcePaths::test(&self.root));
        let mut view = DirectoryWindow::new(active.to_path_buf(), services, cx);
        view.browser.set_view_mode(ViewMode::Column);
        view.column_view.columns = ColumnState::new(active);
        for path in view.column_view.columns.paths() {
            let entries = if path.starts_with(&self.parent) {
                fs::read_dir(&path)
                    .unwrap()
                    .map(|entry| {
                        let entry = entry.unwrap();
                        let metadata = entry.metadata().unwrap();
                        FileEntry {
                            id: Uuid::new_v4(),
                            path: entry.path(),
                            size: metadata.len(),
                            modified: metadata.modified().unwrap(),
                            hidden: false,
                            is_dir: metadata.is_dir(),
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
                    })
                    .collect()
            } else {
                Vec::new()
            };
            if path == active {
                view.browser.replace_entries(entries.clone());
            }
            assert!(view.column_view.columns.apply_listed(&path, entries));
        }
        view.column_view.scroll_handles = view
            .column_view
            .columns
            .columns()
            .iter()
            .map(|_| UniformListScrollHandle::new())
            .collect();
        view.column_view.scroll_to_leaf_attempts = 0;
        view.settings.view.show_preview_panel = false;
        view.listing.state = ListingState::Ready;
        window.focus(&view.focus_handle, cx);
        view
    }

    fn parent_index(&self) -> usize {
        build_path_stack(&self.parent).len() - 1
    }

    fn row_selector(&self, view: &DirectoryWindow, name: &str) -> &'static str {
        let column_index = self.parent_index();
        let path = self.parent.join(name);
        let row_index = view.column_view.columns.columns()[column_index]
            .visible_entries(&view.browser)
            .iter()
            .position(|entry| entry.path == path)
            .unwrap();
        Box::leak(format!("column-entry-{column_index}-{row_index}").into_boxed_str())
    }

    fn background_selector(&self) -> &'static str {
        Box::leak(format!("column-marquee-surface-{}", self.parent_index()).into_boxed_str())
    }
}

impl Drop for ColumnFixture {
    fn drop(&mut self) {
        assert!(self.root.is_absolute() && self.root.starts_with(std::env::temp_dir()));
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[gpui::test]
fn ancestor_background_click_activates_column_and_clears_selection(cx: &mut TestAppContext) {
    let fixture = ColumnFixture::new();
    let (view, window) = cx.add_window_view(|window, cx| {
        let mut view = fixture.view(&fixture.child, window, cx);
        view.browser.select(fixture.child.join("leaf.txt"));
        view.sync_column_selection_from_browser();
        view
    });
    window.simulate_resize(gpui::size(px(4000.0), px(720.0)));
    window.run_until_parked();
    let bounds = window.debug_bounds(fixture.background_selector()).unwrap();
    let point = gpui::point(bounds.center().x, bounds.bottom() - px(12.0));
    window.simulate_click(point, gpui::Modifiers::default());
    window.run_until_parked();

    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.parent);
        assert_eq!(
            view.column_view.columns.paths().last(),
            Some(&fixture.parent)
        );
        assert_eq!(
            view.column_view.columns.columns().len(),
            fixture.parent_index() + 1
        );
        assert!(view.browser.selected_paths().is_empty());
        assert!(view.effective_selected_paths().is_empty());
        assert!(view.column_view.pending_selection.is_none());
    });
}

#[gpui::test]
fn ancestor_file_click_keeps_keyboard_navigation_in_its_column(cx: &mut TestAppContext) {
    cx.update(|cx| cx.bind_keys([KeyBinding::new("down", SelectNext, Some("browser"))]));
    let fixture = ColumnFixture::new();
    let (view, window) = cx.add_window_view(|window, cx| fixture.view(&fixture.child, window, cx));
    window.simulate_resize(gpui::size(px(4000.0), px(720.0)));
    window.run_until_parked();
    let selector = view.update(window, |view, _| fixture.row_selector(view, "alpha.txt"));
    let point = window.debug_bounds(selector).unwrap().center();
    window.simulate_click(point, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.parent);
        assert_eq!(
            view.effective_selected_paths(),
            vec![fixture.parent.join("alpha.txt")]
        );
    });

    window.simulate_keystrokes("down");
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.parent);
        assert_eq!(
            view.effective_selected_paths(),
            vec![fixture.parent.join("beta.txt")]
        );
        assert_eq!(
            view.browser.selected_paths(),
            view.effective_selected_paths()
        );
    });
}

#[gpui::test]
fn modifier_folder_clicks_select_siblings_without_entering_them(cx: &mut TestAppContext) {
    let fixture = ColumnFixture::new();
    let (view, window) = cx.add_window_view(|window, cx| fixture.view(&fixture.parent, window, cx));
    window.simulate_resize(gpui::size(px(4000.0), px(720.0)));
    window.run_until_parked();

    for (name, modifiers) in [
        (
            "child",
            gpui::Modifiers {
                control: true,
                ..Default::default()
            },
        ),
        (
            "other",
            gpui::Modifiers {
                platform: true,
                ..Default::default()
            },
        ),
    ] {
        let selector = view.update(window, |view, _| fixture.row_selector(view, name));
        let point = window.debug_bounds(selector).unwrap().center();
        window.simulate_click(point, modifiers);
        window.run_until_parked();
    }
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.parent);
        assert_eq!(
            view.effective_selected_paths(),
            vec![fixture.child.clone(), fixture.parent.join("other")]
        );
        assert_eq!(
            view.browser.selected_paths(),
            view.effective_selected_paths()
        );
    });
}

#[gpui::test]
fn column_select_all_updates_effective_selection_and_copy(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("ctrl-a", SelectAll, Some("browser")),
            KeyBinding::new("ctrl-c", CopySelected, Some("browser")),
        ]);
    });
    let fixture = ColumnFixture::new();
    let (view, window) = cx.add_window_view(|window, cx| fixture.view(&fixture.parent, window, cx));
    window.simulate_resize(gpui::size(px(4000.0), px(720.0)));
    window.run_until_parked();
    let selector = view.update(window, |view, _| fixture.row_selector(view, "alpha.txt"));
    let point = window.debug_bounds(selector).unwrap().center();
    window.simulate_click(point, gpui::Modifiers::default());
    window.simulate_keystrokes("ctrl-a ctrl-c");
    window.run_until_parked();

    view.update(window, |view, _| {
        assert_eq!(view.browser.selection_count(), 4);
        assert_eq!(view.effective_selection_count(), 4);
        assert_eq!(
            view.browser.selected_paths(),
            view.effective_selected_paths()
        );
        assert_eq!(
            view.clipboard.state.as_ref().unwrap().paths,
            view.effective_selected_paths()
        );
    });
}

#[gpui::test]
fn ancestor_background_context_menu_targets_clicked_column(cx: &mut TestAppContext) {
    let fixture = ColumnFixture::new();
    let (view, window) = cx.add_window_view(|window, cx| {
        let mut view = fixture.view(&fixture.child, window, cx);
        view.browser.select(fixture.child.join("leaf.txt"));
        view.sync_column_selection_from_browser();
        view.copy_selected(cx);
        view
    });
    window.simulate_resize(gpui::size(px(4000.0), px(720.0)));
    window.run_until_parked();
    let bounds = window.debug_bounds(fixture.background_selector()).unwrap();
    let point = gpui::point(bounds.center().x, bounds.bottom() - px(12.0));
    window.simulate_mouse_down(point, MouseButton::Right, gpui::Modifiers::default());
    window.simulate_mouse_up(point, MouseButton::Right, gpui::Modifiers::default());
    window.run_until_parked();

    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.parent);
        assert_eq!(
            view.column_view.columns.paths().last(),
            Some(&fixture.parent)
        );
        assert!(view.effective_selected_paths().is_empty());
        assert!(view.context_menu.menu.as_ref().unwrap().paths.is_empty());
        assert_eq!(
            view.context_menu_actions(),
            vec![(ContextMenuAction::Paste, false)]
        );
    });
    view.update(window, |view, cx| {
        view.execute_context_menu_action(ContextMenuAction::Paste, cx);
    });
    let pasted = fixture.parent.join("leaf.txt");
    let poll_deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < poll_deadline {
        window.run_until_parked();
        if pasted.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fs::read_to_string(pasted).unwrap(), "leaf");
}

#[gpui::test]
fn column_type_selection_updates_terminal_preview(cx: &mut TestAppContext) {
    let fixture = ColumnFixture::new();
    let (view, window) = cx.add_window_view(|window, cx| {
        let mut view = fixture.view(&fixture.parent, window, cx);
        view.settings.view.show_preview_panel = true;
        view.browser.select(fixture.parent.join("alpha.txt"));
        view.sync_column_selection_from_browser();
        view.preview.state = PreviewState::Loading {
            path: fixture.parent.join("alpha.txt"),
        };
        view
    });
    window.simulate_resize(gpui::size(px(4000.0), px(720.0)));
    window.run_until_parked();
    window.simulate_keystrokes("b");
    window.run_until_parked();
    view.update(window, |view, _| {
        let selected = fixture.parent.join("beta.txt");
        assert_eq!(view.browser.selected_path(), Some(selected.as_path()));
        assert_eq!(view.effective_selected_paths(), vec![selected.clone()]);
        assert_eq!(view.preview.state.path(), Some(selected.as_path()));
    });
}

#[gpui::test]
fn ancestor_shift_folder_click_selects_range_without_opening_folder(cx: &mut TestAppContext) {
    let fixture = ColumnFixture::new();
    let (view, window) = cx.add_window_view(|window, cx| fixture.view(&fixture.child, window, cx));
    window.simulate_resize(gpui::size(px(4000.0), px(720.0)));
    window.run_until_parked();
    for (name, modifiers) in [
        (
            "child",
            gpui::Modifiers {
                control: true,
                ..Default::default()
            },
        ),
        (
            "other",
            gpui::Modifiers {
                shift: true,
                ..Default::default()
            },
        ),
    ] {
        let selector = view.update(window, |view, _| fixture.row_selector(view, name));
        let point = window.debug_bounds(selector).unwrap().center();
        window.simulate_click(point, modifiers);
        window.run_until_parked();
    }
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.parent);
        assert_eq!(
            view.effective_selected_paths(),
            vec![fixture.child.clone(), fixture.parent.join("other")]
        );
    });
}

#[gpui::test]
fn empty_column_background_clears_pending_selection_and_preview(cx: &mut TestAppContext) {
    let fixture = ColumnFixture::new();
    let empty = fixture.parent.join("other");
    let (view, window) = cx.add_window_view(|window, cx| {
        let mut view = fixture.view(&empty, window, cx);
        view.column_view.pending_selection = Some(ColumnSelectionTarget::First);
        view.set_column_selection(fixture.parent.join("alpha.txt"));
        view.preview.state = PreviewState::Loading {
            path: fixture.parent.join("alpha.txt"),
        };
        view
    });
    window.simulate_resize(gpui::size(px(4000.0), px(720.0)));
    window.run_until_parked();
    let selector = Box::leak(
        format!(
            "column-marquee-surface-{}",
            build_path_stack(&empty).len() - 1
        )
        .into_boxed_str(),
    );
    let bounds = window.debug_bounds(selector).unwrap();
    window.simulate_click(bounds.center(), gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), empty);
        assert!(view.column_view.pending_selection.is_none());
        assert!(view.column_view.selection.is_empty());
        assert!(view.browser.selected_paths().is_empty());
        assert!(matches!(view.preview.state, PreviewState::Closed));
    });
}

#[gpui::test]
fn ancestor_marquee_selection_keeps_keyboard_navigation_in_parent(cx: &mut TestAppContext) {
    cx.update(|cx| cx.bind_keys([KeyBinding::new("down", SelectNext, Some("browser"))]));
    let fixture = ColumnFixture::new();
    let (view, window) = cx.add_window_view(|window, cx| fixture.view(&fixture.child, window, cx));
    window.simulate_resize(gpui::size(px(4000.0), px(720.0)));
    window.run_until_parked();
    let (first_selector, last_selector) = view.update(window, |view, _| {
        (
            fixture.row_selector(view, "alpha.txt"),
            fixture.row_selector(view, "beta.txt"),
        )
    });
    let first = window.debug_bounds(first_selector).unwrap();
    let last = window.debug_bounds(last_selector).unwrap();
    let start = gpui::point(last.right() - px(12.0), last.bottom() + px(12.0));
    let end = gpui::point(first.right() - px(30.0), first.center().y);
    window.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
    window.simulate_mouse_move(end, Some(MouseButton::Left), gpui::Modifiers::default());
    window.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.parent);
        assert_eq!(
            view.effective_selected_paths(),
            vec![
                fixture.parent.join("alpha.txt"),
                fixture.parent.join("beta.txt")
            ]
        );
        assert_eq!(
            view.browser.selected_paths(),
            view.effective_selected_paths()
        );
    });
    window.simulate_keystrokes("down");
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.parent);
        assert_eq!(view.browser.selection_count(), 1);
        assert!(
            view.browser
                .selected_path()
                .unwrap()
                .starts_with(&fixture.parent)
        );
        assert!(
            !view
                .browser
                .selected_path()
                .unwrap()
                .starts_with(&fixture.child)
        );
        assert_eq!(
            view.browser.selected_paths(),
            view.effective_selected_paths()
        );
    });
}

#[gpui::test]
fn navigating_child_preserves_parent_column_vertical_scroll(cx: &mut TestAppContext) {
    let fixture = ColumnFixture::new();
    for index in 0..80 {
        fs::write(fixture.parent.join(format!("item-{index:02}.txt")), "item").unwrap();
    }
    let (view, window) = cx.add_window_view(|window, cx| fixture.view(&fixture.parent, window, cx));
    window.simulate_resize(gpui::size(px(4000.0), px(720.0)));
    window.run_until_parked();
    let handle = view.update(window, |view, _| {
        view.column_view.scroll_handles[fixture.parent_index()]
            .0
            .borrow()
            .base_handle
            .clone()
    });
    assert!(handle.max_offset().y > px(200.0));
    handle.set_offset(gpui::point(px(0.0), px(-200.0)));
    view.update(window, |view, cx| {
        view.navigate_to(fixture.child.clone(), cx)
    });
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.child);
        assert_eq!(
            view.column_view.scroll_handles[fixture.parent_index()]
                .0
                .borrow()
                .base_handle
                .offset()
                .y,
            px(-200.0)
        );
    });
    view.update(window, |view, cx| {
        view.activate_column(fixture.parent_index(), cx)
    });
    window.run_until_parked();
    view.update(window, |view, _| {
        assert_eq!(view.browser.path(), fixture.parent);
        assert_eq!(
            view.column_view.scroll_handles[fixture.parent_index()]
                .0
                .borrow()
                .base_handle
                .offset()
                .y,
            px(-200.0)
        );
    });
}
