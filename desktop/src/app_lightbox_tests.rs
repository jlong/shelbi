//! The Changelog's image lightbox over the window (`changelog-lightbox`):
//! a figure's `ChangelogEvent::OpenImage` opens it at the image's own size or
//! fit to the window, a resize re-fits it, and Esc or a click on the scrim,
//! the image or the close button closes it, the keys back on the Changelog.
//! The click on the figure itself is `views::markdown`'s and
//! `views::changelog`'s to test.

use std::path::PathBuf;

use gpui::{px, size, Bounds, Pixels, TestAppContext};
use shelbi_app::changelog::ImageSource;

use super::smoke::Harness;
use super::*;
use crate::views::overlay::lightbox::{self, LightboxImage, FOOTER_GAP, FOOTER_H};

fn wide_shot() -> LightboxImage {
    LightboxImage {
        source: ImageSource::File(PathBuf::from("/nonexistent/wide.png")),
        width: 1200.0,
        height: 600.0,
        caption: "The board after".into(),
    }
}

/// The harness on the Changelog, its keys there, entrances drawn in place.
fn on_changelog(cx: &mut TestAppContext) -> Harness {
    let mut h = Harness::open(cx);
    h.window.update(&mut h.cx, |w, _| {
        w.tap = None;
        w.reduce_motion = Rc::new(|| true);
    });
    h.click_selector("sidebar-nav-Changelog");
    h.draw();
    assert!(changelog_has_keys(&mut h), "the Changelog took the keys");
    h
}

fn resize(h: &mut Harness, w: f32, ht: f32) {
    h.cx.simulate_resize(size(px(w), px(ht)));
    h.draw();
}

/// What a click on a scaled-down figure sends.
fn open(h: &mut Harness, image: LightboxImage) {
    let window = h.window.clone();
    h.cx.update(|win, cx| {
        let changelog = window.update(cx, |w, cx| w.ensure_changelog(win, cx));
        changelog.update(cx, |_, cx| cx.emit(ChangelogEvent::OpenImage(image)));
    });
    h.draw();
}

fn is_open(h: &Harness) -> bool {
    h.window.read_with(&h.cx, |w, _| {
        matches!(w.overlay, Some(ActiveOverlay::Lightbox(_))) && w.client.overlay() == Some(&shelbi_app::nav::Overlay::Lightbox)
    })
}

fn changelog_has_keys(h: &mut Harness) -> bool {
    let window = h.window.clone();
    h.cx.update(|win, cx| window.read(cx).changelog.as_ref().is_some_and(|v| v.read(cx).has_keys(win)))
}

fn image_bounds(h: &mut Harness) -> Bounds<Pixels> {
    h.draw();
    h.cx.debug_bounds("lightbox-image").expect("the lightbox's image is drawn")
}

/// The image is drawn at [`lightbox::fit`]'s size, centered across the
/// window, the image and its footer centered down it.
fn assert_fitted(h: &mut Harness, window: (f32, f32), want: (f32, f32), what: &str) {
    let got = image_bounds(h);
    let fit = lightbox::fit(1200.0, 600.0, size(px(window.0), px(window.1)));
    assert_eq!((fit.width.as_f32(), fit.height.as_f32()), want, "{what}: the fit");
    assert!((got.size.width.as_f32() - want.0).abs() < 0.5, "{what}: width {:?}", got.size.width);
    assert!((got.size.height.as_f32() - want.1).abs() < 0.5, "{what}: height {:?}", got.size.height);
    assert!((got.center().x.as_f32() - window.0 / 2.0).abs() < 0.5, "{what}: centered across, {got:?}");
    let top = (window.1 - (want.1 + FOOTER_GAP + FOOTER_H)) / 2.0;
    assert!((got.top().as_f32() - top).abs() < 0.5, "{what}: centered down with its footer, {got:?}");
    let close = h.cx.debug_bounds("lightbox-close").expect("the close button");
    assert!((close.top().as_f32() - (got.bottom().as_f32() + FOOTER_GAP)).abs() < 0.5, "{what}: the footer under it");
    assert!(h.cx.debug_bounds("lightbox-caption").is_some(), "{what}: the caption");
}

#[gpui::test]
fn a_wide_image_opens_at_its_own_size_or_fit_to_the_window(cx: &mut TestAppContext) {
    let mut h = on_changelog(cx);
    // A large window: room for all of it.
    resize(&mut h, 1600.0, 1200.0);
    open(&mut h, wide_shot());
    assert!(is_open(&h), "the lightbox opened");
    assert_fitted(&mut h, (1600.0, 1200.0), (1200.0, 600.0), "large window");
    h.key("escape");
    assert!(!is_open(&h));

    // A small window: width-bound, 32pt either side.
    resize(&mut h, 800.0, 600.0);
    open(&mut h, wide_shot());
    assert_fitted(&mut h, (800.0, 600.0), (736.0, 368.0), "small window");

    // Resized while open, it re-fits: short and wide is height-bound.
    resize(&mut h, 1600.0, 400.0);
    let room_h = 400.0 - 64.0 - FOOTER_GAP - FOOTER_H;
    assert_fitted(&mut h, (1600.0, 400.0), (room_h * 2.0, room_h), "short window");
    resize(&mut h, 1600.0, 1200.0);
    assert_fitted(&mut h, (1600.0, 1200.0), (1200.0, 600.0), "grown back");
}

/// One way of closing the lightbox.
type CloseWay = fn(&mut Harness);

#[gpui::test]
fn esc_and_a_click_on_the_scrim_the_image_or_the_close_button_close_it(cx: &mut TestAppContext) {
    let mut h = on_changelog(cx);
    resize(&mut h, 1000.0, 800.0);
    let ways: [(&str, CloseWay); 4] = [
        ("esc", |h| h.key("escape")),
        ("the scrim", |h| {
            let image = image_bounds(h);
            // Beside the image, on the scrim.
            h.cx.simulate_click(gpui::point(image.left() / 2.0, image.center().y), gpui::Modifiers::none());
            h.cx.run_until_parked();
        }),
        ("the image", |h| {
            let at = image_bounds(h).center();
            h.cx.simulate_click(at, gpui::Modifiers::none());
            h.cx.run_until_parked();
        }),
        ("the close button", |h| h.click_selector("lightbox-close")),
    ];
    for (what, close) in ways {
        open(&mut h, wide_shot());
        assert!(is_open(&h), "{what}: open");
        assert!(!changelog_has_keys(&mut h), "{what}: the lightbox has the keys while open");
        close(&mut h);
        h.draw();
        assert!(!is_open(&h), "{what}: closed it");
        assert!(h.cx.debug_bounds("lightbox-image").is_none(), "{what}: nothing left drawn");
        assert_eq!(h.window.read_with(&h.cx, |w, _| w.client.focus()), Focus::Main, "{what}");
        assert!(changelog_has_keys(&mut h), "{what}: the keys are back on the Changelog");
    }
    // Other keys leave it open.
    open(&mut h, wide_shot());
    h.key("down");
    h.key("space");
    assert!(is_open(&h), "only Esc closes it from the keyboard");
}

#[gpui::test]
fn an_image_with_no_caption_has_only_the_close_button(cx: &mut TestAppContext) {
    let mut h = on_changelog(cx);
    open(&mut h, LightboxImage { caption: String::new(), ..wide_shot() });
    assert!(is_open(&h));
    assert!(h.cx.debug_bounds("lightbox-caption").is_none());
    assert!(h.cx.debug_bounds("lightbox-close").is_some());
}
