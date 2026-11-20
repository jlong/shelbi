//! The Changelog's image menu over the window (`changelog-image-download`):
//! a figure's `ChangelogEvent::ImageMenu` or a right-click on the lightbox's
//! image opens it at the pointer; Esc or a press outside closes it and does
//! nothing else; Download Image… asks the save panel (stubbed here) for a
//! path in `~/Downloads` under the image's name and writes its bytes there,
//! a failure in the toast. The right-click on a figure itself is
//! `views::markdown`'s and `views::changelog`'s to test.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use gpui::{point, px, size, Pixels, Point, TestAppContext};
use shelbi_app::changelog::ImageSource;

use super::smoke::Harness;
use super::*;
use crate::views::image_menu::{self, DOWNLOAD_ROW, MENU};
use crate::views::overlay::lightbox::LightboxImage;

/// The harness on the Changelog, its keys there, entrances drawn in place.
fn on_changelog(cx: &mut TestAppContext) -> Harness {
    let mut h = Harness::open(cx);
    h.window.update(&mut h.cx, |w, _| {
        w.tap = None;
        w.reduce_motion = Rc::new(|| true);
    });
    h.cx.simulate_resize(size(px(1200.0), px(900.0)));
    h.click_selector("sidebar-nav-Changelog");
    h.draw();
    h
}

/// A scratch folder of this test's own.
fn scratch(tag: u32) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shbimg-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// What a right-click on a figure sends.
fn right_click_figure(h: &mut Harness, source: ImageSource, at: Point<Pixels>) {
    let window = h.window.clone();
    h.cx.update(|win, cx| {
        let changelog = window.update(cx, |w, cx| w.ensure_changelog(win, cx));
        changelog.update(cx, |_, cx| cx.emit(ChangelogEvent::ImageMenu { source, at }));
    });
    h.draw();
}

fn menu_source(h: &Harness) -> Option<ImageSource> {
    h.window.read_with(&h.cx, |w, _| w.image_menu.as_ref().map(|m| m.source.clone()))
}

fn lightbox_open(h: &Harness) -> bool {
    h.window.read_with(&h.cx, |w, _| matches!(w.overlay, Some(ActiveOverlay::Lightbox(_))))
}

fn toast(h: &Harness) -> Option<shelbi_app::toast::Toast> {
    h.window.read_with(&h.cx, |w, _| w.toast_showing())
}

/// The menu is drawn just under and right of `at`, with its one row.
fn assert_at_pointer(h: &mut Harness, at: Point<Pixels>, what: &str) {
    h.draw();
    let menu = h.cx.debug_bounds(MENU).unwrap_or_else(|| panic!("{what}: the menu is drawn"));
    assert!((menu.left() - at.x).abs() < px(0.5), "{what}: at the pointer's x, {menu:?} for {at:?}");
    let gap = menu.top() - at.y;
    assert!(gap >= px(0.0) && gap <= px(4.0), "{what}: just under the pointer, {menu:?} for {at:?}");
    assert!(h.cx.debug_bounds(DOWNLOAD_ROW).is_some(), "{what}: Download Image… is its row");
}

/// The save panel the window asks, answering `answer`, and what it was
/// asked (the folder and the name it offered).
type Asked = Rc<RefCell<Vec<(PathBuf, String)>>>;

fn stub_save_panel(h: &mut Harness, answer: Result<Option<PathBuf>, String>) -> Asked {
    let asked: Asked = Default::default();
    let seen = asked.clone();
    h.window.update(&mut h.cx, |w, _| {
        w.save_panel = Rc::new(move |dir: &Path, name: &str, _: &mut App| {
            seen.borrow_mut().push((dir.to_path_buf(), name.to_string()));
            let answer = answer.clone();
            Box::pin(async move { answer })
        });
    });
    asked
}

fn choose_download(h: &mut Harness) {
    h.click_selector(DOWNLOAD_ROW);
    h.draw();
}

#[gpui::test]
fn a_right_click_on_a_figure_opens_the_menu_at_the_pointer(cx: &mut TestAppContext) {
    let mut h = on_changelog(cx);
    let source = ImageSource::File(PathBuf::from("/nonexistent/shot.png"));
    let at = point(px(300.0), px(240.0));
    right_click_figure(&mut h, source.clone(), at);
    assert_eq!(menu_source(&h), Some(source));
    assert_at_pointer(&mut h, at, "on a figure");
    assert!(!lightbox_open(&h), "a right-click never opens the lightbox");
}

#[gpui::test]
fn a_right_click_on_the_lightbox_image_opens_the_menu_over_it(cx: &mut TestAppContext) {
    let mut h = on_changelog(cx);
    let source = ImageSource::File(PathBuf::from("/nonexistent/wide.png"));
    let image = LightboxImage { source: source.clone(), width: 800.0, height: 400.0, caption: "Wide".into() };
    let window = h.window.clone();
    h.cx.update(|win, cx| window.update(cx, |w, cx| w.open_lightbox(crate::views::overlay::lightbox::Lightbox::single(image), win, cx)));
    h.draw();
    assert!(lightbox_open(&h));
    let at = h.cx.debug_bounds("lightbox-image").expect("the lightbox's image").center();
    h.cx.simulate_mouse_down(at, gpui::MouseButton::Right, gpui::Modifiers::none());
    h.cx.simulate_mouse_up(at, gpui::MouseButton::Right, gpui::Modifiers::none());
    h.cx.run_until_parked();
    assert_eq!(menu_source(&h), Some(source));
    assert_at_pointer(&mut h, at, "in the lightbox");
    assert!(lightbox_open(&h), "the lightbox stays open under it");

    // Esc closes the menu only; the next closes the lightbox.
    h.key("escape");
    assert_eq!(menu_source(&h), None);
    assert!(h.cx.debug_bounds(MENU).is_none());
    assert!(lightbox_open(&h), "Esc closed only the menu");
    h.key("escape");
    assert!(!lightbox_open(&h));

    // A press outside it closes it, and goes no further: the lightbox's
    // own close-on-click doesn't fire.
    let image = LightboxImage { source: ImageSource::File("/nonexistent/wide.png".into()), width: 800.0, height: 400.0, caption: String::new() };
    h.cx.update(|win, cx| window.update(cx, |w, cx| w.open_lightbox(crate::views::overlay::lightbox::Lightbox::single(image), win, cx)));
    h.draw();
    let at = h.cx.debug_bounds("lightbox-image").unwrap().center();
    h.cx.simulate_mouse_down(at, gpui::MouseButton::Right, gpui::Modifiers::none());
    h.cx.simulate_mouse_up(at, gpui::MouseButton::Right, gpui::Modifiers::none());
    h.draw();
    assert!(menu_source(&h).is_some());
    h.cx.simulate_click(point(at.x - px(200.0), at.y - px(100.0)), gpui::Modifiers::none());
    h.draw();
    assert_eq!(menu_source(&h), None, "a press outside closed it");
    assert!(lightbox_open(&h), "and did nothing else");
}

#[gpui::test]
fn download_saves_an_identical_copy_of_a_local_image(cx: &mut TestAppContext) {
    let mut h = on_changelog(cx);
    let dir = scratch(line!());
    let file = dir.join("board after.png");
    let bytes: Vec<u8> = (0..=255u8).cycle().take(3000).collect();
    std::fs::write(&file, &bytes).unwrap();
    let dest = dir.join("saved.png");
    let asked = stub_save_panel(&mut h, Ok(Some(dest.clone())));
    right_click_figure(&mut h, ImageSource::File(file.clone()), point(px(300.0), px(240.0)));
    choose_download(&mut h);
    assert_eq!(*asked.borrow(), [(image_menu::downloads_dir(), "board after.png".to_string())]);
    assert!(image_menu::downloads_dir().ends_with("Downloads"));
    assert_eq!(menu_source(&h), None, "the menu closed");
    assert_eq!(std::fs::read(&dest).unwrap(), bytes, "an identical copy");
    assert_eq!(std::fs::read(&file).unwrap(), bytes, "the original untouched");
    assert!(toast(&h).is_none(), "no complaint");
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui::test]
fn download_saves_the_downloaded_bytes_of_a_web_image(cx: &mut TestAppContext) {
    let mut h = on_changelog(cx);
    let dir = scratch(line!());
    let bytes: Vec<u8> = (0..4000u32).map(|i| (i * 7 % 251) as u8).collect();
    let served = bytes.clone();
    let requested = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log = requested.clone();
    h.cx.update(|_, cx| {
        cx.set_http_client(gpui::http_client::FakeHttpClient::create(move |req| {
            log.lock().unwrap().push(req.uri().to_string());
            let body = served.clone();
            async move { Ok(gpui::http_client::Response::builder().status(200).body(body.into()).unwrap()) }
        }))
    });
    let dest = dir.join("from-web.png");
    let asked = stub_save_panel(&mut h, Ok(Some(dest.clone())));
    let url = "https://example.com/shots/the%20board.png?v=2";
    right_click_figure(&mut h, ImageSource::Web(url.into()), point(px(300.0), px(240.0)));
    choose_download(&mut h);
    assert_eq!(asked.borrow()[0].1, "the board.png");
    assert_eq!(*requested.lock().unwrap(), [url.to_string()]);
    assert_eq!(std::fs::read(&dest).unwrap(), bytes, "the downloaded bytes, unchanged");
    assert!(toast(&h).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui::test]
fn cancel_writes_nothing_and_a_failure_is_reported(cx: &mut TestAppContext) {
    let mut h = on_changelog(cx);
    let dir = scratch(line!());
    let file = dir.join("a.png");
    std::fs::write(&file, b"png").unwrap();

    // Cancel: no file, no toast.
    stub_save_panel(&mut h, Ok(None));
    right_click_figure(&mut h, ImageSource::File(file.clone()), point(px(300.0), px(240.0)));
    choose_download(&mut h);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "nothing written");
    assert!(toast(&h).is_none());
    assert_eq!(menu_source(&h), None);

    // A folder that isn't there: the write fails, and says so.
    stub_save_panel(&mut h, Ok(Some(dir.join("gone/a.png"))));
    right_click_figure(&mut h, ImageSource::File(file.clone()), point(px(300.0), px(240.0)));
    choose_download(&mut h);
    let shown = toast(&h).expect("the failure is in the toast");
    assert_eq!(shown.tone, shelbi_app::toast::ToastTone::Error);
    assert!(shown.text.starts_with("Couldn\u{2019}t save a.png"), "{}", shown.text);

    // A web image the server refuses.
    h.cx.update(|_, cx| cx.set_http_client(gpui::http_client::FakeHttpClient::with_404_response()));
    stub_save_panel(&mut h, Ok(Some(dir.join("web.png"))));
    right_click_figure(&mut h, ImageSource::Web("https://example.com/web.png".into()), point(px(300.0), px(240.0)));
    choose_download(&mut h);
    assert!(!dir.join("web.png").exists(), "nothing written for a refused download");
    assert!(toast(&h).expect("reported").text.contains("404"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui::test]
fn an_image_whose_bytes_arent_here_shows_the_row_dimmed(cx: &mut TestAppContext) {
    let mut h = on_changelog(cx);
    let asked = stub_save_panel(&mut h, Ok(Some(PathBuf::from("/nonexistent/x.png"))));
    right_click_figure(&mut h, ImageSource::Unavailable, point(px(300.0), px(240.0)));
    assert!(h.cx.debug_bounds(DOWNLOAD_ROW).is_some(), "the row is there");
    h.click_selector(DOWNLOAD_ROW);
    assert!(asked.borrow().is_empty(), "a dimmed row asks nothing");
    assert!(menu_source(&h).is_some(), "and stays open");
}
