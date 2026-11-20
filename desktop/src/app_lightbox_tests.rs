//! The Changelog's image lightbox over the window (`changelog-lightbox`):
//! a figure's `ChangelogEvent::OpenImage` opens it at the image's own size or
//! fit to the window, a resize re-fits it, and Esc or a click on the scrim,
//! the image or the close button closes it, the keys back on the Changelog.
//! The click on the figure itself is `views::markdown`'s and
//! `views::changelog`'s to test.

use std::cell::Cell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{px, size, Bounds, Pixels, TestAppContext};
use shelbi_app::changelog::ImageSource;

use super::smoke::Harness;
use super::*;
use crate::views::overlay::lightbox::{self, caption_room, Lightbox, LightboxImage, CAPTION_GAP, CLOSE_SIZE};

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

/// What a click on a scaled-down figure sends, on a page with no other
/// image.
fn open(h: &mut Harness, image: LightboxImage) {
    open_in(h, Lightbox::single(image));
}

/// What a click on a figure sends: the page's images, the clicked one up.
fn open_in(h: &mut Harness, lightbox: Lightbox) {
    let window = h.window.clone();
    h.cx.update(|win, cx| {
        let changelog = window.update(cx, |w, cx| w.ensure_changelog(win, cx));
        changelog.update(cx, |_, cx| cx.emit(ChangelogEvent::OpenImage(lightbox)));
    });
    h.draw();
}

/// The caption of the image the lightbox shows.
fn shown_caption(h: &Harness) -> Option<String> {
    h.window.read_with(&h.cx, |w, _| match &w.overlay {
        Some(ActiveOverlay::Lightbox(lb)) => Some(lb.current().caption.clone()),
        _ => None,
    })
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
/// window, the image and its caption centered down it, the close button
/// centered on its top-left corner and the caption under it, the image's
/// width.
fn assert_fitted(h: &mut Harness, window: (f32, f32), want: (f32, f32), what: &str) {
    let got = image_bounds(h);
    let fit = lightbox::fit(1200.0, 600.0, size(px(window.0), px(window.1)), true);
    assert_eq!((fit.width.as_f32(), fit.height.as_f32()), want, "{what}: the fit");
    assert!((got.size.width.as_f32() - want.0).abs() < 0.5, "{what}: width {:?}", got.size.width);
    assert!((got.size.height.as_f32() - want.1).abs() < 0.5, "{what}: height {:?}", got.size.height);
    assert!((got.center().x.as_f32() - window.0 / 2.0).abs() < 0.5, "{what}: centered across, {got:?}");
    let caption = h.cx.debug_bounds("lightbox-caption").expect("the caption");
    let top = (window.1 - (want.1 + caption_room(true))) / 2.0;
    assert!((got.top().as_f32() - top).abs() < 0.5, "{what}: centered down with its caption's room, {got:?}");
    assert!((caption.top().as_f32() - (got.bottom().as_f32() + CAPTION_GAP)).abs() < 0.5, "{what}: the caption under it");
    assert!((caption.size.width - got.size.width).abs().as_f32() < 0.5, "{what}: the caption is the image's width");
    assert_close_on_corner(h, got, window, what);
}

/// The close button: its center on the image's top-left corner, all of it
/// in the window.
fn assert_close_on_corner(h: &mut Harness, image: Bounds<Pixels>, window: (f32, f32), what: &str) {
    let close = h.cx.debug_bounds("lightbox-close").expect("the close button");
    assert!((close.size.width.as_f32() - CLOSE_SIZE).abs() < 0.5, "{what}: {close:?}");
    assert!((close.center().x - image.left()).abs().as_f32() < 0.5, "{what}: on the left edge, {close:?}");
    assert!((close.center().y - image.top()).abs().as_f32() < 0.5, "{what}: on the top edge, {close:?}");
    assert!(close.left().as_f32() >= 0.0 && close.top().as_f32() >= 0.0, "{what}: on screen, {close:?}");
    assert!(close.right().as_f32() <= window.0 && close.bottom().as_f32() <= window.1, "{what}: on screen, {close:?}");
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
    let room_h = 400.0 - 64.0 - caption_room(true);
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
fn the_caption_is_bare_text_under_the_image_and_absent_without_alt_text(cx: &mut TestAppContext) {
    let mut h = on_changelog(cx);
    resize(&mut h, 1000.0, 800.0);
    open(&mut h, wide_shot());
    let image = image_bounds(&mut h);
    let caption = h.cx.debug_bounds("lightbox-caption").expect("the caption");
    // One line: "The board after" fits the image's width.
    assert!((caption.size.height.as_f32() - 18.0).abs() < 0.5, "{caption:?}");
    assert!(caption.top() > image.bottom(), "{caption:?} under {image:?}");
    h.key("escape");

    // A long one wraps to the image's width, two lines at most.
    let long = "A caption long enough that it can't fit on one line under a narrow image, nor on two, \
                so it wraps and then ends in an ellipsis instead of running on and on down the window";
    open(&mut h, LightboxImage { width: 240.0, height: 160.0, caption: long.into(), ..wide_shot() });
    let image = image_bounds(&mut h);
    let caption = h.cx.debug_bounds("lightbox-caption").expect("the caption");
    assert!((caption.size.width - image.size.width).abs().as_f32() < 0.5, "{caption:?}");
    assert!((caption.size.height.as_f32() - 36.0).abs() < 0.5, "two lines: {caption:?}");
    h.key("escape");

    // No alt text: nothing under the image, which is centered alone.
    open(&mut h, LightboxImage { caption: String::new(), ..wide_shot() });
    assert!(is_open(&h));
    assert!(h.cx.debug_bounds("lightbox-caption").is_none());
    let image = image_bounds(&mut h);
    assert!((image.center().y.as_f32() - 400.0).abs() < 0.5, "{image:?}");
    assert_close_on_corner(&mut h, image, (1000.0, 800.0), "no caption");
}

#[gpui::test]
fn the_close_button_stays_on_screen_in_a_small_window(cx: &mut TestAppContext) {
    let mut h = on_changelog(cx);
    for window in [(480.0, 320.0), (360.0, 200.0)] {
        resize(&mut h, window.0, window.1);
        open(&mut h, wide_shot());
        let image = image_bounds(&mut h);
        assert_close_on_corner(&mut h, image, window, &format!("{window:?}"));
        h.click_selector("lightbox-close");
        h.draw();
        assert!(!is_open(&h), "{window:?}: the button closes it");
    }
}

#[gpui::test]
fn h_j_k_l_and_the_arrows_step_through_the_pages_images(cx: &mut TestAppContext) {
    let mut h = on_changelog(cx);
    resize(&mut h, 1000.0, 800.0);
    // Three images from different figures (a gallery's two, a lone one),
    // the middle one clicked; the last a different shape.
    let shot = |n: usize, (width, height): (f32, f32)| LightboxImage {
        source: ImageSource::File(PathBuf::from(format!("/nonexistent/{n}.png"))),
        width,
        height,
        caption: format!("Shot {n}"),
    };
    let images = vec![shot(0, (1200.0, 600.0)), shot(1, (600.0, 600.0)), shot(2, (400.0, 800.0))];
    open_in(&mut h, Lightbox::new(images, 1, shot(1, (600.0, 600.0))));
    assert_eq!(shown_caption(&h).as_deref(), Some("Shot 1"));

    let steps: [(&str, &str); 9] = [
        ("l", "Shot 2"),
        ("right", "Shot 2"), // the last stays
        ("h", "Shot 1"),
        ("k", "Shot 0"),
        ("up", "Shot 0"), // the first stays
        ("j", "Shot 1"),
        ("down", "Shot 2"),
        ("left", "Shot 1"),
        ("right", "Shot 2"),
    ];
    for (key, want) in steps {
        h.key(key);
        h.draw();
        assert!(is_open(&h), "{key}: still open");
        assert_eq!(shown_caption(&h).as_deref(), Some(want), "after {key}");
    }
    // The image redraws at the new one's fit, its button on its corner.
    let image = image_bounds(&mut h);
    let fit = lightbox::fit(400.0, 800.0, size(px(1000.0), px(800.0)), true);
    assert!((image.size.height - fit.height).abs().as_f32() < 0.5, "{image:?}");
    assert!((image.size.width - fit.width).abs().as_f32() < 0.5, "{image:?}");
    assert_close_on_corner(&mut h, image, (1000.0, 800.0), "after stepping");
    let caption = h.cx.debug_bounds("lightbox-caption").expect("its caption");
    assert!(caption.top() > image.bottom());

    h.key("escape");
    assert!(!is_open(&h), "Esc still closes it");
}

#[gpui::test]
fn the_image_grows_from_its_place_on_the_page(cx: &mut TestAppContext) {
    let mut h = on_changelog(cx);
    resize(&mut h, 1000.0, 800.0);
    // Motion on, on a clock run by hand.
    let now = Rc::new(Cell::new(Instant::now()));
    let clock = now.clone();
    h.window.update(&mut h.cx, |w, _| {
        w.reduce_motion = Rc::new(|| false);
        w.clock = Rc::new(move || clock.get());
    });
    let from = Bounds::new(gpui::point(px(300.0), px(500.0)), size(px(240.0), px(120.0)));
    let target = lightbox::place(1200.0, 600.0, size(px(1000.0), px(800.0)), true);
    open_in(&mut h, Lightbox::single(wide_shot()).growing_from(from));
    let near = |a: Bounds<Pixels>, b: Bounds<Pixels>| {
        (a.origin.x - b.origin.x).abs().as_f32() < 0.5
            && (a.origin.y - b.origin.y).abs().as_f32() < 0.5
            && (a.size.width - b.size.width).abs().as_f32() < 0.5
            && (a.size.height - b.size.height).abs().as_f32() < 0.5
    };
    // At the start: where the page drew it.
    let start = image_bounds(&mut h);
    assert!(near(start, from), "starts on the page: {start:?}");
    // Partway: between the two, and growing.
    now.set(now.get() + Duration::from_millis(120));
    let mid = image_bounds(&mut h);
    assert!(mid.size.width > from.size.width && mid.size.width < target.size.width, "{mid:?}");
    assert!(mid.origin.x < from.origin.x && mid.origin.x > target.origin.x, "{mid:?}");
    // Its close button rides its corner the whole way.
    let close = h.cx.debug_bounds("lightbox-close").unwrap();
    assert!((close.center().x - mid.left()).abs().as_f32() < 0.5 && (close.center().y - mid.top()).abs().as_f32() < 0.5);
    // Done: its own place.
    now.set(now.get() + crate::theme::motion::LIGHTBOX_GROW);
    let end = image_bounds(&mut h);
    assert!(near(end, target), "lands in the lightbox: {end:?} vs {target:?}");

    // Stepping to another image doesn't replay it.
    h.key("escape");
    let images = vec![wide_shot(), LightboxImage { caption: "Other".into(), ..wide_shot() }];
    open_in(&mut h, Lightbox::new(images, 0, wide_shot()).growing_from(from));
    h.key("l");
    let other = image_bounds(&mut h);
    assert!(near(other, target), "the next one is in place at once: {other:?}");
}
