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

use gpui::{px, Bounds, Pixels, TestAppContext};
use shelbi_app::view::ReviewState;

use super::smoke::Harness;
use super::*;
use crate::elements::controls::Slider;
use crate::views::column_swap::SLIDE;
use crate::views::sidebar_reveal::REVEAL;
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

    /// Move the clock past the push's end (and a sidebar hide or show's)
    /// and draw.
    fn settle(&mut self) {
        self.h.draw();
        self.now
            .set(self.now.get() + SLIDE.max(REVEAL) + Duration::from_millis(1));
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
    let full = s.bounds("settings-sidebar").expect("drawn").size.width;
    // It slides out like the app's sidebar: clipped at its full width.
    s.h.click_selector("sidebar-collapse");
    s.now.set(s.now.get() + Duration::from_millis(30));
    s.h.draw();
    let column = s.bounds("leading-column").expect("still going").size.width;
    let slid = s.bounds("settings-sidebar").expect("still drawn");
    assert!(column > px(0.0) && column < full, "{column:?}");
    assert_eq!(slid.size.width, full, "never laid out narrower");
    assert!(slid.origin.x < px(0.0), "slid left");
    s.settle();
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

/// The Terminal page's view, with `config.yaml` "read" as `cfg`.
fn terminal_page(s: &mut Settings, cfg: shelbi_state::UserConfig) -> Entity<SettingsView> {
    s.key("cmd-3");
    let view = s.read(|w, _| match &w.route {
        Route::Settings(view) => view.clone(),
        _ => panic!("settings is open"),
    });
    s.update(|_, window, cx| view.update(cx, |v, cx| v.load_config(cfg, window, cx)));
    s.settle();
    view
}

fn live_ansi(s: &mut Settings) -> crate::elements::terminal::colors::AnsiOverrides {
    s.h.cx.update(|_, cx| TerminalSettings::current(cx).ansi)
}

#[gpui::test]
fn the_terminal_page_edits_colors_live_saves_them_and_resets(cx: &mut TestAppContext) {
    use shelbi_term::render::Rgb;
    let mut s = Settings::open(cx);
    s.open_settings();
    let view = terminal_page(&mut s, shelbi_state::UserConfig::default());
    for selector in [
        "settings-ansi-black-regular",
        "settings-ansi-white-bold",
        "settings-ansi-reset",
        "settings-terminal-preview",
        "settings-option-Meta",
    ] {
        assert!(s.drawn(selector), "{selector}");
    }
    let default_red = s.read(|_, cx| cx.global::<crate::theme::Theme>().terminal.ansi[1]);
    let text = |s: &mut Settings, slot| s.read(|_, cx| view.read(cx).color_text(slot, cx));
    assert_eq!(text(&mut s, 1), crate::elements::terminal::colors::hex(default_red));
    assert!(!s.read(|_, cx| view.read(cx).can_reset()), "nothing to reset");

    // A color applies to every terminal at once and is saved, as the table
    // writes it.
    s.update(|_, window, cx| view.update(cx, |v, cx| v.type_color(1, "123abc", window, cx)));
    assert_eq!(live_ansi(&mut s)[1], Some(Rgb::hex(0x123abc)));
    assert_eq!(text(&mut s, 1), "#123ABC");
    let saved = s.read(|_, cx| view.read(cx).saved().desktop.terminal_colors.clone());
    assert_eq!(saved.red.regular.as_deref(), Some("#123ABC"));
    assert!(s.read(|_, cx| view.read(cx).can_reset()));
    // The preview pane draws with it.
    let preview = s.read(|_, cx| view.read(cx).preview());
    assert_eq!(s.read(|_, cx| preview.read(cx).settings.ansi[1]), Some(Rgb::hex(0x123abc)));

    // Text that isn't a color marks the field and changes nothing.
    s.update(|_, window, cx| view.update(cx, |v, cx| v.type_color(9, "#12345", window, cx)));
    s.settle();
    assert!(s.drawn("settings-ansi-invalid"));
    assert_eq!(text(&mut s, 9), "#12345", "the text stays for the user to fix");
    assert_eq!(live_ansi(&mut s)[9], None);
    let saved_after = s.read(|_, cx| view.read(cx).saved().desktop.terminal_colors.clone());
    assert_eq!(saved_after, saved, "not saved");

    // The theme's own color isn't the user's: nothing is written for it.
    let default_green = s.read(|_, cx| cx.global::<crate::theme::Theme>().terminal.ansi[2]);
    let hex = crate::elements::terminal::colors::hex(default_green).to_lowercase();
    s.update(|_, window, cx| view.update(cx, |v, cx| v.type_color(2, &hex, window, cx)));
    assert_eq!(live_ansi(&mut s)[2], None);
    let saved = s.read(|_, cx| view.read(cx).saved().desktop.terminal_colors.clone());
    assert!(saved.green.is_empty());

    // Reset puts every color back and clears the mark.
    s.click("settings-ansi-reset");
    assert_eq!(live_ansi(&mut s), [None; 16]);
    assert!(s.read(|_, cx| view.read(cx).saved().desktop.terminal_colors.is_empty()));
    assert_eq!(text(&mut s, 1), crate::elements::terminal::colors::hex(default_red));
    assert!(!s.drawn("settings-ansi-invalid"));
    assert!(!s.read(|_, cx| view.read(cx).can_reset()));
}

#[gpui::test]
fn the_terminal_page_shows_saved_colors(cx: &mut TestAppContext) {
    let mut s = Settings::open(cx);
    s.open_settings();
    let mut cfg = shelbi_state::UserConfig::default();
    cfg.desktop.terminal_colors.set(4, Some("#5893d6".into()));
    cfg.desktop.terminal_colors.set(12, Some("blue".into()));
    let view = terminal_page(&mut s, cfg);
    let text = |s: &mut Settings, slot| s.read(|_, cx| view.read(cx).color_text(slot, cx));
    assert_eq!(text(&mut s, 4), "#5893D6");
    let default = s.read(|_, cx| cx.global::<crate::theme::Theme>().terminal.ansi[12]);
    assert_eq!(text(&mut s, 12), crate::elements::terminal::colors::hex(default), "a bad value is ignored");
    assert!(s.read(|_, cx| view.read(cx).can_reset()));
}

#[gpui::test]
fn the_preview_draws_the_frame_s_session_with_no_session_behind_it(cx: &mut TestAppContext) {
    let mut s = Settings::open(cx);
    s.open_settings();
    let view = terminal_page(&mut s, shelbi_state::UserConfig::default());
    s.settle();
    let preview = s.read(|_, cx| view.read(cx).preview());
    let text = s.read(|_, cx| preview.read(cx).terminal_text());
    let rows: Vec<&str> = text.lines().collect();
    assert!(rows[0].ends_with("Claude Code"), "{text}");
    assert!(text.contains("● Update(crates/shelbi-state/src/keymap/actions.rs)"), "{text}");
    assert_eq!(rows.last().copied(), Some("? for shortcuts"), "the prompt at the foot: {text}");
    // Keys go nowhere: the screen doesn't change.
    let focus = s.read(|_, cx| gpui::Focusable::focus_handle(preview.read(cx), cx));
    s.update(|_, window, cx| window.focus(&focus, cx));
    s.key("x");
    assert_eq!(s.read(|_, cx| preview.read(cx).terminal_text()), text);
}

// --- the Keybindings page (`dt-settings-keybindings-editor`) ----------------

impl Settings {
    /// Settings open on its Keybindings page.
    fn open_keybindings(cx: &mut TestAppContext) -> Settings {
        let mut s = Settings::open(cx);
        s.open_settings();
        s.key("cmd-2");
        s
    }

    /// Read the Keybindings page.
    fn page_read<R>(&self, f: impl FnOnce(&crate::views::keybindings::KeybindingsPage) -> R) -> R {
        self.read(|w, cx| match &w.route {
            Route::Settings(view) => f(view.read(cx).keybindings().read(cx)),
            _ => panic!("Settings is shown"),
        })
    }

    /// The window's binding for `chord` in global.
    fn global(&self, chord: &str) -> Option<GlobalAction> {
        let chord = shelbi_state::keymap::KeyChord::parse(chord).unwrap();
        self.read(|w, _| w.keymaps.global.dispatch(chord))
    }
}

#[gpui::test]
fn the_keybindings_page_lists_every_context(cx: &mut TestAppContext) {
    let mut s = Settings::open_keybindings(cx);
    for selector in [
        "settings-open-keys",
        "keybindings-search",
        "keybindings-context",
        "keybindings-reset-all",
        "keybindings-group-global",
        "keybindings-group-sidebar",
        "keybindings-group-kanban",
        "keybindings-group-popover",
        "keybindings-group-activity",
        "keybindings-group-palette",
        "keybinding-global-open_palette-key-0",
        "keybinding-sidebar-nav_up-key-1",
    ] {
        assert!(s.drawn(selector), "{selector}");
    }
    // The ↑ keycap leads (the frame's order), K follows.
    let up = s.bounds("keybinding-sidebar-nav_up-key-1").unwrap();
    let k = s.bounds("keybinding-sidebar-nav_up-key-0").unwrap();
    assert!(up.origin.x < k.origin.x);
    // The hover buttons are out of the resting layout.
    let row = s.bounds("keybinding-sidebar-nav_up").unwrap();
    assert!(k.right() <= row.right());
}

#[gpui::test]
fn a_rebind_takes_effect_in_the_window_at_once(cx: &mut TestAppContext) {
    let mut s = Settings::open_keybindings(cx);
    assert_eq!(s.global("ctrl-p"), Some(GlobalAction::OpenPalette));
    s.click("keybinding-global-open_palette-key-0");
    assert!(s.page_read(|p| p.state().capture().is_some()));
    assert!(s.drawn("keybinding-global-open_palette-key-0"), "the keycap waits");

    // ⌘K is the window's palette chord now, ⌃P no longer.
    s.key("cmd-k");
    assert!(s.page_read(|p| p.state().capture().is_none()));
    assert_eq!(s.global("super-k"), Some(GlobalAction::OpenPalette));
    assert_eq!(s.global("ctrl-p"), None);
    assert!(s.page_read(|p| p.state().any_customized()), "Reset all is on");

    // Reset all puts ⌃P back.
    s.click("keybindings-reset-all");
    assert_eq!(s.global("ctrl-p"), Some(GlobalAction::OpenPalette));
    assert_eq!(s.global("super-k"), None);
}

#[gpui::test]
fn a_capture_takes_the_windows_own_chords_and_esc_only_cancels(cx: &mut TestAppContext) {
    let mut s = Settings::open_keybindings(cx);
    // Esc ends the capture without leaving Settings.
    s.click("keybinding-sidebar-nav_up-key-0");
    s.key("escape");
    assert_eq!(s.page(), Some(SettingsPage::Keybindings));
    assert!(s.page_read(|p| p.state().capture().is_none()));

    // ⌘1, the General page's chord, is the capture's while it is open.
    s.click("keybinding-sidebar-nav_up-key-0");
    s.key("cmd-1");
    assert_eq!(s.page(), Some(SettingsPage::Keybindings));
    let nav_up = shelbi_state::keymap::Action::Sidebar(SidebarAction::NavUp);
    let chords: Vec<String> = s.page_read(|p| p.state().chords(nav_up).iter().map(|c| c.canonical()).collect());
    assert_eq!(chords, ["super-1", "up"]);
    // Closed again, ⌘1 is the page chord once more.
    s.key("cmd-1");
    assert_eq!(s.page(), Some(SettingsPage::General));

    // A capture left open by a click on another page's row ends with it.
    s.key("cmd-2");
    s.click("keybinding-sidebar-nav_up-key-0");
    s.click("settings-nav-terminal");
    assert!(s.page_read(|p| p.state().capture().is_none()));
    s.key("cmd-2");
    assert_eq!(s.page(), Some(SettingsPage::Keybindings), "⌘2 reaches the window");
}

#[gpui::test]
fn a_conflict_shows_on_its_row_until_confirmed(cx: &mut TestAppContext) {
    let mut s = Settings::open_keybindings(cx);
    s.click("keybinding-sidebar-nav_up-key-0");
    s.key("r");
    assert!(s.drawn("keybindings-conflict"));
    s.key("r");
    assert!(!s.drawn("keybindings-conflict"));
    let refresh = shelbi_state::keymap::Action::Sidebar(SidebarAction::Refresh);
    assert!(s.page_read(|p| p.state().chords(refresh).is_empty()), "moved off Refresh");
    // The window's sidebar has it too.
    let r = shelbi_state::keymap::KeyChord::parse("r").unwrap();
    assert_eq!(s.read(|w, _| w.keymaps.sidebar.dispatch(r)), Some(SidebarAction::NavUp));
}

#[gpui::test]
fn the_search_and_the_context_picker_narrow_the_list(cx: &mut TestAppContext) {
    let mut s = Settings::open_keybindings(cx);
    s.click("keybindings-search");
    for key in ["c", "o", "l", "u", "m", "n"] {
        s.key(key);
    }
    assert_eq!(s.page(), Some(SettingsPage::Keybindings), "typing stays in the field");
    assert!(s.drawn("keybinding-kanban-nav_left"));
    assert!(!s.drawn("keybinding-sidebar-nav_up") && !s.drawn("keybindings-group-global"));
    // Esc clears the search, then hands the keys back to the sidebar.
    s.key("escape");
    assert!(s.drawn("keybinding-sidebar-nav_up"));
    s.key("escape");
    assert!(s.window_focused());
    assert_eq!(s.page(), Some(SettingsPage::Keybindings));

    s.click("keybindings-context");
    assert!(s.page_read(|p| p.picker_open()));
    s.click("keybindings-context-activity");
    assert!(!s.page_read(|p| p.picker_open()));
    assert!(s.drawn("keybindings-group-activity"));
    assert!(!s.drawn("keybindings-group-global") && !s.drawn("keybindings-group-sidebar"));
}

/// The General page's view, with `config.yaml` "read" as `cfg`.
fn general_page(s: &mut Settings, cfg: shelbi_state::UserConfig) -> Entity<SettingsView> {
    s.key("cmd-1");
    let view = s.read(|w, _| match &w.route {
        Route::Settings(view) => view.clone(),
        _ => panic!("settings is open"),
    });
    s.update(|_, window, cx| view.update(cx, |v, cx| v.load_config(cfg, window, cx)));
    s.settle();
    view
}

fn look(s: &mut Settings) -> crate::theme::window::Translucency {
    s.h.cx.update(|_, cx| crate::theme::window::WindowLook::current(cx).effective())
}

fn theme_opacity(s: &mut Settings) -> f32 {
    s.h.cx.update(|_, cx| cx.global::<Theme>().opacity())
}

fn window_applied(s: &mut Settings) -> crate::theme::window::Translucency {
    s.h.cx.update(|window, _| crate::theme::window::applied(window.window_handle().window_id()))
}

#[gpui::test]
fn the_sliders_apply_to_the_window_as_they_move_and_save(cx: &mut TestAppContext) {
    use crate::theme::window::Translucency;
    let mut s = Settings::open(cx);
    s.open_settings();
    // Nothing set: opaque and clear, the app as it was.
    assert_eq!(look(&mut s), Translucency::default());
    assert_eq!(theme_opacity(&mut s), 1.0);
    let mut cfg = shelbi_state::UserConfig::default();
    (cfg.desktop.window_opacity, cfg.desktop.window_blur) = (Some(85), Some(20));
    let view = general_page(&mut s, cfg);
    for selector in ["settings-window-opacity", "settings-window-blur"] {
        assert!(s.drawn(selector), "{selector}");
    }
    assert!(!s.drawn("settings-reduce-transparency"));
    let (opacity, blur) = s.read(|_, cx| (view.read(cx).opacity_slider(), view.read(cx).blur_slider()));
    let value = |s: &mut Settings, e: &Entity<Slider>| s.read(|_, cx| e.read(cx).value());
    assert_eq!((value(&mut s, &opacity), value(&mut s, &blur)), (85, 20), "the saved values");
    assert_eq!(look(&mut s), Translucency { opacity: 85, blur: 20 });
    assert_eq!(theme_opacity(&mut s), 0.85, "the surfaces carry it");
    assert_eq!(window_applied(&mut s), Translucency { opacity: 85, blur: 20 }, "the open window has it");

    // The keys: a click focuses it (and lands it under the pointer, the
    // track's middle), → steps it, ⇧→ steps further, Home / End go to the
    // ends; each applies at once and is saved.
    s.click("settings-window-opacity");
    assert_eq!(value(&mut s, &opacity), 65);
    let saved = |s: &mut Settings| s.read(|_, cx| view.read(cx).saved().desktop.clone());
    assert_eq!(saved(&mut s).window_opacity, Some(65), "letting go saves it");
    s.key("right");
    assert_eq!((value(&mut s, &opacity), theme_opacity(&mut s)), (66, 0.66));
    s.key("shift-right");
    assert_eq!(value(&mut s, &opacity), 76);
    s.key("left");
    assert_eq!(value(&mut s, &opacity), 75);
    assert_eq!(saved(&mut s).window_opacity, Some(75));
    assert_eq!(window_applied(&mut s).opacity, 75);
    s.key("home");
    assert_eq!(value(&mut s, &opacity), 30, "the floor");
    s.key("left");
    assert_eq!(value(&mut s, &opacity), 30, "stays at the floor");
    s.key("end");
    assert_eq!((value(&mut s, &opacity), theme_opacity(&mut s)), (100, 1.0));
    let desktop = saved(&mut s);
    assert_eq!((desktop.window_opacity, desktop.window_blur), (None, Some(20)), "100% isn't written");
    assert_eq!(window_applied(&mut s), Translucency { opacity: 100, blur: 20 });

    s.click("settings-window-blur");
    s.key("shift-left");
    assert_eq!(value(&mut s, &blur), 20, "the middle of 0 to 50, then 5 down");
    s.key("home");
    assert_eq!(saved(&mut s).window_blur, None, "0 isn't written");
    assert!(saved(&mut s).is_empty(), "back to the defaults");

    // A theme switch keeps the window's opacity.
    s.click("settings-window-opacity");
    s.key("home");
    s.click("settings-theme-Light");
    assert_eq!(theme_opacity(&mut s), 0.3);
    assert!(!s.read(|_, cx| cx.global::<Theme>().is_dark()));
}

#[gpui::test]
fn reduce_transparency_makes_the_window_opaque_and_disables_the_sliders(cx: &mut TestAppContext) {
    use crate::theme::window::{self as translucency, Translucency};
    let mut s = Settings::open(cx);
    s.open_settings();
    let mut cfg = shelbi_state::UserConfig::default();
    (cfg.desktop.window_opacity, cfg.desktop.window_blur) = (Some(70), Some(10));
    let view = general_page(&mut s, cfg);
    assert_eq!(theme_opacity(&mut s), 0.7);

    s.update(|_, _, cx| translucency::set_reduce_transparency(true, cx));
    s.settle();
    assert_eq!(look(&mut s), Translucency::default());
    assert_eq!(theme_opacity(&mut s), 1.0);
    assert_eq!(window_applied(&mut s), Translucency { opacity: 100, blur: 0 });
    assert!(s.drawn("settings-reduce-transparency"), "the note says why");
    let opacity = s.read(|_, cx| view.read(cx).opacity_slider());
    assert!(s.read(|_, cx| opacity.read(cx).is_disabled()));
    s.click("settings-window-opacity");
    s.key("right");
    assert_eq!(s.read(|_, cx| opacity.read(cx).value()), 70, "a disabled slider doesn't move");
    assert_eq!(s.read(|_, cx| view.read(cx).saved().desktop.window_opacity), Some(70), "the setting is kept");

    s.update(|_, _, cx| translucency::set_reduce_transparency(false, cx));
    s.settle();
    assert_eq!(look(&mut s), Translucency { opacity: 70, blur: 10 });
    assert_eq!(theme_opacity(&mut s), 0.7);
    assert!(!s.drawn("settings-reduce-transparency"));
    assert!(!s.read(|_, cx| opacity.read(cx).is_disabled()));
}
