//! Settings in its own sidebar (`dt-settings-sections`): opening it pushes
//! the settings sidebar into the leading column on the General page (or the
//! last page used), ⌘1 to ⌘4 and ↑ / ↓ plus Enter switch its pages while it
//! is open (and ⌘N keeps its sidebar meaning outside), and Back or Esc
//! return to the view it was opened over with the app sidebar back and its
//! selection on that view's row (`dt-back-syncs-sidebar`).
//!
//! The window is the smoke test's headless one ([`Harness`]) with an
//! injected clock for the push. The settings view reads no disk under test,
//! except the Workflows tests, which read a throwaway `SHELBI_HOME`.

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

    s.key("cmd-4");
    assert!(s.drawn("settings-page-workflows") && s.drawn("settings-workflows"));
    assert!(s.drawn("workflows-new") && s.drawn("statuses-new") && s.drawn("statuses-edit-yaml"));
    assert!(s.drawn("status-categories"));
    assert!(!s.drawn("settings-font-size"));
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
        Some(SettingsPage::Workflows),
        "it crosses into the project section"
    );
    s.key("down");
    assert_eq!(
        s.selected_row(),
        Some(SettingsPage::Workflows),
        "it stops at the last row"
    );
    s.key("enter");
    assert_eq!(s.page(), Some(SettingsPage::Workflows));
    s.key("up");
    s.key("enter");
    assert_eq!(
        (s.page(), s.selected_row()),
        (Some(SettingsPage::Terminal), Some(SettingsPage::Terminal))
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

// ---- Settings ▸ Workflows (`dt-wf-settings-index`) -------------------------

use crate::views::workflows::WorkflowsPage;
use crate::views::workflows_state::tests::Home;
use shelbi_state::workflow_config::WorkflowConfig;

impl Settings {
    /// Settings on Workflows (⌘4), its page loaded from `home`'s files.
    fn open_workflows(cx: &mut TestAppContext) -> Settings {
        let mut s = Settings::open(cx);
        s.open_settings();
        s.key("cmd-4");
        let page = s.workflows();
        s.update(|_, window, cx| page.update(cx, |p, cx| p.load(window, cx)));
        s.settle();
        s
    }

    fn workflows(&self) -> Entity<WorkflowsPage> {
        self.read(|w, cx| match &w.route {
            Route::Settings(view) => view.read(cx).workflows(),
            _ => panic!("Settings is open"),
        })
    }

    /// Run the page's saves out and draw.
    fn saved(&mut self) {
        self.h.cx.run_until_parked();
        self.settle();
    }

    /// Type `text` into `input` as the user would, then press Enter.
    fn enter_text(&mut self, input: Entity<gpui_kit::component::input::InputState>, text: &str) {
        self.update(|_, window, cx| {
            let focus = gpui::Focusable::focus_handle(input.read(cx), cx);
            window.focus(&focus, cx);
            input.update(cx, |s, cx| s.set_value(text.to_string(), window, cx));
        });
        self.h.key("enter");
        self.saved();
    }

    fn edited(&self) -> Vec<String> {
        self.read(|w, _| w.edited_workflows.clone())
    }
}

fn config() -> WorkflowConfig {
    WorkflowConfig::load("alpha").unwrap()
}

#[gpui::test]
fn the_settings_sidebar_has_the_project_section(cx: &mut TestAppContext) {
    let mut s = Settings::open(cx);
    s.open_settings();
    assert!(s.drawn("settings-project") && s.drawn("sidebar-project-title"));
    let global = s.bounds("settings-nav-terminal").unwrap();
    let switcher = s.bounds("sidebar-project-title").unwrap();
    let workflows = s.bounds("settings-nav-workflows").unwrap();
    assert!(global.bottom() < switcher.top() && switcher.bottom() < workflows.top(), "in the frame's order");
    s.click("settings-nav-workflows");
    assert_eq!((s.page(), s.selected_row()), (Some(SettingsPage::Workflows), Some(SettingsPage::Workflows)));

    // The switcher opens the app sidebar's project menu.
    s.h.take_calls();
    s.click("sidebar-project-title");
    assert_eq!(s.h.take_calls(), [HostCall::OpenSwitcher]);
    s.update(|w, _, cx| w.open_switcher(cx));
    s.settle();
    assert!(s.drawn("sidebar-switcher"), "its menu shows over the settings sidebar");
}

#[gpui::test]
fn switching_projects_reopens_the_project_pages_for_the_new_project(cx: &mut TestAppContext) {
    // The switch subscribes the window to beta's feed: a canned one, as a
    // live feed's reader thread would wake the test scheduler off its thread.
    // Both projects' files live in the test's own home.
    let home = Home::with_project("page-switch", "alpha");
    crate::views::workflows_state::tests::write_project(&home.root, "beta");
    let (hub, _feeds) = crate::feeds::tests::canned_hub();
    cx.update(|cx| cx.set_global(hub));
    let mut s = Settings::open_workflows(cx);
    let before = s.workflows();
    s.update(|w, window, cx| w.switch_to("beta", window, cx));
    s.saved();
    assert_eq!(s.page(), Some(SettingsPage::Workflows), "still on Workflows");
    let after = s.workflows();
    assert_ne!(before.entity_id(), after.entity_id(), "a fresh page");
    assert_eq!(s.read(|_, cx| after.read(cx).state().project().to_string()), "beta");
    assert!(s.drawn("settings-project"));
    assert!(!s.drawn("status-row-todo"), "alpha's rows went with its page");
    s.update(|_, window, cx| after.update(cx, |p, cx| p.load(window, cx)));
    s.saved();
    assert!(s.drawn("status-row-todo"), "beta's statuses");
}

#[gpui::test]
fn the_workflows_page_lists_the_projects_workflows_and_statuses(cx: &mut TestAppContext) {
    let _home = Home::with_project("page-rows", "alpha");
    let mut s = Settings::open_workflows(cx);
    assert!(s.drawn("workflow-row-task") && s.drawn("workflow-row-subtask"));
    let badge = s.bounds("workflow-default-badge").unwrap();
    assert!(s.bounds("workflow-row-task").unwrap().contains(&badge.center()), "Task is the default");
    for id in ["backlog", "todo", "in-progress", "review", "done", "canceled"] {
        let selector: &'static str = Box::leak(format!("status-row-{id}").into_boxed_str());
        assert!(s.drawn(selector), "{id}");
    }
    let page = s.workflows();
    let name = s.read(|_, cx| page.read(cx).name_field("todo").unwrap().read(cx).value().to_string());
    assert_eq!(name, "To Do");

    // The edit button and a row click both open the editor.
    s.click("workflow-edit-task");
    assert!(s.drawn("workflow-editor"));
    s.click("workflow-editor-back");
    s.click("workflow-row-subtask");
    assert_eq!(s.edited(), ["task", "subtask"]);
    assert_eq!(s.title(), "subtask");
}

#[gpui::test]
fn status_edits_save_as_they_are_made(cx: &mut TestAppContext) {
    let home = Home::with_project("page-statuses", "alpha");
    let mut s = Settings::open_workflows(cx);
    let page = s.workflows();

    // Rename on Enter: the id stays.
    let field = s.read(|_, cx| page.read(cx).name_field("todo").unwrap());
    s.enter_text(field, "Up next");
    assert_eq!(config().status("todo").unwrap().name, "Up next");
    assert!(home.read("statuses.yaml").starts_with("## The project's statuses."), "comments kept");

    // Category from the picker.
    s.click("status-category-review");
    assert!(s.drawn("status-category-menu"));
    s.click("status-category-review-active");
    s.saved();
    assert_eq!(config().status("review").unwrap().category, shelbi_core::StatusCategory::Active);
    assert!(!s.drawn("status-category-menu"));

    // Reorder by dropping a row on another's.
    s.update(|_, window, cx| page.update(cx, |p, cx| p.drop_on("canceled", "backlog", window, cx)));
    s.saved();
    assert_eq!(config().statuses()[0].id, "canceled");

    // A status in use can't be deleted.
    s.click("status-delete-review");
    s.saved();
    assert!(config().status("review").is_some());

    // New status: typed into the new row, then deleted.
    s.click("statuses-new");
    assert!(s.drawn("status-new-name"));
    let draft = s.read(|_, cx| page.read(cx).draft_input());
    s.enter_text(draft, "QA");
    assert_eq!(config().status("qa").map(|st| st.name.clone()).as_deref(), Some("QA"));
    assert!(!s.drawn("status-new-name"), "the new row closes");
    s.click("status-delete-qa");
    s.saved();
    assert!(config().status("qa").is_none());
}

#[gpui::test]
fn a_refused_rename_shows_under_its_row_and_puts_the_name_back(cx: &mut TestAppContext) {
    let _home = Home::with_project("page-refused", "alpha");
    let mut s = Settings::open_workflows(cx);
    let page = s.workflows();
    let field = s.read(|_, cx| page.read(cx).name_field("todo").unwrap());
    s.enter_text(field.clone(), "   ");
    assert!(s.drawn("status-error-todo"));
    assert_eq!(s.read(|_, cx| field.read(cx).value().to_string()), "To Do");
    assert_eq!(config().status("todo").unwrap().name, "To Do");
}

#[gpui::test]
fn workflow_actions_create_duplicate_default_and_guard_delete(cx: &mut TestAppContext) {
    let home = Home::with_project("page-workflows", "alpha");
    let mut s = Settings::open_workflows(cx);
    let page = s.workflows();

    // The default can't be deleted.
    s.click("workflow-more-task");
    assert!(s.drawn("workflow-menu"));
    s.click("workflow-delete-task");
    s.saved();
    assert!(home.workflows().join("task.yaml").exists());
    assert!(s.drawn("workflow-menu"), "a disabled item leaves the menu open");
    s.click("workflow-more-task");
    assert!(!s.drawn("workflow-menu"), "its button closes it");

    // Set as default.
    s.click("workflow-more-subtask");
    s.click("workflow-set-default-subtask");
    s.saved();
    assert_eq!(config().default_workflow(), "subtask");

    // Duplicate offers a name; Enter makes it.
    s.click("workflow-more-task");
    s.click("workflow-duplicate-task");
    let draft = s.read(|_, cx| page.read(cx).draft_input());
    assert_eq!(s.read(|_, cx| draft.read(cx).value().to_string()), "task-copy");
    s.enter_text(draft.clone(), "task-copy");
    assert!(config().workflow("task-copy").is_some());

    // New workflow: created, then opened in the editor.
    s.click("workflows-new");
    s.enter_text(draft, "hotfix");
    assert!(home.workflows().join("hotfix.yaml").exists());
    assert_eq!(s.edited(), ["hotfix"]);
    assert!(s.drawn("workflow-editor"));
    s.click("workflow-editor-back");
    s.saved();

    // Delete one nothing uses.
    s.click("workflow-more-hotfix");
    s.click("workflow-delete-hotfix");
    s.saved();
    assert!(!home.workflows().join("hotfix.yaml").exists());
}

// ---- The workflow editor (`dt-wf-editor-form`) ------------------------------

use crate::views::workflow_editor::WorkflowEditor;
use crate::views::workflow_editor_state::{Change, Field, ReviewMode};

impl Settings {
    /// Settings ▸ Workflows with workflow `name` open in the editor (by
    /// its row's edit button).
    fn open_editor(cx: &mut TestAppContext, name: &'static str) -> Settings {
        let mut s = Settings::open_workflows(cx);
        let selector: &'static str = Box::leak(format!("workflow-edit-{name}").into_boxed_str());
        s.click(selector);
        assert!(s.drawn("workflow-editor"), "the editor opened");
        s
    }

    fn editor(&self) -> Option<Entity<WorkflowEditor>> {
        self.read(|w, cx| match &w.route {
            Route::Settings(view) => view.read(cx).editor(),
            _ => None,
        })
    }

    fn edit(&mut self, f: impl FnOnce(&mut WorkflowEditor, &mut Window, &mut Context<WorkflowEditor>)) {
        let editor = self.editor().expect("the editor is open");
        self.update(|_, window, cx| editor.update(cx, |e, cx| f(e, window, cx)));
        self.settle();
    }

    fn dirty(&self) -> bool {
        let editor = self.editor().expect("the editor is open");
        self.read(|_, cx| editor.read(cx).state().is_some_and(|s| s.is_dirty()))
    }

    fn error_at(&self, field: Field) -> Option<String> {
        let editor = self.editor().expect("the editor is open");
        self.read(|_, cx| editor.read(cx).state().and_then(|s| s.error_at(&field)).map(str::to_string))
    }

    fn title(&mut self) -> String {
        let editor = self.editor().expect("the editor is open");
        self.read(|_, cx| editor.read(cx).state().map(|s| s.name().to_string()).unwrap_or_default())
    }

    /// Type `text` into `input` as the user would: the field focused and
    /// emptied, then the text typed (a typed edit, not a set value).
    fn type_into(&mut self, input: Entity<gpui_kit::component::input::InputState>, text: &str) {
        self.update(|_, window, cx| {
            let focus = gpui::Focusable::focus_handle(input.read(cx), cx);
            window.focus(&focus, cx);
            input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        });
        self.h.cx.simulate_input(text);
        self.settle();
    }
}

fn on_disk(name: &str) -> shelbi_core::Workflow {
    config().workflow(name).unwrap().clone()
}

#[gpui::test]
fn the_editor_takes_the_window_without_the_sidebar(cx: &mut TestAppContext) {
    let _home = Home::with_project("ed-shell", "alpha");
    let mut s = Settings::open_editor(cx, "task");
    assert!(!s.drawn("column-settings") && !s.drawn("leading-column"), "no sidebar");
    let titlebar = s.bounds("workflow-editor-titlebar").unwrap();
    assert_eq!((titlebar.top(), titlebar.size.height), (px(0.0), px(52.0)), "the window's top row");
    let back = s.bounds("workflow-editor-back").unwrap();
    assert_eq!(back.left(), px(80.0), "after the traffic lights' room");
    assert!(s.drawn("workflow-editor-default-badge"), "Task is the default");
    assert!(s.drawn("workflow-editor-save") && !s.drawn("workflow-editor-discard"), "nothing to discard yet");
    assert!(s.drawn("workflow-form") && s.drawn("workflow-statuses-table"));
    for id in ["backlog", "todo", "in-progress", "review", "done", "canceled"] {
        let selector: &'static str = Box::leak(format!("workflow-status-row-{id}").into_boxed_str());
        assert!(s.drawn(selector), "{id}");
    }
    assert!(s.drawn("workflow-transition-row-0"));

    // Builder shows its placeholder over the same draft; Form comes back.
    s.click("workflow-editor-mode-builder");
    assert!(s.drawn("workflow-builder-placeholder") && !s.drawn("workflow-form"));
    s.click("workflow-editor-mode-form");
    assert!(s.drawn("workflow-form"));

    // ⌘N doesn't switch pages under it; Back returns to the page, the
    // sidebar with it.
    s.key("cmd-1");
    assert!(s.drawn("workflow-editor"));
    s.click("workflow-editor-back");
    assert!(!s.drawn("workflow-editor") && s.drawn("settings-workflows") && s.drawn("column-settings"));
    assert_eq!(s.page(), Some(SettingsPage::Workflows));
}

#[gpui::test]
fn edits_wait_for_save_and_discard_reverts_them(cx: &mut TestAppContext) {
    let home = Home::with_project("ed-save", "alpha");
    let before = home.read("task.yaml");
    let mut s = Settings::open_editor(cx, "task");

    // An owner from its picker: dirty, nothing written.
    s.click("workflow-owner-backlog");
    assert!(s.drawn("workflow-owner-menu"));
    s.click("workflow-owner-backlog-agent");
    assert!(s.dirty() && s.drawn("workflow-editor-discard"));
    assert_eq!(home.read("task.yaml"), before);

    // Discard reverts it.
    s.click("workflow-editor-discard");
    assert!(!s.dirty() && !s.drawn("workflow-editor-discard"));

    // A description typed, then saved: written with the file's comments.
    let field = s.read(|w, cx| match &w.route {
        Route::Settings(view) => view.read(cx).editor().unwrap().read(cx).description_field(),
        _ => unreachable!(),
    });
    s.type_into(field, "Branch work, reviewed.");
    assert!(s.dirty());
    s.click("workflow-editor-save");
    s.saved();
    assert!(!s.dirty());
    assert_eq!(on_disk("task").description.as_deref(), Some("Branch work, reviewed."));
    let text = home.read("task.yaml");
    assert!(text.contains("## Statuses run left to right."), "{text}");
}

#[gpui::test]
fn leaving_with_unsaved_edits_asks_to_save_or_discard(cx: &mut TestAppContext) {
    let home = Home::with_project("ed-leave", "alpha");
    let before = home.read("subtask.yaml");
    let mut s = Settings::open_editor(cx, "subtask");
    s.edit(|e, window, cx| e.make(Change::InitialStatus("todo".into()), window, cx));

    // Esc asks; Cancel stays with the edits.
    s.h.focus_window();
    s.key("escape");
    assert!(s.drawn("workflow-leave-prompt"));
    s.click("workflow-leave-cancel");
    assert!(!s.drawn("workflow-leave-prompt") && s.drawn("workflow-editor") && s.dirty());

    // Back asks; Discard leaves without writing.
    s.click("workflow-editor-back");
    assert!(s.drawn("workflow-leave-prompt"));
    s.click("workflow-leave-discard");
    assert!(!s.drawn("workflow-editor"));
    assert_eq!(home.read("subtask.yaml"), before);

    // Save writes, then leaves.
    s.click("workflow-edit-subtask");
    s.edit(|e, window, cx| e.make(Change::InitialStatus("todo".into()), window, cx));
    s.click("workflow-editor-back");
    s.click("workflow-leave-save");
    s.saved();
    assert!(!s.drawn("workflow-editor"));
    assert_eq!(on_disk("subtask").initial_status.as_deref(), Some("todo"));

    // Nothing unsaved: Esc just goes.
    s.click("workflow-edit-subtask");
    s.h.focus_window();
    s.key("escape");
    assert!(!s.drawn("workflow-editor") && !s.drawn("workflow-leave-prompt"));
}

#[gpui::test]
fn statuses_and_transitions_edit_from_their_controls(cx: &mut TestAppContext) {
    let _home = Home::with_project("ed-rows", "alpha");
    let mut s = Settings::open_editor(cx, "task");

    // A refused owner shows under its row; the draft is as it was.
    s.click("workflow-owner-done");
    s.click("workflow-owner-done-agent");
    assert!(s.drawn("workflow-status-error-done"));
    assert!(!s.dirty());
    // Its agent first, then the owner.
    s.click("workflow-agent-done");
    s.click("workflow-agent-done-review");
    s.click("workflow-owner-done");
    s.click("workflow-owner-done-agent");
    assert!(!s.drawn("workflow-status-error-done"));

    // Tags typed into a row's field.
    let tags = s.read(|w, cx| match &w.route {
        Route::Settings(view) => view.read(cx).editor().unwrap().read(cx).tags_field("done").unwrap(),
        _ => unreachable!(),
    });
    s.type_into(tags, "mac, gpu");

    // Reorder by a drop, remove a status.
    s.edit(|e, window, cx| e.drop_on("canceled", "backlog", window, cx));
    s.click("workflow-status-delete-todo");

    // An action added from a row's +, another moved earlier and removed.
    s.click("workflow-add-action-0");
    assert!(s.drawn("workflow-add-action-menu"));
    s.click("workflow-add-action-0-merge");
    s.click("workflow-action-0-2");
    s.click("workflow-action-0-2-earlier");
    s.click("workflow-action-0-0");
    s.click("workflow-action-0-0-remove");
    // A transition's endpoint from its picker.
    s.click("workflow-transition-to-2");
    s.click("workflow-transition-to-2-done");

    s.click("workflow-editor-save");
    s.saved();
    let w = on_disk("task");
    let ids: Vec<_> = w.statuses.iter().map(|st| st.id.as_str()).collect();
    assert_eq!(ids, ["canceled", "backlog", "in-progress", "review", "done"]);
    let done = w.status("done").unwrap();
    assert_eq!((done.owner, done.agent.as_deref()), (shelbi_core::Owner::Agent, Some("review")));
    assert_eq!(done.tags, ["mac", "gpu"]);
    let ts = w.transitions.unwrap();
    use shelbi_core::TransitionAction::*;
    assert_eq!(ts[0].actions, [Merge, OpenPr]);
    assert_eq!((ts[2].from.as_str(), ts[2].to.as_str()), ("in-progress", "done"));
}

#[gpui::test]
fn details_and_lower_sections_show_their_errors_inline(cx: &mut TestAppContext) {
    let _home = Home::with_project("ed-details", "alpha");
    let mut s = Settings::open_editor(cx, "task");

    // Details open under the row; a bad timeout shows there.
    s.click("workflow-transition-details-0");
    assert!(s.drawn("workflow-details"));
    let timeout = s.read(|w, cx| match &w.route {
        Route::Settings(view) => view.read(cx).editor().unwrap().read(cx).details_timeout_field().unwrap(),
        _ => unreachable!(),
    });
    s.type_into(timeout.clone(), "soon");
    assert!(s.error_at(Field::Transition(0)).is_some_and(|e| e.contains("whole number")));
    assert!(s.drawn("workflow-transition-error-0"));
    s.type_into(timeout, "120");
    assert_eq!(s.error_at(Field::Transition(0)), None);
    s.click("workflow-transition-details-0");
    assert!(!s.drawn("workflow-details"), "the button closes them");

    // Git: a templated base branch needs a required field.
    let base = s.read(|w, cx| match &w.route {
        Route::Settings(view) => view.read(cx).editor().unwrap().read(cx).base_branch_field(),
        _ => unreachable!(),
    });
    s.type_into(base, "feature/{{feature}}");
    assert!(s.error_at(Field::Git).is_some_and(|e| e.contains("required task field")));

    // Review: Run the branch without a serve command is held at Save.
    s.edit(|e, window, cx| e.make(Change::ReviewMode(ReviewMode::Serve), window, cx));
    s.edit(|e, window, cx| e.save(window, cx));
    s.saved();
    assert!(s.error_at(Field::Review).is_some_and(|e| e.contains("starts the server")));
    assert!(on_disk("task").review.is_none(), "nothing written");
    assert!(on_disk("task").transitions.unwrap()[0].ready_timeout.is_none());
}

#[gpui::test]
fn the_more_menu_sets_default_duplicates_and_deletes(cx: &mut TestAppContext) {
    let home = Home::with_project("ed-more", "alpha");
    let mut s = Settings::open_editor(cx, "subtask");
    assert!(!s.drawn("workflow-editor-default-badge"));
    s.click("workflow-editor-more");
    s.click("workflow-editor-set-default");
    assert!(s.drawn("workflow-editor-default-badge") && s.dirty());
    s.click("workflow-editor-save");
    s.saved();
    assert_eq!(config().default_workflow(), "subtask");

    // Duplicate: the copy opens in the editor.
    s.click("workflow-editor-more");
    s.click("workflow-editor-duplicate");
    s.saved();
    assert_eq!(s.title(), "subtask-copy");
    assert!(home.workflows().join("subtask-copy.yaml").exists());

    // Delete the copy: back to the page, its file gone.
    s.click("workflow-editor-more");
    s.click("workflow-editor-delete");
    s.saved();
    assert!(!s.drawn("workflow-editor"));
    assert!(!home.workflows().join("subtask-copy.yaml").exists());
}

#[gpui::test]
fn manage_project_statuses_returns_to_the_page(cx: &mut TestAppContext) {
    let _home = Home::with_project("ed-manage", "alpha");
    let mut s = Settings::open_editor(cx, "task");
    s.click("workflow-manage-statuses");
    s.saved();
    assert!(!s.drawn("workflow-editor") && s.drawn("statuses-table") && s.drawn("column-settings"));
    assert_eq!(s.page(), Some(SettingsPage::Workflows));
}

#[gpui::test]
fn a_rename_waits_for_save_and_a_taken_name_shows_at_the_field(cx: &mut TestAppContext) {
    let home = Home::with_project("ed-rename", "alpha");
    let mut s = Settings::open_editor(cx, "subtask");
    let name = s.read(|w, cx| match &w.route {
        Route::Settings(view) => view.read(cx).editor().unwrap().read(cx).name_field(),
        _ => unreachable!(),
    });
    s.type_into(name.clone(), "task");
    assert!(s.dirty());
    s.click("workflow-editor-save");
    s.saved();
    assert!(s.drawn("workflow-name-error"));
    assert!(home.workflows().join("subtask.yaml").exists());

    s.type_into(name, "child");
    assert!(!s.drawn("workflow-name-error"), "typing clears it");
    s.click("workflow-editor-save");
    s.saved();
    assert_eq!(s.title(), "child");
    assert!(home.workflows().join("child.yaml").exists() && !home.workflows().join("subtask.yaml").exists());
}

