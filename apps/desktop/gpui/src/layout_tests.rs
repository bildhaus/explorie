//! Layout checks at the default 1024×768 window, where the sidebar and the
//! preview inspector leave the content the least room.

use gpui::TestAppContext;
use uuid::Uuid;

use super::*;

fn entry_at(path: PathBuf, is_dir: bool) -> FileEntry {
    FileEntry {
        id: Uuid::new_v4(),
        path,
        size: 1_234,
        modified: std::time::SystemTime::UNIX_EPOCH,
        hidden: false,
        is_dir,
        custom: std::collections::HashMap::new(),
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

#[gpui::test]
fn column_strip_scrolls_the_leaf_into_view_without_clipping_the_leftmost_column(
    cx: &mut TestAppContext,
) {
    let leaf = (1..=7).fold(PathBuf::from("column-align"), |path, depth| {
        path.join(format!("level-{depth}"))
    });
    let stack = build_path_stack(&leaf);
    let (view, window) = cx.add_window_view(|window, cx| {
        let mut view = DirectoryWindow::new(leaf.clone(), NativeServices::default(), cx);
        view.visuals.icon_loading_enabled = false;
        view.browser.set_view_mode(ViewMode::Column);
        view.settings.view.show_preview_panel = true;
        view.column_view.columns = ColumnState::new(&leaf);
        for (index, path) in stack.iter().enumerate() {
            let mut entries = (0..10)
                .map(|item| entry_at(path.join(format!("folder-{item}")), true))
                .collect::<Vec<_>>();
            if let Some(child) = stack.get(index + 1) {
                entries.push(entry_at(child.clone(), true));
            }
            assert!(view.column_view.columns.apply_listed(path, entries));
            // Narrow enough that the leaf, its parent and the preview fit
            // whole in the strip, but a third column does not.
            view.browser.set_column_view_width(path.clone(), 200);
        }
        view.column_view.scroll_handles = stack
            .iter()
            .map(|_| UniformListScrollHandle::new())
            .collect();
        view.column_view.scroll_to_leaf_attempts = 3;
        view.listing.state = ListingState::Ready;
        window.focus(&view.focus_handle, cx);
        view
    });
    window.simulate_resize(gpui::size(px(1_024.0), px(768.0)));
    window.run_until_parked();
    // Reveal the leaf the way navigating there does, now that the window has
    // its final size.
    view.update(window, |view, cx| {
        view.column_view.scroll_to_leaf_attempts = 3;
        cx.notify();
    });
    window.run_until_parked();

    let strip = view.update(window, |view, _| view.column_view.strip_scroll.clone());
    let strip_bounds = strip.bounds();
    assert!(
        strip.offset().x < px(0.0),
        "the strip must scroll to the leaf"
    );
    let columns = (0..stack.len())
        .map(|index| {
            window
                .debug_bounds(Box::leak(format!("column-{index}").into_boxed_str()))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let visible = columns
        .iter()
        .enumerate()
        .filter(|(_, bounds)| bounds.right() > strip_bounds.left() + px(0.5))
        .collect::<Vec<_>>();
    let (first_index, first) = visible[0];
    assert!(
        (f32::from(first.left()) - f32::from(strip_bounds.left())).abs() < 0.5,
        "leftmost visible column {first_index} {first:?} is clipped by strip {strip_bounds:?}"
    );
    assert_eq!(
        first_index,
        stack.len() - 2,
        "the parent column must be shown whole when there is room for it"
    );
    let leaf_bounds = columns.last().unwrap();
    assert!(leaf_bounds.right() <= strip_bounds.right() + px(0.5));
    let preview = window.debug_bounds("column-preview").unwrap();
    assert!(
        (f32::from(preview.right()) - f32::from(strip_bounds.right())).abs() < 0.5,
        "the preview {preview:?} must reach the strip's end {strip_bounds:?}"
    );
}

#[gpui::test]
fn list_names_stay_whole_beside_a_custom_column_with_the_inspector_open(cx: &mut TestAppContext) {
    let directory = PathBuf::from("list-layout").join("Documents");
    let names = [
        "data.json",
        "notes.txt",
        "letter.docx",
        "README.md",
        "report.pdf",
        "summary.xlsx",
    ];
    let entries = names
        .iter()
        .map(|name| {
            let mut entry = entry_at(directory.join(name), false);
            entry.modified = std::time::SystemTime::now();
            entry
                .custom
                .insert("status".to_string(), serde_json::json!("Draft"));
            entry
        })
        .collect::<Vec<_>>();
    let (view, window) = cx.add_window_view(|_, cx| {
        DirectoryWindow::new(directory.clone(), NativeServices::default(), cx)
    });
    view.update(window, |view, cx| {
        view.visuals.icon_loading_enabled = false;
        view.settings.view.show_preview_panel = true;
        view.browser.replace_entries(entries);
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    window.simulate_resize(gpui::size(px(1_024.0), px(768.0)));
    window.run_until_parked();

    assert!(window.debug_bounds("preview-panel").is_some());
    assert!(window.debug_bounds("sort-custom-status").is_some());
    let visible = view.update(window, |view, _| {
        view.browser
            .visible_entries()
            .iter()
            .map(|entry| file_name(entry))
            .collect::<Vec<_>>()
    });
    // The test text system advances every character 0.6 em, so a 14 px
    // label is 8.4 px per character.
    for (index, name) in visible.iter().enumerate() {
        let bounds = window
            .debug_bounds(Box::leak(format!("entry-{index}-name").into_boxed_str()))
            .unwrap();
        let needed = name.chars().count() as f32 * 8.4;
        assert!(
            f32::from(bounds.size.width) + 0.5 >= needed,
            "{name} is truncated to {:?}, needing {needed}px",
            bounds.size.width
        );
    }
    // The other columns stay readable rather than collapsing to nothing.
    for selector in ["sort-custom-status", "sort-size", "sort-modified"] {
        assert!(f32::from(window.debug_bounds(selector).unwrap().size.width) >= 56.0);
    }
}

fn crumb_bounds(window: &mut gpui::VisualTestContext, index: usize) -> gpui::Bounds<Pixels> {
    window
        .debug_bounds(Box::leak(format!("breadcrumb-{index}").into_boxed_str()))
        .unwrap()
}

#[gpui::test]
fn toolbar_breadcrumbs_take_the_room_the_search_field_can_spare(cx: &mut TestAppContext) {
    let directory = ["Users", "someone", "Desktop", "Fixture", "Documents"]
        .iter()
        .fold(PathBuf::from("Volumes"), |path, part| path.join(part));
    let stack = build_path_stack(&directory);
    let current = stack.len() - 1;
    let (view, window) = cx.add_window_view(|_, cx| {
        DirectoryWindow::new(directory.clone(), NativeServices::default(), cx)
    });
    view.update(window, |view, cx| {
        view.visuals.icon_loading_enabled = false;
        view.settings.view.show_preview_panel = true;
        view.listing.state = ListingState::Ready;
        cx.notify();
    });
    window.simulate_resize(gpui::size(px(1_024.0), px(768.0)));
    window.run_until_parked();

    let toolbar = window.debug_bounds("browser-toolbar").unwrap();
    let breadcrumbs = window.debug_bounds("breadcrumbs").unwrap();
    let trailing = window.debug_bounds("toolbar-trailing-controls").unwrap();
    let search = window.debug_bounds("search").unwrap();
    assert_eq!(f32::from(toolbar.size.height), 40.0, "single-row toolbar");
    assert!(
        trailing.right() <= toolbar.right() + px(0.5),
        "trailing controls {trailing:?} overflow the toolbar {toolbar:?}"
    );
    assert_eq!(
        f32::from(search.size.width),
        112.0,
        "the search field shrinks before the path collapses"
    );
    assert!(f32::from(breadcrumbs.size.width) > 96.0);
    // The test text system advances every character 0.6 em: 8.4 px at 14 px,
    // plus the crumb's 8 px of padding.
    let current_bounds = crumb_bounds(window, current);
    assert!(
        f32::from(current_bounds.size.width) + 0.5 >= "Documents".len() as f32 * 8.4 + 8.0,
        "the current folder {current_bounds:?} must stay whole"
    );
    assert!(current_bounds.right() <= breadcrumbs.right());
    let parent = crumb_bounds(window, current - 1);
    assert!(
        parent.left() >= breadcrumbs.left() && parent.right() <= current_bounds.left(),
        "the nearest parent {parent:?} keeps what room is left before {current_bounds:?}"
    );

    // With room to spare the whole path shows and the search field is whole.
    window.simulate_resize(gpui::size(px(1_600.0), px(768.0)));
    window.run_until_parked();
    assert_eq!(
        f32::from(window.debug_bounds("search").unwrap().size.width),
        180.0
    );
    let breadcrumbs = window.debug_bounds("breadcrumbs").unwrap();
    for (index, path) in stack.iter().enumerate() {
        let label = path.file_name().unwrap().to_string_lossy();
        let bounds = crumb_bounds(window, index);
        assert!(
            f32::from(bounds.size.width) + 0.5 >= label.chars().count() as f32 * 8.4 + 8.0,
            "{label} is truncated to {bounds:?}"
        );
        assert!(bounds.left() >= breadcrumbs.left() && bounds.right() <= breadcrumbs.right());
    }
}

#[gpui::test]
fn markdown_preview_renders_structure_instead_of_raw_markup(cx: &mut TestAppContext) {
    let directory = std::env::temp_dir().join(format!("explorie-markdown-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let readme = directory.join("README.md");
    std::fs::write(
        &readme,
        "# Explorie test\n\n\
         [Docs](https://example.com/docs) stay offline.\n\n\
         Some **markdown** with a [link](https://example.com).\n\n\
         ## Details\n\n\
         - first\n- second\n\n\
         > quoted\n\n\
         ```rust\nfn main() {}\n```\n\n\
         ---\n\n\
         | Name | Size |\n| --- | --- |\n| a.txt | 1 KB |\n",
    )
    .unwrap();
    let services = NativeServices::new(explorie_native_services::ResourcePaths::test(&directory));
    let (view, window) =
        cx.add_window_view(|_, cx| DirectoryWindow::new(directory.clone(), services, cx));
    view.update(window, |view, cx| {
        view.visuals.icon_loading_enabled = false;
        view.settings.view.show_preview_panel = true;
        view.browser
            .replace_entries(vec![entry_at(readme.clone(), false)]);
        view.listing.state = ListingState::Ready;
        view.browser.select(readme.clone());
        view.preview_selected(cx);
    });
    for _ in 0..4_000 {
        window.run_until_parked();
        if view.update(window, |view, _| {
            matches!(
                view.preview.state,
                PreviewState::Ready {
                    content: PreviewContent::Rich(_),
                    ..
                }
            )
        }) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    view.update(window, |view, _| {
        let PreviewState::Ready {
            content: PreviewContent::Rich(preview),
            ..
        } = &view.preview.state
        else {
            panic!("the Markdown preview did not load");
        };
        assert_eq!(preview.title, "Explorie test");
        for block in &preview.blocks {
            assert_ne!(block.text, "Explorie test", "the title is repeated");
            assert!(!block.text.starts_with('#'), "raw heading {:?}", block.text);
            assert!(!block.text.contains("**"), "raw emphasis {:?}", block.text);
            assert!(!block.text.contains("]("), "raw link {:?}", block.text);
        }
    });
    window.simulate_resize(gpui::size(px(1_024.0), px(768.0)));
    window.run_until_parked();

    assert!(window.debug_bounds("rich-preview").is_some());
    assert!(window.debug_bounds("rich-preview-title").is_some());
    for selector in [
        "rich-link-0",
        "rich-paragraph-1",
        "rich-link-1",
        "rich-heading-2-2",
        "rich-list-item-3",
        "rich-list-item-4",
        "rich-quote-5",
        "rich-code-6",
        "rich-rule-7",
        "rich-table-row-8",
        "rich-table-row-9",
    ] {
        assert!(
            window.debug_bounds(selector).is_some(),
            "missing rendered Markdown element {selector}"
        );
    }

    // A link opens in the system browser only when clicked.
    assert_eq!(window.opened_url(), None);
    let docs = window.debug_bounds("rich-link-0").unwrap();
    window.simulate_click(
        gpui::point(docs.left() + px(4.0), docs.center().y),
        gpui::Modifiers::default(),
    );
    window.run_until_parked();
    assert_eq!(
        window.opened_url().as_deref(),
        Some("https://example.com/docs")
    );

    std::fs::remove_dir_all(directory).unwrap();
}
