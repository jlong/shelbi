//! A click on a sidebar nav row leaves the keys on what it opened
//! (`nav-click-focuses-content`): the native views take them on their next
//! frame, the one already shown included. The task panels' rows (a review, a
//! workspace with a task) and their views are `app_column_tests`'.

use gpui::TestAppContext;

use super::smoke::Harness;
use super::*;

fn window_has_keys(h: &mut Harness) -> bool {
    let window = h.window.clone();
    h.cx.update(|w, cx| window.read(cx).focus_handle.is_focused(w))
}

#[gpui::test]
fn a_click_on_a_nav_row_hands_the_keys_to_its_view(cx: &mut TestAppContext) {
    let mut h = Harness::open(cx);
    h.focus_window();
    // Commands run for real: the tap only records them.
    h.window.update(&mut h.cx, |w, _| w.tap = None);
    for (selector, view) in [
        ("sidebar-nav-Issues", View::Issues),
        ("sidebar-nav-Changelog", View::Changelog),
        // The view already shown.
        ("sidebar-nav-Changelog", View::Changelog),
        ("sidebar-nav-Issues", View::Issues),
    ] {
        // The keys start on the sidebar (the window's own handle).
        h.window.update(&mut h.cx, |w, _| w.client.focus_sidebar());
        h.focus_window();
        h.click_selector(selector);
        h.draw();
        assert_eq!(h.window.read_with(&h.cx, |w, _| w.client.view().clone()), view, "{selector}");
        assert_eq!(h.window.read_with(&h.cx, |w, _| w.client.focus()), Focus::Main, "{selector}");
        let in_view = h.cx.update(|window, cx| {
            let w = h.window.read(cx);
            match view {
                View::Issues => w.issues.as_ref().is_some_and(|v| v.read(cx).has_keys(window)),
                View::Changelog => w.changelog.as_ref().is_some_and(|v| v.read(cx).has_keys(window)),
                _ => unreachable!(),
            }
        });
        assert!(in_view, "{selector}: its view has the keys");
        assert!(!window_has_keys(&mut h), "{selector}: the sidebar doesn't");
    }
}
