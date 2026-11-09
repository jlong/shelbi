//! The window's floating toast (`editor-launch-toast`): the Workspace and
//! Review views put one up (`Launching Zed…`) and the window paints it over
//! the main area's top-right corner. It never takes the keys, takes itself
//! down after its lifetime, and a newer one (a failed launch's error)
//! replaces it with a lifetime of its own.

use std::time::Duration;

use gpui::TestAppContext;
use shelbi_app::toast::{Toast, ERROR_TOAST_LIFETIME, TOAST_LIFETIME};

use super::smoke::Harness;

fn toast_drawn(h: &mut Harness) -> Option<gpui::Bounds<gpui::Pixels>> {
    h.draw();
    h.cx.debug_bounds("toast")
}

fn advance(h: &mut Harness, by: Duration) {
    h.cx.executor().advance_clock(by);
    h.cx.run_until_parked();
}

#[gpui::test]
fn a_toast_floats_top_right_and_takes_itself_down(cx: &mut TestAppContext) {
    let mut h = Harness::open(cx);
    h.focus_window();
    let has_keys = |h: &mut Harness| {
        let window = h.window.clone();
        h.cx.update(|w, cx| window.read(cx).focus_handle.is_focused(w))
    };
    assert!(has_keys(&mut h));
    let focus = h.window.read_with(&h.cx, |w, _| w.client.focus());

    h.window.update(&mut h.cx, |w, cx| w.show_toast(Some(Toast::launching_editor("Zed")), cx));
    let bounds = toast_drawn(&mut h).expect("the toast is drawn at once");
    let viewport = h.cx.update(|w, _| w.viewport_size());
    assert!(
        bounds.right() < viewport.width && bounds.right() > viewport.width - gpui::px(40.0),
        "it sits by the main area's right edge: {bounds:?} in {viewport:?}"
    );
    assert!(bounds.top() < gpui::px(80.0), "and near its top: {bounds:?}");
    // Painted only: the keys and the focus model stay where they were.
    assert!(has_keys(&mut h), "the toast never takes the keys");
    assert_eq!(h.window.read_with(&h.cx, |w, _| w.client.focus()), focus);

    advance(&mut h, TOAST_LIFETIME - Duration::from_millis(100));
    assert!(toast_drawn(&mut h).is_some(), "still up inside its lifetime");
    advance(&mut h, Duration::from_millis(100));
    assert!(toast_drawn(&mut h).is_none(), "gone after its lifetime");
}

#[gpui::test]
fn a_failed_launch_replaces_the_toast_and_outlives_its_timer(cx: &mut TestAppContext) {
    let mut h = Harness::open(cx);
    h.window.update(&mut h.cx, |w, cx| w.show_toast(Some(Toast::launching_editor("Zed")), cx));
    advance(&mut h, Duration::from_secs(1));
    let failed = Toast::editor_launch_failed("Zed", "Unable to find application named 'Zed'");
    h.window.update(&mut h.cx, |w, cx| w.show_toast(Some(failed.clone()), cx));
    assert_eq!(h.window.read_with(&h.cx, |w, _| w.toast_showing()), Some(failed.clone()));

    // The launching toast's timer fires; the error stays.
    advance(&mut h, TOAST_LIFETIME);
    assert!(toast_drawn(&mut h).is_some());
    assert_eq!(h.window.read_with(&h.cx, |w, _| w.toast_showing()), Some(failed));
    advance(&mut h, ERROR_TOAST_LIFETIME);
    assert!(toast_drawn(&mut h).is_none());
}

#[gpui::test]
fn none_takes_the_toast_down_at_once(cx: &mut TestAppContext) {
    let mut h = Harness::open(cx);
    h.window.update(&mut h.cx, |w, cx| w.show_toast(Some(Toast::launching_editor("Zed")), cx));
    assert!(toast_drawn(&mut h).is_some());
    h.window.update(&mut h.cx, |w, cx| w.show_toast(None, cx));
    assert!(toast_drawn(&mut h).is_none(), "a terminal editor never sits under it");
}
