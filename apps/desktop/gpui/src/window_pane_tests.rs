//! The listing and the sidebar render as cached child views: they must be
//! reused when only another view changed, and rebuilt, re-laid-out and
//! fully interactive whenever the window itself changed.

use gpui::{
    KeyBinding, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, VisualTestContext,
};

use super::render_perf_tests::{Fixture, open_window, preview, wait_for};
use super::*;

fn reset_stats(cx: &mut VisualTestContext, view: &Entity<DirectoryWindow>) {
    view.update(cx, |view, _| view.render_stats = RenderStats::default());
}

fn render_stats(cx: &mut VisualTestContext, view: &Entity<DirectoryWindow>) -> RenderStats {
    view.update(cx, |view, _| view.render_stats)
}

/// A window with the video previewed beside the listing and every area
/// drawn once.
fn window_with_video<'a>(
    fixture: &Fixture,
    cx: &'a mut TestAppContext,
) -> (Entity<DirectoryWindow>, &'a mut VisualTestContext) {
    let (view, cx) = open_window(fixture, cx);
    preview(cx, &view, &fixture.video_path);
    wait_for(cx, &view, "the first video frame", |view, cx| {
        view.media.read(cx).video_frame.is_some()
    });
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    (view, cx)
}

/// Notify only the media player, as a playback tick does.
fn notify_media(cx: &mut VisualTestContext, view: &Entity<DirectoryWindow>) {
    view.update(cx, |view, cx| view.media.update(cx, |_, cx| cx.notify()));
    cx.run_until_parked();
}

#[gpui::test]
fn player_updates_reuse_the_listing_and_sidebar(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = window_with_video(&fixture, cx);
    reset_stats(cx, &view);
    for _ in 0..5 {
        notify_media(cx, &view);
    }
    let stats = render_stats(cx, &view);
    assert_eq!(stats.root, 5, "the player's ancestors re-render");
    assert_eq!(stats.listing, 0);
    assert_eq!(stats.listing_rows, 0);
    assert_eq!(stats.sidebar, 0);
}

#[gpui::test]
fn window_notifications_rebuild_the_listing_and_sidebar(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = window_with_video(&fixture, cx);
    reset_stats(cx, &view);
    let third = view.update(cx, |view, cx| {
        let third = view.browser.visible_entries()[2].path.clone();
        view.browser.select(third.clone());
        cx.notify();
        third
    });
    cx.run_until_parked();
    let stats = render_stats(cx, &view);
    assert!(stats.listing >= 1 && stats.listing_rows > 0);
    assert!(stats.sidebar >= 1);
    // Debug bounds come from painting, so the rebuilt panes are on screen.
    assert!(cx.debug_bounds("listing-drop-surface").is_some());
    assert!(cx.debug_bounds("entry-2").is_some());
    assert!(cx.debug_bounds("sidebar").is_some());
    view.update(cx, |view, _| {
        assert_eq!(view.browser.selected_path(), Some(third.as_path()));
    });
}

#[gpui::test]
fn resizing_the_window_re_lays_out_the_panes(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = window_with_video(&fixture, cx);
    let listing = cx.debug_bounds("listing-drop-surface").unwrap();
    let sidebar = cx.debug_bounds("sidebar").unwrap();
    reset_stats(cx, &view);

    cx.simulate_resize(gpui::size(px(1200.0), px(760.0)));
    cx.run_until_parked();
    let resized_listing = cx.debug_bounds("listing-drop-surface").unwrap();
    let resized_sidebar = cx.debug_bounds("sidebar").unwrap();
    assert!(resized_listing.size.width < listing.size.width);
    assert!(resized_listing.size.height < listing.size.height);
    assert!(resized_sidebar.size.height < sidebar.size.height);
    assert_eq!(resized_sidebar.size.width, sidebar.size.width);
    let stats = render_stats(cx, &view);
    assert!(stats.listing >= 1 && stats.sidebar >= 1);

    // The file surface around the listing and the listing pane agree.
    let surface = cx.debug_bounds("file-surface").unwrap();
    assert_eq!(resized_listing.right(), surface.right());
    assert_eq!(resized_listing.bottom(), surface.bottom());
}

#[gpui::test]
fn scrolling_the_listing_re_renders_only_the_listing(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = window_with_video(&fixture, cx);
    let listing = cx.debug_bounds("listing-drop-surface").unwrap();
    reset_stats(cx, &view);
    cx.simulate_event(ScrollWheelEvent {
        position: listing.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-2_000.0))),
        ..Default::default()
    });
    cx.run_until_parked();
    let stats = render_stats(cx, &view);
    assert!(stats.listing >= 1 && stats.listing_rows > 0, "{stats:?}");
    assert_eq!(stats.sidebar, 0, "the sidebar stays cached");
    assert!(
        cx.debug_bounds("entry-0").is_none(),
        "scrolled past the top"
    );
    let offset = view.update(cx, |view, _| {
        view.listing.scroll_handle.0.borrow().base_handle.offset().y
    });
    assert!(offset < px(0.0));
}

#[gpui::test]
fn cached_panes_keep_handling_clicks_and_keys(cx: &mut TestAppContext) {
    cx.update(|cx| cx.bind_keys([KeyBinding::new("down", SelectNext, Some("browser"))]));
    let fixture = Fixture::new();
    let (view, cx) = window_with_video(&fixture, cx);
    let row = cx.debug_bounds("entry-3").unwrap();
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    cx.run_until_parked();
    // Frames drawn for the player alone reuse the listing's hitboxes,
    // listeners and key bindings.
    notify_media(cx, &view);
    notify_media(cx, &view);
    reset_stats(cx, &view);

    cx.simulate_click(row.center(), Modifiers::none());
    cx.run_until_parked();
    let (clicked, next) = view.update(cx, |view, _| {
        let entries = view.browser.visible_entries();
        (entries[3].path.clone(), entries[4].path.clone())
    });
    view.update(cx, |view, _| {
        assert_eq!(view.browser.selected_path(), Some(clicked.as_path()));
    });
    assert!(
        render_stats(cx, &view).listing >= 1,
        "the selection is redrawn"
    );

    notify_media(cx, &view);
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    view.update(cx, |view, _| {
        assert_eq!(view.browser.selected_path(), Some(next.as_path()));
    });
}

#[gpui::test]
fn changed_render_inputs_rebuild_the_panes_without_a_window_notification(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = window_with_video(&fixture, cx);
    reset_stats(cx, &view);
    // A change the window only picks up while rendering, like the system
    // switching between light and dark: the palette differs on the next
    // render even though nothing notified the window.
    view.update(cx, |view, _| {
        view.settings.appearance.theme = if view.palette.dark {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
    });
    notify_media(cx, &view);
    let stats = render_stats(cx, &view);
    assert_eq!(stats.listing, 1);
    assert_eq!(stats.sidebar, 1);
    // Back to caching once the panes have seen the new palette (the first
    // cached frame after an uncached one renders once more to record what
    // it can reuse).
    reset_stats(cx, &view);
    for _ in 0..4 {
        notify_media(cx, &view);
    }
    let stats = render_stats(cx, &view);
    assert_eq!((stats.listing, stats.sidebar), (1, 1));
}

#[gpui::test]
fn panes_render_uncached_while_an_accessibility_tree_is_built(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = window_with_video(&fixture, cx);
    reset_stats(cx, &view);
    // What the probe records when the previous frame built an accessibility
    // tree for an assistive client.
    view.update(cx, |view, _| view.panes.accessibility_seen.set(true));
    notify_media(cx, &view);
    let stats = render_stats(cx, &view);
    assert_eq!((stats.listing, stats.sidebar), (1, 1));
    // Tests have no assistive client, so the probe records nothing and the
    // panes are cached again.
    view.update(cx, |view, _| assert!(!view.panes.accessibility_seen.get()));
    reset_stats(cx, &view);
    for _ in 0..4 {
        notify_media(cx, &view);
    }
    let stats = render_stats(cx, &view);
    assert_eq!((stats.listing, stats.sidebar), (1, 1));
}
