//! Settings in its own sidebar (`dt-settings-sections`): opening it pushes
//! the settings sidebar into the leading column on the General page (or the
//! last page used), ⌘1 to ⌘3 and ↑ / ↓ plus Enter switch its pages while it
//! is open (and ⌘N keeps its sidebar meaning outside), and Back or Esc
//! return to the view it was opened over with the app sidebar back and its
//! selection on that view's row (`dt-back-syncs-sidebar`).
//!
//! The window is the smoke test's headless one ([`Harness`]) with an
//! injected clock for the push. The settings view reads no disk under test.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{Bounds, Pixels, TestAppContext};
use shelbi_app::view::ReviewState;

use super::smoke::Harness;
use super::*;
use crate::views::column_swap::SLIDE;
use crate::views::review::tests::{fake_deps, info, row};

struct Settings {
    h: Harness,
    now: Rc<Cell<Instant>>,
}

impl Settings {
    fn open(cx: &mut TestAppContext) -> Settings {
        cx.update(|cx| cx.set_global(TerminalSettings::default()));
        let mut h = Harness::open(cx);
        let now = Rc::new(Cell::new(Instant::now()));
        let n = now.clone();
        let (deps, _sessions, ops) = fake_deps();
        h.window.update(&mut h.cx, |w, _| {
            w.clock = Rc::new(move || n.get());
            w.reduce_motion = Rc::new(|| false);
            w.review_ops = ops;
            w.review_deps = Some(deps);
            w.model.reviews.push(row(ReviewState::Serving));
        });
        h.focus_window();
        h.take_calls();
        Settings { h, now }
    }

    fn update<R>(
        &mut self,
        f: impl FnOnce(&mut ShelbiWindow, &mut Window, &mut Context<ShelbiWindow>) -> R,
    ) -> R {
        let window = self.h.window.clone();
        let r = self
            .h
            .cx
            .update(|win, cx| window.update(cx, |w, cx| f(w, win, cx)));
        self.h.cx.run_until_parked();
        r
    }

    fn read<R>(&self, f: impl FnOnce(&ShelbiWindow, &App) -> R) -> R {
        self.h.window.read_with(&self.h.cx, f)
    }

    /// Open Settings as ⌘, does, and let the push run out.
    fn open_settings(&mut self) {
        self.update(|w, window, cx| w.open_settings(None, window, cx));
        self.settle();
    }

    /// Move the push's clock past its end and draw.
    fn settle(&mut self) {
        self.h.draw();
        self.now
            .set(self.now.get() + SLIDE + Duration::from_millis(1));
        self.h.draw();
    }

    fn key(&mut self, keystroke: &str) {
        self.h.key(keystroke);
        self.settle();
    }

    fn click(&mut self, selector: &'static str) {
        self.h.click_selector(selector);
        self.settle();
    }

    fn bounds(&mut self, selector: &'static str) -> Option<Bounds<Pixels>> {
        self.h.cx.debug_bounds(selector)
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        self.bounds(selector).is_some()
    }

    /// The page the settings view shows (and its sidebar has marked).
    fn page(&self) -> Option<SettingsPage> {
        self.read(|w, cx| match &w.route {
            Route::Settings(view) => Some(view.read(cx).page()),
            _ => None,
        })
    }

    /// The settings sidebar row the selection chip is on.
    fn selected_row(&mut self) -> Option<SettingsPage> {
        let at = self.bounds("settings-nav-selection")?.origin;
        settings_nav::pages().find(|p| self.bounds(p.selector()).is_some_and(|b| b.contains(&at)))
    }

    fn window_focused(&mut self) -> bool {
        let handle = self.read(|w, _| w.focus_handle.clone());
        self.h.cx.update(|window, _| handle.is_focused(window))
    }
}

#[gpui::test]
fn settings_takes_the_column_with_its_own_sidebar(cx: &mut TestAppContext) {
    let mut s = Settings::open(cx);
    let column_w = s.bounds("leading-column").expect("the column").size.width;
    s.open_settings();

    assert!(s.drawn("column-settings") && s.drawn("settings-sidebar"));
    assert!(!s.drawn("sidebar"), "the app sidebar is out of the column");
    assert_eq!(
        s.bounds("leading-column").expect("the column").size.width,
        column_w
    );
    for selector in [
        "settings-back",
        "settings-nav-general",
        "settings-nav-keybindings",
        "settings-nav-terminal",
    ] {
        assert!(s.drawn(selector), "{selector}");
    }
    // The frame's sidebar has no footer: no Zen pill, no settings gear.
    assert!(!s.drawn("sidebar-zen") && !s.drawn("sidebar-settings"));
    // Its titlebar keeps Search and Toggle sidebar.
    assert!(s.drawn("sidebar-search") && s.drawn("sidebar-collapse"));
    assert_eq!(s.page(), Some(SettingsPage::General));
    assert_eq!(s.selected_row(), Some(SettingsPage::General));
    assert!(s.drawn("settings-page-general"));
    assert!(s.window_focused(), "the settings sidebar has the keys");
}

#[gpui::test]
fn each_page_shows_its_controls(cx: &mut TestAppContext) {
    let mut s = Settings::open(cx);
    s.open_settings();
    assert!(s.drawn("settings-cli-path") && s.drawn("settings-cli-browse"));
    assert!(
        s.drawn("settings-theme-Auto") && s.drawn("settings-notify-A task is ready for review")
    );
    assert!(!s.drawn("settings-font-size") && !s.drawn("settings-open-keys"));

    s.key("cmd-2");
    assert!(s.drawn("settings-page-keybindings") && s.drawn("settings-open-keys"));
    assert!(!s.drawn("settings-cli-path") && !s.drawn("settings-font-size"));

    s.key("cmd-3");
    assert!(s.drawn("settings-page-terminal"));
    assert!(
        s.drawn("settings-font-size")
            && s.drawn("settings-font-size-up")
            && s.drawn("settings-option-Meta")
    );
    assert!(!s.drawn("settings-open-keys") && !s.drawn("settings-theme-Auto"));
}

#[gpui::test]
fn the_numbered_chords_switch_pages_inside_settings_only(cx: &mut TestAppContext) {
    let mut s = Settings::open(cx);
    s.open_settings();
    s.key("cmd-3");
    assert_eq!(
        (s.page(), s.selected_row()),
        (Some(SettingsPage::Terminal), Some(SettingsPage::Terminal))
    );
    s.key("cmd-2");
    assert_eq!(s.page(), Some(SettingsPage::Keybindings));
    s.key("cmd-9");
    assert_eq!(s.page(), Some(SettingsPage::Keybindings), "no page is ⌘9");
    assert!(s.h.take_calls().is_empty(), "no sidebar row opened");

    // Outside Settings, ⌘1 is the sidebar's first numbered row again.
    s.key("escape");
    s.h.take_calls();
    s.key("cmd-1");
    let mut calls = Vec::new();
    commands::dispatch_command(
        &CommandKind::LoadReview {
            task_id: "t-1".into(),
        },
        &mut calls,
    );
    assert_eq!(s.h.take_calls(), calls);
    assert_eq!(s.page(), None);
}

#[gpui::test]
fn up_down_and_enter_walk_the_settings_rows(cx: &mut TestAppContext) {
    let mut s = Settings::open(cx);
    s.open_settings();
    s.key("down");
    assert_eq!(
        s.selected_row(),
        Some(SettingsPage::Keybindings),
        "the selection moves"
    );
    assert_eq!(
        s.page(),
        Some(SettingsPage::General),
        "but opens nothing yet"
    );
    s.key("down");
    s.key("down");
    assert_eq!(
        s.selected_row(),
        Some(SettingsPage::Terminal),
        "it stops at the last row"
    );
    s.key("enter");
    assert_eq!(s.page(), Some(SettingsPage::Terminal));
    s.key("up");
    s.key("enter");
    assert_eq!(
        (s.page(), s.selected_row()),
        (
            Some(SettingsPage::Keybindings),
            Some(SettingsPage::Keybindings)
        )
    );
    assert!(
        s.h.take_calls().is_empty(),
        "the app sidebar's rows stay shut"
    );
}

#[gpui::test]
fn a_row_click_shows_its_page(cx: &mut TestAppContext) {
    let mut s = Settings::open(cx);
    s.open_settings();
    s.click("settings-nav-terminal");
    assert_eq!(
        (s.page(), s.selected_row()),
        (Some(SettingsPage::Terminal), Some(SettingsPage::Terminal))
    );
}

#[gpui::test]
fn back_and_esc_return_to_the_view_with_its_sidebar_row_selected(cx: &mut TestAppContext) {
    for leave in ["back", "escape"] {
        let mut s = Settings::open(cx);
        s.update(|w, _, _| w.client.set_view(View::Issues));
        s.h.draw();
        assert_eq!(
            s.read(|w, cx| w.selected(cx)),
            Some(SidebarTarget::View(View::Issues))
        );
        s.open_settings();
        assert_eq!(
            s.read(|w, cx| w.selected(cx)),
            None,
            "{leave}: no app row while Settings is up"
        );

        if leave == "back" {
            s.click("settings-back");
        } else {
            s.key("escape");
        }
        assert_eq!(
            s.h.take_calls(),
            vec![HostCall::ShowView(View::Issues)],
            "{leave}"
        );
        assert!(
            s.drawn("sidebar") && !s.drawn("column-settings"),
            "{leave}: the app sidebar is back"
        );
        assert_eq!(s.page(), None);
        assert_eq!(
            s.read(|w, cx| w.selected(cx)),
            Some(SidebarTarget::View(View::Issues)),
            "{leave}: its row is selected again"
        );
        assert_eq!(s.read(|w, _| w.client.focus()), Focus::Sidebar, "{leave}");
        assert!(s.window_focused(), "{leave}: the sidebar's keys");
        // The app sidebar's keys work again.
        s.key("down");
        assert_ne!(
            s.read(|w, cx| w.selected(cx)),
            Some(SidebarTarget::View(View::Issues)),
            "{leave}"
        );
    }
}

#[gpui::test]
fn settings_reopens_on_the_page_last_used(cx: &mut TestAppContext) {
    let mut s = Settings::open(cx);
    s.open_settings();
    s.key("cmd-3");
    s.key("escape");
    s.open_settings();
    assert_eq!(
        (s.page(), s.selected_row()),
        (Some(SettingsPage::Terminal), Some(SettingsPage::Terminal))
    );
}

#[gpui::test]
fn settings_over_a_review_returns_to_the_review(cx: &mut TestAppContext) {
    let mut s = Settings::open(cx);
    s.update(|w, window, cx| w.open_review(info(), window, cx));
    s.settle();
    assert!(s.drawn("column-review"));
    s.open_settings();
    assert!(s.drawn("column-settings") && !s.drawn("column-review"));
    s.key("escape");
    s.settle();
    assert!(
        s.drawn("column-review") && !s.drawn("column-settings"),
        "the review panel is back"
    );
    assert!(s.read(|w, _| matches!(w.route, Route::Review)));
    assert_eq!(s.read(|w, cx| w.shown_review(cx)), Some("t-1".into()));
}

#[gpui::test]
fn toggle_sidebar_collapses_the_settings_sidebar(cx: &mut TestAppContext) {
    let mut s = Settings::open(cx);
    s.open_settings();
    s.click("sidebar-collapse");
    assert!(!s.drawn("settings-sidebar") && !s.drawn("leading-column"));
    assert!(s.drawn("settings-page-general"), "the page stays");
    assert!(s.drawn("expand-sidebar"));
    s.click("expand-sidebar");
    assert!(s.drawn("settings-sidebar"));
}

#[gpui::test]
fn a_typed_letter_in_a_field_stays_the_fields(cx: &mut TestAppContext) {
    let mut s = Settings::open(cx);
    s.open_settings();
    s.click("settings-cli-path");
    assert!(!s.window_focused(), "the field has focus");
    s.key("j");
    assert_eq!(
        s.selected_row(),
        Some(SettingsPage::General),
        "j didn't move the settings selection"
    );
}
