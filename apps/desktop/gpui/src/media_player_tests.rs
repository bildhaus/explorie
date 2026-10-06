//! Input that reaches the media player through the window: Quick Look
//! shortcuts and pointer drags that leave the control they started on.

use gpui::{Modifiers, TestAppContext};

use super::render_perf_tests::{Fixture, open_window, preview, wait_for};
use super::*;

fn audio_position_ms(view: &DirectoryWindow, cx: &App) -> u64 {
    view.media
        .read(cx)
        .audio_status
        .as_ref()
        .expect("audio status")
        .position_ms
}

#[gpui::test]
fn quick_look_media_shortcuts_reach_the_player(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = open_window(&fixture, cx);
    let audio_path = fixture.audio_path.clone();
    view.update(cx, |view, cx| {
        view.browser.select(audio_path.clone());
        view.open_quick_look(audio_path.clone(), vec![audio_path.clone()], cx);
    });
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    wait_for(cx, &view, "the audio preview", |view, cx| {
        view.quick_look.open && view.media.read(cx).audio_status.is_some()
    });

    cx.simulate_keystrokes("k");
    view.update(cx, |view, cx| {
        assert!(
            view.media
                .read(cx)
                .audio_status
                .as_ref()
                .is_some_and(|status| status.playing),
            "K plays"
        );
    });
    cx.simulate_keystrokes("l");
    view.update(cx, |view, cx| {
        assert_eq!(audio_position_ms(view, cx), 10_000, "L skips ahead");
    });
    cx.simulate_keystrokes("end");
    view.update(cx, |view, cx| {
        assert_eq!(audio_position_ms(view, cx), 600_000, "End seeks to the end");
    });
    cx.simulate_keystrokes("m");
    view.update(cx, |view, cx| {
        let volume = view.media.read(cx).audio_status.as_ref().unwrap().volume;
        assert!(volume <= 0.01, "M mutes, volume is {volume}");
    });
}

#[gpui::test]
fn media_slider_drags_follow_the_pointer_across_the_window(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = open_window(&fixture, cx);
    preview(cx, &view, &fixture.audio_path);
    wait_for(cx, &view, "the audio preview", |view, cx| {
        view.media.read(cx).audio_status.is_some()
    });
    cx.run_until_parked();
    let slider = cx
        .debug_bounds("audio-seek")
        .expect("audio seek slider is shown");
    let window_width = cx.update(|window, _| window.viewport_size().width);
    let y = slider.center().y;

    // Press near the start of the slider...
    cx.simulate_mouse_down(
        point(slider.left() + px(4.0), y),
        MouseButton::Left,
        Modifiers::none(),
    );
    let start = view.update(cx, |view, cx| audio_position_ms(view, cx));
    assert!(
        start < 60_000,
        "pressing near the start seeks near the start"
    );

    // ...and drag past its end, off the player: the window forwards the move.
    let outside = point(window_width - px(2.0), y);
    cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, cx| {
        assert_eq!(
            audio_position_ms(view, cx),
            600_000,
            "dragging past the end"
        );
    });

    // Releasing anywhere ends the drag; later moves no longer seek.
    cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(point(slider.left() + px(4.0), y), None, Modifiers::none());
    view.update(cx, |view, cx| {
        assert_eq!(audio_position_ms(view, cx), 600_000, "the drag has ended");
    });
}

#[gpui::test]
fn video_state_changes_redraw_even_while_a_frame_converts(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = open_window(&fixture, cx);
    preview(cx, &view, &fixture.video_path);
    wait_for(cx, &view, "the first video frame", |view, cx| {
        view.media.read(cx).video_frame.is_some()
    });
    view.update(cx, |view, cx| {
        view.media
            .update(cx, |media, cx| media.toggle_video_playback(cx))
    });
    wait_for(cx, &view, "video playback", |view, cx| {
        view.media
            .read(cx)
            .video_status
            .as_ref()
            .is_some_and(|status| status.playing)
    });
    // The decoder stops on a new frame: the tick both starts converting it
    // and sees playback end. The conversion fails, so only the tick itself
    // can redraw the player as paused.
    fixture
        .video
        .stop_on_malformed_frame(Duration::from_millis(66));
    let renders = view.update(cx, |view, cx| view.media.read(cx).renders);
    cx.executor().advance_clock(Duration::from_millis(66));
    cx.run_until_parked();
    view.update(cx, |view, cx| {
        let media = view.media.read(cx);
        assert!(!media.video_status.as_ref().unwrap().playing);
        assert_eq!(media.renders, renders + 1);
    });
}
