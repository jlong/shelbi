//! Where the notices draw: the window's relaunch and remote bars along its
//! foot, under the sidebar and the main area, with the project tabs the top
//! row; the main area's own notices (the banner here) along the main area's
//! foot, under its screen; and the banner still dismisses from there.
//!
//! The window is the smoke test's headless one ([`Harness`]).

use gpui::TestAppContext;

use super::smoke::Harness;
use super::*;
use crate::platform::RelaunchNotice;

#[gpui::test]
fn the_notices_sit_along_the_foot_and_the_tabs_stay_on_top(cx: &mut TestAppContext) {
    let mut h = Harness::open(cx);
    h.window.update(&mut h.cx, |w, cx| {
        let control = &w.ready().expect("ready").control;
        assert!(control.require_relaunch(RelaunchNotice {
            app_version: "0.9.0".into(),
            daemon_version: Some("0.9.2".into()),
            reason: "The daemon was upgraded.".into(),
        }));
        w.set_remote_bar(Some(RemoteBar::Stopped("Shelbi isn't running on devbox.".into())));
        w.banner = Some("Couldn't save the workflow.".into());
        cx.notify();
    });
    h.draw();
    let window = h.cx.update(|window, _| window.viewport_size());
    let bounds = |h: &mut Harness, selector: &'static str| {
        h.cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
    };
    let tabs = bounds(&mut h, "tab-bar");
    let sidebar = bounds(&mut h, "sidebar");
    let relaunch = bounds(&mut h, "relaunch-bar");
    let remote = bounds(&mut h, "remote-stopped-bar");
    let banner = bounds(&mut h, "banner");

    // The tabs are the window's top row.
    assert_eq!(tabs.top(), gpui::px(0.0), "the tab bar is the top row: {tabs:?}");
    // The window's bars span its width along the foot, under the sidebar.
    for (what, bar) in [("relaunch", relaunch), ("remote", remote)] {
        assert_eq!(bar.left(), gpui::px(0.0), "{what}: full width");
        assert_eq!(bar.size.width, window.width, "{what}: full width");
        assert!(bar.top() >= sidebar.bottom(), "{what} is under the sidebar: {bar:?} / {sidebar:?}");
    }
    assert_eq!(relaunch.bottom(), remote.top(), "the remote bar follows the relaunch bar");
    assert_eq!(remote.bottom(), window.height, "the bars end at the window's foot");
    // The banner sits at the main area's foot, just over the window's bars.
    assert!(banner.left() >= sidebar.right(), "the banner is in the main area: {banner:?}");
    assert!(banner.top() > tabs.bottom());
    assert_eq!(banner.bottom(), relaunch.top(), "the banner is the main area's foot");

    // The banner still dismisses from there.
    h.click_selector("dismiss-banner");
    h.draw();
    assert!(h.window.read_with(&h.cx, |w, _| w.banner.is_none()), "dismissed");
    assert!(h.cx.debug_bounds("banner").is_none());
}
