//! Settings in its own sidebar (`dt-settings-sections`): opening it pushes
//! the settings sidebar into the leading column on the General page (or the
//! last page used), ⌘1 to ⌘6 and ↑ / ↓ plus Enter switch its pages while it
//! is open (and ⌘N keeps its sidebar meaning outside), and Back or Esc
//! return to the view it was opened over with the app sidebar back and its
//! selection on that view's row (`dt-back-syncs-sidebar`).
//!
//! The window is the smoke test's headless one ([`Harness`]) with an
//! injected clock for the push. The settings view reads no disk under test,
//! except the Workflows and Agents tests, which read a throwaway
//! `SHELBI_HOME`.

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
        shelbi_app::settings::pages().find(|p| self.bounds(p.selector()).is_some_and(|b| b.contains(&at)))
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
        &CommandKind::FocusWorkspace {
            workspace: "alpha-1".into(),
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
        Some(SettingsPage::Workspaces),
        "it crosses into the project section"
    );
    s.key("down");
    s.key("down");
    s.key("down");
    assert_eq!(
        s.selected_row(),
        Some(SettingsPage::Agents),
        "it stops at the last row"
    );
    s.key("enter");
    assert_eq!(s.page(), Some(SettingsPage::Agents));
    s.key("up");
    s.key("up");
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

/// dt-terminal-scrollbar-overlay: Show scrollbar shows the saved choice,
/// and a pick applies to every terminal at once and is saved (Automatic,
/// the default, isn't written). The preview's scrollbar overlays its inset.
#[gpui::test]
fn the_terminal_page_picks_when_the_scrollbar_shows(cx: &mut TestAppContext) {
    use shelbi_state::TerminalScrollbar;
    let mut s = Settings::open(cx);
    s.open_settings();
    let mut cfg = shelbi_state::UserConfig::default();
    cfg.desktop.terminal_scrollbar = Some(TerminalScrollbar::Always);
    let view = terminal_page(&mut s, cfg);
    for selector in ["settings-scrollbar-Automatic", "settings-scrollbar-When scrolling", "settings-scrollbar-Always"] {
        assert!(s.drawn(selector), "{selector}");
    }
    assert_eq!(s.read(|_, cx| view.read(cx).scrollbar()), TerminalScrollbar::Always);
    let live = |s: &mut Settings| s.h.cx.update(|_, cx| TerminalSettings::current(cx).scrollbar);
    let saved = |s: &mut Settings| s.read(|_, cx| view.read(cx).saved().desktop.terminal_scrollbar);

    s.click("settings-scrollbar-When scrolling");
    assert_eq!(live(&mut s), TerminalScrollbar::Scrolling, "every terminal follows at once");
    assert_eq!(saved(&mut s), Some(TerminalScrollbar::Scrolling));
    let preview = s.read(|_, cx| view.read(cx).preview());
    assert_eq!(s.read(|_, cx| preview.read(cx).settings.scrollbar), TerminalScrollbar::Scrolling);

    s.click("settings-scrollbar-Automatic");
    assert_eq!(live(&mut s), TerminalScrollbar::Auto);
    assert_eq!(saved(&mut s), None, "the default isn't written");

    // Changed elsewhere (another window's Settings): the control follows.
    s.h.cx.update(|_, cx| {
        TerminalSettings::set(TerminalSettings { scrollbar: TerminalScrollbar::Always, ..TerminalSettings::current(cx) }, cx)
    });
    s.settle();
    assert_eq!(s.read(|_, cx| view.read(cx).scrollbar()), TerminalScrollbar::Always);

    let inset = crate::views::settings::PREVIEW_INSET;
    assert_eq!(
        s.read(|_, cx| preview.read(cx).scrollbar_padding()),
        crate::elements::terminal::scrollbar::Padding { top: inset, right: inset, bottom: inset }
    );
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

#[gpui::test]
fn the_search_clear_button_shows_with_a_query_and_clears_it(cx: &mut TestAppContext) {
    let mut s = Settings::open_keybindings(cx);
    let search_focused = |s: &mut Settings| {
        let window = s.h.window.clone();
        s.h.cx.update(|win, cx| match &window.read(cx).route {
            Route::Settings(view) => view.read(cx).keybindings().read(cx).search_focused(win, cx),
            _ => panic!("Settings is shown"),
        })
    };
    assert!(!s.drawn("keybindings-search-clear"), "hidden while the box is empty");
    s.click("keybindings-search");
    assert!(search_focused(&mut s));
    assert!(!s.drawn("keybindings-search-clear"), "focus alone doesn't show it");
    for key in ["c", "o", "l"] {
        s.key(key);
    }
    assert!(s.drawn("keybindings-search-clear"), "shown with a query");
    assert!(!s.drawn("keybinding-sidebar-nav_up"));
    // The text area ends before the button, inside the box.
    let field = s.bounds("keybindings-search-field").unwrap();
    let clear = s.bounds("keybindings-search-clear").unwrap();
    let bx = s.bounds("keybindings-search").unwrap();
    assert!(field.right() <= clear.left(), "{field:?} runs under {clear:?}");
    assert!(clear.right() <= bx.right() && clear.top() >= bx.top() && clear.bottom() <= bx.bottom());

    s.click("keybindings-search-clear");
    assert!(s.page_read(|p| p.state().query.is_empty()));
    assert!(s.drawn("keybinding-sidebar-nav_up"), "every row is back");
    assert!(!s.drawn("keybindings-search-clear"), "hidden again");
    assert!(search_focused(&mut s), "the keyboard stays in the field");
    assert_eq!(s.page(), Some(SettingsPage::Keybindings));
    s.key("k");
    assert_eq!(s.page_read(|p| p.state().query.clone()), "k", "ready for the next search");
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

// ---- Edit YAML in the ⋮ menus (`dt-workflow-edit-yaml`) ---------------------

use crate::views::external_edit::tests::opened;

impl Settings {
    /// Another app takes the front (the user's editor), then this window
    /// comes back.
    fn come_back(&mut self) {
        self.h.cx.deactivate_window();
        self.h.cx.update(|window, _| window.activate_window());
        self.saved();
    }

    /// Whether `first`'s item sits above `second`'s in the open menu.
    fn above(&mut self, first: &'static str, second: &'static str) -> bool {
        self.bounds(first).unwrap().bottom() <= self.bounds(second).unwrap().top()
    }

    fn page_description(&self, name: &str) -> Option<String> {
        let page = self.workflows();
        self.read(|_, cx| {
            let w = page.read(cx).state().workflows().into_iter().find(|w| w.name == name);
            w.and_then(|w| w.description)
        })
    }
}

#[gpui::test]
fn edit_yaml_on_a_workflow_row_opens_its_file_and_the_page_rereads_it(cx: &mut TestAppContext) {
    let home = Home::with_project("page-edit-yaml", "alpha");
    let file = home.workflows().join("task.yaml");
    let mut s = Settings::open_workflows(cx);
    s.click("workflow-more-task");
    assert!(s.above("workflow-edit-yaml-task", "workflow-duplicate-task"), "Edit YAML comes first");
    s.click("workflow-edit-yaml-task");
    s.saved();
    assert_eq!(opened(&file), 1, "the workflow's file, in the editor");
    assert!(!s.drawn("workflow-menu"), "the menu closes");
    assert!(s.edited().is_empty(), "the workflow editor stays shut");

    // The hand edit shows once the window is back in front.
    let edited = home.read("task.yaml").replace("Branch work with a review gate.", "Edited by hand.");
    std::fs::write(&file, edited).unwrap();
    assert_eq!(s.page_description("task").as_deref(), Some("Branch work with a review gate."));
    s.come_back();
    assert_eq!(s.page_description("task").as_deref(), Some("Edited by hand."));
}

#[gpui::test]
fn a_broken_workflow_file_still_offers_edit_yaml(cx: &mut TestAppContext) {
    let home = Home::with_project("page-broken-yaml", "alpha");
    let file = home.workflows().join("broken.yaml");
    std::fs::write(&file, "name: broken\nstatuses: [\n").unwrap();
    let mut s = Settings::open_workflows(cx);
    assert!(s.drawn("workflow-row-broken") && s.drawn("workflow-more-broken"));
    s.click("workflow-more-broken");
    assert!(s.drawn("workflow-edit-yaml-broken"));
    assert!(!s.drawn("workflow-duplicate-broken") && !s.drawn("workflow-delete-broken"), "Edit YAML only");
    s.click("workflow-edit-yaml-broken");
    s.saved();
    assert_eq!(opened(&file), 1);

    // Fixed by hand: back in front, it's a workflow like the others.
    std::fs::write(&file, crate::views::workflows_state::fixture::SUBTASK.replace("name: subtask", "name: broken")).unwrap();
    s.come_back();
    assert!(s.page_description("broken").is_some(), "the fixed file reads as a workflow");
    assert!(s.drawn("workflow-edit-broken"), "and can be edited here");
}

// ---- The workflow editor (`dt-wf-editor-form`) ------------------------------

use crate::views::workflow_editor::WorkflowEditor;
use crate::views::workflow_editor_state::{Change, Field, ReviewMode};

impl Settings {
    /// Settings ▸ Workflows with workflow `name` open in the editor (by
    /// its row's edit button), on its Form.
    fn open_editor(cx: &mut TestAppContext, name: &'static str) -> Settings {
        let mut s = Settings::open_builder(cx, name);
        s.click("workflow-editor-mode-form");
        assert!(s.drawn("workflow-form"), "the Form shows");
        s
    }

    /// [`Settings::open_editor`], left on the Builder it opens on.
    fn open_builder(cx: &mut TestAppContext, name: &'static str) -> Settings {
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

    /// The draft's description.
    fn draft_description(&self) -> Option<String> {
        let editor = self.editor().expect("the editor is open");
        self.read(|_, cx| editor.read(cx).state().and_then(|s| s.workflow().description.clone()))
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
    assert_eq!(back.left(), px(sidebar::BESIDE_TRAFFIC_LIGHTS), "beside the traffic lights");
    assert!(
        f32::from(back.left()) - sidebar::TRAFFIC_LIGHTS_RIGHT >= 12.0,
        "a clear gap from the zoom button: {:?}",
        back.left()
    );
    let middle = back.top() + back.size.height / 2.0;
    assert_eq!(middle, px(sidebar::TITLEBAR_H / 2.0), "centered on the traffic lights");
    assert!(s.drawn("workflow-editor-default-badge"), "Task is the default");
    assert!(s.drawn("workflow-editor-save") && !s.drawn("workflow-editor-discard"), "nothing to discard yet");
    assert!(s.drawn("workflow-form") && s.drawn("workflow-statuses-table"));
    for id in ["backlog", "todo", "in-progress", "review", "done", "canceled"] {
        let selector: &'static str = Box::leak(format!("workflow-status-row-{id}").into_boxed_str());
        assert!(s.drawn(selector), "{id}");
    }
    assert!(s.drawn("workflow-transition-row-0"));

    // Builder draws the same draft; Form comes back.
    s.click("workflow-editor-mode-builder");
    assert!(s.drawn("workflow-builder") && !s.drawn("workflow-form"));
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
fn edit_yaml_in_the_editor_asks_about_unsaved_edits_first(cx: &mut TestAppContext) {
    let home = Home::with_project("ed-edit-yaml", "alpha");
    let file = home.workflows().join("subtask.yaml");
    let before = home.read("subtask.yaml");
    let mut s = Settings::open_editor(cx, "subtask");
    s.click("workflow-editor-more");
    assert!(s.above("workflow-editor-edit-yaml", "workflow-editor-duplicate"), "Edit YAML comes first");

    // Nothing unsaved: it opens at once.
    s.click("workflow-editor-edit-yaml");
    s.saved();
    assert_eq!(opened(&file), 1);
    assert!(!s.drawn("workflow-leave-prompt") && !s.drawn("workflow-editor-menu"));

    // Unsaved edits: it asks. Cancel opens nothing and keeps them.
    s.edit(|e, window, cx| e.make(Change::InitialStatus("todo".into()), window, cx));
    s.click("workflow-editor-more");
    s.click("workflow-editor-edit-yaml");
    assert!(s.drawn("workflow-leave-prompt"));
    s.click("workflow-leave-cancel");
    s.saved();
    assert_eq!(opened(&file), 1);
    assert!(s.dirty() && s.drawn("workflow-editor"));

    // Discard reverts, then opens.
    s.click("workflow-editor-more");
    s.click("workflow-editor-edit-yaml");
    s.click("workflow-leave-discard");
    s.saved();
    assert_eq!(opened(&file), 2);
    assert!(!s.dirty() && s.drawn("workflow-editor"), "still editing, the draft clean");
    assert_eq!(home.read("subtask.yaml"), before);

    // Save writes, then opens.
    s.edit(|e, window, cx| e.make(Change::InitialStatus("todo".into()), window, cx));
    s.click("workflow-editor-more");
    s.click("workflow-editor-edit-yaml");
    s.click("workflow-leave-save");
    s.saved();
    assert_eq!(opened(&file), 3);
    assert_eq!(on_disk("subtask").initial_status.as_deref(), Some("todo"));
    assert!(!s.dirty() && s.drawn("workflow-editor"));
}

#[gpui::test]
fn the_editor_rereads_a_clean_draft_on_return_and_keeps_a_dirty_one(cx: &mut TestAppContext) {
    let home = Home::with_project("ed-yaml-return", "alpha");
    let file = home.workflows().join("subtask.yaml");
    let original = "Focused work that merges back into a parent task.";
    let hand_edit = |text: &str| {
        let edited = home.read("subtask.yaml").replace(original, text);
        std::fs::write(&file, edited).unwrap();
    };
    let mut s = Settings::open_editor(cx, "subtask");
    let description = |s: &Settings| s.draft_description();

    // A clean draft takes the hand edit once the window is back in front.
    s.click("workflow-editor-more");
    s.click("workflow-editor-edit-yaml");
    s.saved();
    hand_edit("Edited by hand.");
    assert_eq!(description(&s).as_deref(), Some(original), "not before");
    s.come_back();
    assert_eq!(description(&s).as_deref(), Some("Edited by hand."));
    assert!(!s.dirty());
    assert_eq!(opened(&file), 1);

    // A draft edited meanwhile is kept, and its save won't overwrite a
    // second hand edit.
    s.click("workflow-editor-more");
    s.click("workflow-editor-edit-yaml");
    s.saved();
    s.edit(|e, window, cx| e.make(Change::InitialStatus("todo".into()), window, cx));
    let edited = home.read("subtask.yaml").replace("Edited by hand.", "Edited twice.");
    std::fs::write(&file, &edited).unwrap();
    s.come_back();
    assert!(s.dirty(), "the draft is kept");
    assert_eq!(description(&s).as_deref(), Some("Edited by hand."));
    s.click("workflow-editor-save");
    s.saved();
    assert!(s.error_at(Field::Save).unwrap().starts_with("Changed on disk"));
    assert_eq!(home.read("subtask.yaml"), edited, "the hand edit stands");
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


// ---- The workflow editor's Builder (`dt-wf-editor-builder`) -----------------

use crate::views::workflow_builder_state::Selection;
use crate::views::workflow_editor_state::Mode;
use gpui::{Modifiers, MouseButton, Point, ScrollDelta, ScrollWheelEvent, TouchPhase};

impl Settings {
    fn center(&mut self, selector: &'static str) -> Point<Pixels> {
        self.bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn")).center()
    }

    fn mouse_move(&mut self, at: Point<Pixels>, held: bool) {
        self.h.cx.simulate_mouse_move(at, held.then_some(MouseButton::Left), Modifiers::none());
        self.settle();
    }

    fn mouse_down(&mut self, at: Point<Pixels>) {
        self.h.cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
        self.settle();
    }

    fn mouse_up(&mut self, at: Point<Pixels>) {
        self.h.cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
        self.settle();
    }

    fn scroll(&mut self, at: Point<Pixels>, dx: f32, dy: f32, modifiers: Modifiers) {
        self.h.cx.simulate_event(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Pixels(gpui::point(px(dx), px(dy))),
            modifiers,
            touch_phase: TouchPhase::Moved,
        });
        self.settle();
    }

    fn selection(&self) -> Option<Selection> {
        let editor = self.editor().expect("the editor is open");
        self.read(|_, cx| editor.read(cx).builder_selection())
    }

    fn draft(&self) -> shelbi_core::Workflow {
        let editor = self.editor().expect("the editor is open");
        self.read(|_, cx| editor.read(cx).state().unwrap().workflow().clone())
    }
}

#[gpui::test]
fn the_builder_draws_the_workflows_statuses_and_transitions(cx: &mut TestAppContext) {
    let _home = Home::with_project("bu-draw", "alpha");
    let mut s = Settings::open_builder(cx, "task");
    assert!(s.drawn("workflow-builder") && s.drawn("builder-toolbar") && !s.drawn("workflow-form"), "opens on the Builder");
    for id in ["backlog", "todo", "in-progress", "review", "done", "canceled"] {
        let selector: &'static str = Box::leak(format!("builder-node-{id}").into_boxed_str());
        let b = s.bounds(selector).unwrap_or_else(|| panic!("{id}'s node"));
        assert_eq!((b.size.width, b.size.height), (px(216.0), px(88.0)), "{id} at 100%");
    }
    // The frame's arrangement: Review beside In Progress, Done under it,
    // Canceled at the top right.
    let (ip, review, done, canceled) = (
        s.bounds("builder-node-in-progress").unwrap(),
        s.bounds("builder-node-review").unwrap(),
        s.bounds("builder-node-done").unwrap(),
        s.bounds("builder-node-canceled").unwrap(),
    );
    assert_eq!(review.top(), ip.top());
    assert_eq!(review.left() - ip.right(), px(168.0));
    assert_eq!(done.left(), review.left());
    assert_eq!(done.top() - review.bottom(), px(120.0), "the labeled gap");
    assert_eq!(canceled.top(), s.bounds("builder-node-backlog").unwrap().top());
    // Every transition's label, the Initial badge on Backlog, the legend
    // and the hint; nothing selected, no inspector.
    for i in 0..4 {
        let selector: &'static str = Box::leak(format!("builder-edge-label-{i}").into_boxed_str());
        assert!(s.drawn(selector), "transition {i}'s label");
    }
    let badge = s.bounds("builder-initial-badge").unwrap();
    assert!(s.bounds("builder-node-backlog").unwrap().contains(&badge.center()));
    assert!(s.drawn("builder-legend") && s.drawn("builder-canvas-hint"));
    assert!(!s.drawn("builder-inspector"));
}

#[gpui::test]
fn the_builder_lays_statuses_out_in_the_catalogs_order(cx: &mut TestAppContext) {
    let _home = Home::with_project("bu-catalog", "alpha");
    let mut s = Settings::open_workflows(cx);
    let page = s.workflows();
    let nodes = |s: &mut Settings| {
        ["backlog", "todo", "in-progress", "review", "done", "canceled"].map(|id| {
            let selector: &'static str = Box::leak(format!("builder-node-{id}").into_boxed_str());
            s.bounds(selector).unwrap()
        })
    };
    // Review made a Done status: within the category, the catalog's order,
    // Review beside In Progress and Done under it.
    s.click("status-category-review");
    s.click("status-category-review-done");
    s.saved();
    s.click("workflow-edit-task");
    let [_, _, ip, review, done, _] = nodes(&mut s);
    assert_eq!((review.top(), done.left()), (ip.top(), review.left()));
    assert_eq!(done.top() - review.bottom(), px(120.0), "Done under Review");

    // Done moved above Review in the catalog: the Builder re-lays.
    s.click("workflow-editor-back");
    s.update(|_, window, cx| page.update(cx, |p, cx| p.drop_on("done", "review", window, cx)));
    s.saved();
    s.click("workflow-edit-task");
    let [_, _, ip, review, done, _] = nodes(&mut s);
    assert_eq!((done.top(), review.left()), (ip.top(), done.left()));
    assert_eq!(review.top() - done.bottom(), px(120.0), "Review under Done");

    // The workflow's own list order moves nothing.
    let before = nodes(&mut s);
    s.edit(|e, window, cx| e.make(Change::MoveStatus { id: "backlog".into(), index: 5 }, window, cx));
    assert_eq!(s.draft().statuses.last().map(|st| st.id.as_str()), Some("backlog"));
    assert_eq!(nodes(&mut s), before);
}

#[gpui::test]
fn selecting_a_status_edits_it_in_the_inspector_and_the_form_sees_it(cx: &mut TestAppContext) {
    let _home = Home::with_project("bu-status", "alpha");
    let mut s = Settings::open_builder(cx, "task");
    s.click("builder-node-review");
    assert_eq!(s.selection(), Some(Selection::Status("review".into())));
    assert!(s.drawn("builder-inspector") && s.drawn("builder-status-category"));
    assert!(!s.drawn("builder-canvas-hint"), "the frame drops the hint once something is selected");
    // Its outgoing transitions: Done and Canceled.
    assert!(s.drawn("builder-outgoing-1") && s.drawn("builder-outgoing-3") && !s.drawn("builder-outgoing-0"));

    // Owner and agent from their pickers, Set as initial.
    s.click("builder-owner-review");
    s.click("builder-owner-review-agent");
    s.click("builder-set-initial");
    let w = s.draft();
    assert_eq!(w.status("review").unwrap().owner, shelbi_core::Owner::Agent);
    assert_eq!(w.initial_status.as_deref(), Some("review"));
    assert!(s.dirty());

    // Its tags field is the Form's: typed here, it's there.
    let tags = s.read(|w, cx| match &w.route {
        Route::Settings(view) => view.read(cx).editor().unwrap().read(cx).tags_field("review").unwrap(),
        _ => unreachable!(),
    });
    s.type_into(tags.clone(), "review, gpu");
    assert_eq!(s.draft().status("review").unwrap().tags, ["review", "gpu"]);
    s.click("workflow-editor-mode-form");
    assert!(s.drawn("workflow-tags-review"));
    assert_eq!(s.read(|_, cx| tags.read(cx).value().to_string()), "review, gpu");

    // An edit in the Form shows in the Builder.
    s.click("workflow-agent-done");
    s.click("workflow-agent-done-review");
    s.click("workflow-editor-mode-builder");
    s.click("builder-node-done");
    assert_eq!(s.draft().status("done").unwrap().agent.as_deref(), Some("review"));

    // The Status picker moves the selection; Remove from workflow removes.
    s.click("builder-status");
    s.click("builder-status-canceled");
    assert_eq!(s.selection(), Some(Selection::Status("canceled".into())));
    s.click("builder-remove-status");
    assert!(s.draft().status("canceled").is_none());
    assert_eq!(s.selection(), None);
    assert!(!s.drawn("builder-node-canceled"));

    // Add status puts it back (from the catalog) and selects it.
    s.click("builder-add-status");
    s.click("builder-add-status-canceled");
    assert!(s.draft().status("canceled").is_some());
    assert_eq!(s.selection(), Some(Selection::Status("canceled".into())));

    // Close deselects.
    s.click("builder-inspector-close");
    assert!(!s.drawn("builder-inspector"));

    s.click("workflow-editor-save");
    s.saved();
    let w = on_disk("task");
    assert_eq!(w.initial_status.as_deref(), Some("review"));
    assert_eq!(w.status("review").unwrap().tags, ["review", "gpu"]);
    assert_eq!(w.statuses.last().unwrap().id, "canceled");
}

#[gpui::test]
fn dragging_a_handle_onto_a_status_draws_a_transition(cx: &mut TestAppContext) {
    let _home = Home::with_project("bu-drag", "alpha");
    let mut s = Settings::open_builder(cx, "task");
    // Hover In Progress: its handles show.
    let ip = s.center("builder-node-in-progress");
    s.mouse_move(ip, false);
    assert!(s.drawn("builder-handle-in-progress-bottom"));
    assert!(!s.drawn("builder-handle-review-top"), "only the hovered node's");

    // Press its bottom handle and drag over Done.
    let handle = s.center("builder-handle-in-progress-bottom");
    s.mouse_down(handle);
    assert!(matches!(s.selection(), Some(Selection::Draw(_))));
    assert!(s.drawn("builder-inspector") && s.drawn("builder-destination-none"));
    let done = s.center("builder-node-done");
    s.mouse_move(done, true);
    assert!(s.drawn("builder-destination") && s.drawn("builder-drawing-label"));
    assert!(s.drawn("builder-handle-done-left"), "the destination's handles show");
    s.mouse_up(done);

    // New transition: both ends, no actions yet.
    let Some(Selection::New(n)) = s.selection() else { panic!("a new transition") };
    assert_eq!((n.from.as_str(), n.to.as_str()), ("in-progress", "done"));
    s.click("builder-new-add-action");
    s.click("builder-new-add-action-push_branch");
    s.click("builder-new-add-action");
    s.click("builder-new-add-action-merge");
    assert_eq!(s.draft().transitions.as_ref().unwrap().len(), 4, "not in the draft until Add transition");
    s.click("builder-new-add");

    // In the draft, selected, with the note.
    let ts = s.draft().transitions.unwrap();
    assert_eq!(ts.len(), 5);
    use shelbi_core::TransitionAction::*;
    assert_eq!((ts[4].from.as_str(), ts[4].to.as_str(), ts[4].actions.as_slice()), ("in-progress", "done", &[PushBranch, Merge][..]));
    assert_eq!(s.selection(), Some(Selection::Transition { index: 4, added: true }));
    assert!(s.drawn("builder-transition-added") && s.drawn("builder-edge-label-4"));

    s.click("workflow-editor-save");
    s.saved();
    assert_eq!(on_disk("task").transitions.unwrap().len(), 5);
}

impl Settings {
    /// `out` points (on screen) beyond the middle of In Progress's `side`:
    /// its handle's outer half, past the node's edge.
    fn past_handle(&mut self, side: &'static str, out: f32) -> Point<Pixels> {
        let selector: &'static str = Box::leak(format!("builder-handle-in-progress-{side}").into_boxed_str());
        let c = self.center(selector);
        let out = px(out);
        match side {
            "top" => gpui::point(c.x, c.y - out),
            "right" => gpui::point(c.x + out, c.y),
            "bottom" => gpui::point(c.x, c.y + out),
            _ => gpui::point(c.x - out, c.y),
        }
    }

    /// Hover In Progress, then move out onto `side`'s handle `out` points
    /// past the node's edge.
    fn onto_handle(&mut self, side: &'static str, out: f32) -> Point<Pixels> {
        let ip = self.center("builder-node-in-progress");
        self.mouse_move(ip, false);
        let at = self.past_handle(side, out);
        self.mouse_move(at, false);
        at
    }

    fn drawing_from(&self) -> Option<(String, Option<crate::views::workflow_graph::Side>)> {
        match self.selection() {
            Some(Selection::Draw(d)) => Some((d.from, d.handle)),
            _ => None,
        }
    }
}

#[gpui::test]
fn a_handle_stays_hovered_past_the_nodes_edge_and_beats_the_edge_there(cx: &mut TestAppContext) {
    use crate::views::workflow_graph::Side;
    let _home = Home::with_project("bu-handle", "alpha");
    let mut s = Settings::open_builder(cx, "task");
    // In Progress → Review leaves from In Progress's right side, where its
    // right handle sits. On the handle's outer half, past the node's edge:
    // the handles still show (they used to vanish, the hover being the
    // node's rect alone)...
    let at = s.onto_handle("right", 4.0);
    assert!(s.drawn("builder-handle-in-progress-right"), "still hovered past the node's edge");
    assert!(s.drawn("builder-handle-hot-in-progress-right"), "the hovered handle shows it");
    // ...and a press there draws, rather than selecting the transition
    // whose line starts under it.
    s.mouse_down(at);
    assert_eq!(s.drawing_from(), Some(("in-progress".into(), Some(Side::Right))));
    s.mouse_up(at);
    s.key("escape");
    s.key("escape");
    assert_eq!(s.selection(), None);

    // Every side, from inside the node out to the hit area's edge, without
    // the handles flickering off.
    for side in ["top", "right", "bottom", "left"] {
        let ip = s.center("builder-node-in-progress");
        s.mouse_move(ip, false);
        for out in [-4.0, 0.0, 4.0, 8.0, 11.0] {
            let at = s.past_handle(side, out);
            s.mouse_move(at, false);
            assert!(s.drawn("builder-handle-in-progress-bottom"), "{side} {out}: still hovered");
        }
    }
    // Well away, the hover goes.
    let at = s.past_handle("bottom", 40.0);
    s.mouse_move(at, false);
    assert!(!s.drawn("builder-handle-in-progress-bottom"));
}

#[gpui::test]
fn a_handles_hit_area_is_large_at_every_zoom(cx: &mut TestAppContext) {
    use crate::views::workflow_graph::Side;
    let _home = Home::with_project("bu-handle-zoom", "alpha");
    let mut s = Settings::open_builder(cx, "task");
    for zoom in ["50", "100", "200"] {
        s.click("builder-zoom");
        let level: &'static str = Box::leak(format!("builder-zoom-{zoom}").into_boxed_str());
        s.click(level);
        // In Progress to the canvas's middle (a scroll pans by its delta).
        let (canvas, ip) = (s.center("builder-canvas"), s.center("builder-node-in-progress"));
        s.scroll(canvas, f32::from(canvas.x - ip.x), f32::from(canvas.y - ip.y), Modifiers::none());
        // 11pt out from the dot's middle, on screen, at any zoom: well past
        // the 5pt dot, inside the 24pt target.
        for (side, want) in [("bottom", Side::Bottom), ("right", Side::Right), ("top", Side::Top)] {
            let at = s.onto_handle(side, 11.0);
            s.mouse_down(at);
            assert_eq!(s.drawing_from(), Some(("in-progress".into(), Some(want))), "{side} at {zoom}%");
            s.mouse_up(at);
            s.key("escape");
            s.key("escape");
            assert_eq!(s.selection(), None);
        }
    }
}

#[gpui::test]
fn add_transition_then_a_click_picks_the_destination(cx: &mut TestAppContext) {
    let _home = Home::with_project("bu-add", "alpha");
    let mut s = Settings::open_builder(cx, "task");
    s.click("builder-node-todo");
    s.click("builder-status-add-transition");
    assert!(matches!(s.selection(), Some(Selection::Draw(ref d)) if d.from == "todo"));
    assert!(s.drawn("builder-canvas-hint"), "the drawing's hint");
    // A click on the source itself isn't a destination.
    s.click("builder-node-todo");
    assert!(matches!(s.selection(), Some(Selection::Draw(_))));
    s.click("builder-node-in-progress");
    assert!(matches!(s.selection(), Some(Selection::New(_))));
    // Its To picker; then Cancel goes back to the status.
    s.click("builder-new-to");
    s.click("builder-new-to-review");
    assert!(matches!(s.selection(), Some(Selection::New(ref n)) if n.to == "review"));
    s.click("builder-new-cancel");
    assert_eq!(s.selection(), Some(Selection::Status("todo".into())));

    // A transition that already exists is refused in the panel.
    s.click("builder-node-in-progress");
    s.click("builder-add-transition");
    s.click("builder-node-review");
    s.click("builder-new-add");
    assert!(s.drawn("builder-new-error"));
    assert!(matches!(s.selection(), Some(Selection::New(_))), "still open to fix");
    assert_eq!(s.draft().transitions.unwrap().len(), 4);
    // Esc steps back: the new transition, then the status, then nothing.
    s.key("escape");
    assert_eq!(s.selection(), Some(Selection::Status("in-progress".into())));
    assert!(!s.drawn("builder-new-error"));
    s.key("escape");
    assert_eq!(s.selection(), None);
    assert!(s.drawn("workflow-editor"), "still in the editor");
}

#[gpui::test]
fn a_transition_panel_goes_back_to_the_status_it_came_from(cx: &mut TestAppContext) {
    let _home = Home::with_project("bu-back", "alpha");
    let mut s = Settings::open_builder(cx, "task");
    let editor = s.editor().unwrap();
    let offset = |s: &Settings| s.read(|_, cx| editor.read(cx).inspector_offset());
    s.click("builder-node-review");
    assert!(!s.drawn("builder-inspector-back"), "a Status panel has nothing to go back to");
    // Scrolled down a little, then into an outgoing transition.
    let inspector = s.center("builder-inspector");
    s.scroll(inspector, 0.0, -40.0, Modifiers::none());
    let kept = offset(&s);
    assert!(kept.y < px(0.0), "the Status panel scrolled");
    s.click("builder-outgoing-1");
    assert_eq!(s.selection(), Some(Selection::Transition { index: 1, added: false }));
    assert_eq!(offset(&s).y, px(0.0), "the transition panel starts at the top");
    // ← leads the heading, left of the title.
    let back = s.bounds("builder-inspector-back").expect("the back button");
    let title = s.bounds("builder-inspector-title").unwrap();
    assert!(back.right() <= title.left() && back.left() < title.left());
    assert_eq!(back.size.width, px(28.0), "the app's back button");
    s.click("builder-inspector-back");
    assert_eq!(s.selection(), Some(Selection::Status("review".into())));
    assert_eq!(offset(&s), kept, "where the Status panel was");
    assert!(!s.drawn("builder-inspector-back"));

    // Esc takes the same path: the status, then nothing.
    s.click("builder-outgoing-3");
    s.key("escape");
    assert_eq!(s.selection(), Some(Selection::Status("review".into())));
    s.key("escape");
    assert_eq!(s.selection(), None);

    // Add transition: ← drops the drawing, then the new transition.
    s.click("builder-node-todo");
    s.click("builder-status-add-transition");
    assert!(matches!(s.selection(), Some(Selection::Draw(_))) && s.drawn("builder-inspector-back"));
    s.click("builder-inspector-back");
    assert_eq!(s.selection(), Some(Selection::Status("todo".into())));
    s.click("builder-status-add-transition");
    s.click("builder-node-in-progress");
    assert!(matches!(s.selection(), Some(Selection::New(_))) && s.drawn("builder-inspector-back"));
    s.click("builder-inspector-back");
    assert_eq!(s.selection(), Some(Selection::Status("todo".into())));
    assert_eq!(s.draft().transitions.unwrap().len(), 4, "nothing added");
    assert!(!s.dirty());

    // A transition picked on the canvas: no ←, and Esc deselects.
    s.click("builder-edge-label-1");
    assert_eq!(s.selection(), Some(Selection::Transition { index: 1, added: false }));
    assert!(!s.drawn("builder-inspector-back"));
    s.key("escape");
    assert_eq!(s.selection(), None);
}

#[gpui::test]
fn a_selected_transitions_actions_reorder_change_and_delete(cx: &mut TestAppContext) {
    let _home = Home::with_project("bu-transition", "alpha");
    let mut s = Settings::open_builder(cx, "task");
    s.click("builder-edge-label-1");
    assert_eq!(s.selection(), Some(Selection::Transition { index: 1, added: false }));
    assert!(!s.drawn("builder-transition-added"));
    use shelbi_core::TransitionAction::*;
    // Push branch dragged after Merge; Merge becomes Close PR; Delete
    // branch added.
    s.drag_action("builder-action-1", 0, 1);
    assert_eq!(s.draft().transitions.unwrap()[1].actions, [Merge, PushBranch]);
    s.click("builder-action-1-0");
    s.click("builder-action-1-0-close_pr");
    s.click("builder-add-action");
    s.click("builder-add-action-delete_branch");
    s.click("builder-action-1-1-remove");
    assert_eq!(s.draft().transitions.unwrap()[1].actions, [ClosePr, DeleteBranch]);
    // To from its picker.
    s.click("builder-transition-to");
    s.click("builder-transition-to-canceled");
    assert!(s.drawn("builder-transition-error"), "review → canceled exists already");
    assert_eq!(s.draft().transitions.unwrap()[1].to, "done");

    s.click("builder-delete-transition");
    assert_eq!(s.draft().transitions.unwrap().len(), 3);
    assert_eq!(s.selection(), None);
}

impl Settings {
    /// Drag action row `from` of the list whose ids start `prefix` by its
    /// handle, and let go over row `onto`.
    fn drag_action(&mut self, prefix: &str, from: usize, onto: usize) {
        let selector = |part: String| -> &'static str { Box::leak(format!("{prefix}-{part}").into_boxed_str()) };
        let handle = self.center(selector(format!("{from}-handle")));
        self.mouse_down(handle);
        // Past gpui's drag threshold, so the drag starts.
        self.mouse_move(gpui::point(handle.x, handle.y + px(6.0)), true);
        let target = self.center(selector(format!("{onto}-row")));
        self.mouse_move(target, true);
        self.mouse_up(target);
    }

    /// Transition `index`'s label on the canvas, a line per action.
    fn canvas_label(&self, index: usize) -> Vec<String> {
        let editor = self.editor().expect("the editor is open");
        let t = self.draft().transitions.unwrap()[index].clone();
        self.read(|_, cx| {
            let g = editor.read(cx).builder_graph().expect("a graph");
            let edge = g.edges.iter().find(|e| e.from == t.from && e.to == t.to).expect("its edge");
            edge.label.as_ref().map(|l| l.lines.clone()).unwrap_or_default()
        })
    }
}

#[gpui::test]
fn dragging_an_action_handle_reorders_the_transitions_actions(cx: &mut TestAppContext) {
    let _home = Home::with_project("bu-action-drag", "alpha");
    let mut s = Settings::open_builder(cx, "task");
    use shelbi_core::TransitionAction::*;
    // Review → Done: Push branch, Merge; Delete branch added third.
    s.click("builder-edge-label-1");
    s.click("builder-add-action");
    s.click("builder-add-action-delete_branch");
    let actions = |s: &Settings| s.draft().transitions.unwrap()[1].actions.clone();
    assert_eq!(actions(&s), [PushBranch, Merge, DeleteBranch]);
    assert!(s.drawn("builder-action-1-0-handle") && s.drawn("builder-action-1-0-remove"));
    assert!(!s.drawn("builder-action-1-0-stepper"), "the stepper is gone");

    // First to last, last to first, and a drop on its own row.
    s.drag_action("builder-action-1", 0, 2);
    assert_eq!(actions(&s), [Merge, DeleteBranch, PushBranch]);
    s.drag_action("builder-action-1", 2, 0);
    assert_eq!(actions(&s), [PushBranch, Merge, DeleteBranch]);
    s.drag_action("builder-action-1", 1, 1);
    assert_eq!(actions(&s), [PushBranch, Merge, DeleteBranch], "dropped in place");

    // The canvas label and the Form follow the draft.
    s.drag_action("builder-action-1", 2, 0);
    assert_eq!(actions(&s), [DeleteBranch, PushBranch, Merge]);
    assert_eq!(s.canvas_label(1), ["Delete branch", "Push branch", "Merge"]);
    assert!(s.dirty());
    s.click("workflow-editor-mode-form");
    let width = |s: &mut Settings, slot: usize| {
        let selector: &'static str = Box::leak(format!("workflow-action-1-{slot}").into_boxed_str());
        s.bounds(selector).unwrap_or_else(|| panic!("the Form's chip {slot}")).size.width
    };
    // "Merge" is the shortest chip, now last.
    assert!(width(&mut s, 2) < width(&mut s, 0) && width(&mut s, 2) < width(&mut s, 1));

    s.click("workflow-editor-save");
    s.saved();
    assert_eq!(on_disk("task").transitions.unwrap()[1].actions, [DeleteBranch, PushBranch, Merge]);
}

#[gpui::test]
fn option_arrows_on_a_focused_handle_step_its_action(cx: &mut TestAppContext) {
    let _home = Home::with_project("bu-action-keys", "alpha");
    let mut s = Settings::open_builder(cx, "task");
    use shelbi_core::TransitionAction::*;
    s.click("builder-edge-label-1");
    s.click("builder-add-action");
    s.click("builder-add-action-delete_branch");
    let actions = |s: &Settings| s.draft().transitions.unwrap()[1].actions.clone();
    // A click focuses the handle; ⌥↓ steps it later and the focus follows.
    s.click("builder-action-1-0-handle");
    s.key("alt-down");
    assert_eq!(actions(&s), [Merge, PushBranch, DeleteBranch]);
    s.key("alt-down");
    assert_eq!(actions(&s), [Merge, DeleteBranch, PushBranch]);
    s.key("alt-down");
    assert_eq!(actions(&s), [Merge, DeleteBranch, PushBranch], "already last");
    s.key("alt-up");
    assert_eq!(actions(&s), [Merge, PushBranch, DeleteBranch]);
    // A plain arrow is not a reorder.
    s.key("up");
    assert_eq!(actions(&s), [Merge, PushBranch, DeleteBranch]);
    assert_eq!(s.selection(), Some(Selection::Transition { index: 1, added: false }));
}

#[gpui::test]
fn the_new_transition_panel_reorders_its_actions_the_same_way(cx: &mut TestAppContext) {
    let _home = Home::with_project("bu-new-action-drag", "alpha");
    let mut s = Settings::open_builder(cx, "task");
    use shelbi_core::TransitionAction::*;
    s.click("builder-node-in-progress");
    s.click("builder-status-add-transition");
    s.click("builder-node-done");
    for a in ["push_branch", "merge", "delete_branch"] {
        s.click("builder-new-add-action");
        let selector: &'static str = Box::leak(format!("builder-new-add-action-{a}").into_boxed_str());
        s.click(selector);
    }
    let actions = |s: &Settings| match s.selection() {
        Some(Selection::New(n)) => n.actions,
        other => panic!("a new transition, not {other:?}"),
    };
    // The same row as the Transition panel's: handle, picker, remove.
    let (row, picker) = (s.bounds("builder-new-action-0-row").unwrap(), s.bounds("builder-new-action-0").unwrap());
    assert_eq!(row.size.height, px(28.0));
    assert_eq!(picker.size.height, px(28.0));
    assert_eq!(s.bounds("builder-new-action-0-handle").unwrap().size.width, px(20.0));
    assert_eq!(s.bounds("builder-new-action-0-remove").unwrap().size.width, px(28.0));

    s.drag_action("builder-new-action", 0, 2);
    assert_eq!(actions(&s), [Merge, DeleteBranch, PushBranch]);
    s.drag_action("builder-new-action", 2, 0);
    assert_eq!(actions(&s), [PushBranch, Merge, DeleteBranch]);
    s.drag_action("builder-new-action", 0, 0);
    assert_eq!(actions(&s), [PushBranch, Merge, DeleteBranch], "dropped in place");
    s.click("builder-new-action-1-handle");
    s.key("alt-up");
    assert_eq!(actions(&s), [Merge, PushBranch, DeleteBranch]);
    s.click("builder-new-add");
    assert_eq!(s.draft().transitions.unwrap()[4].actions, [Merge, PushBranch, DeleteBranch]);
}

#[gpui::test]
fn the_keyboard_moves_the_focus_ring_selects_and_deletes(cx: &mut TestAppContext) {
    let _home = Home::with_project("bu-keys", "alpha");
    let mut s = Settings::open_builder(cx, "task");
    // A click on the background focuses the canvas (and selects nothing).
    let canvas = s.bounds("builder-canvas").unwrap();
    let empty = gpui::point(canvas.left() + px(12.0), canvas.bottom() - px(12.0));
    s.mouse_down(empty);
    s.mouse_up(empty);
    assert_eq!(s.selection(), None);
    s.key("tab");
    s.key("tab");
    let editor = s.editor().unwrap();
    let focus = |s: &Settings| s.read(|_, cx| editor.read(cx).builder_focus());
    assert_eq!(focus(&s).as_deref(), Some("todo"));
    s.key("down");
    assert_eq!(focus(&s).as_deref(), Some("in-progress"));
    s.key("right");
    assert_eq!(focus(&s).as_deref(), Some("review"));
    s.key("enter");
    assert_eq!(s.selection(), Some(Selection::Status("review".into())));
    s.key("backspace");
    assert!(s.draft().status("review").is_none(), "Delete removes the selected status");
    assert!(s.draft().transitions.unwrap().iter().all(|t| t.from != "review" && t.to != "review"));
    // A selected transition (the one left: In Progress → Canceled),
    // deleted by key.
    s.click("builder-edge-label-0");
    assert_eq!(s.selection(), Some(Selection::Transition { index: 0, added: false }));
    s.key("delete");
    assert_eq!(s.draft().transitions, None);
    // Esc with nothing selected leaves (asking first: there are edits).
    s.key("escape");
    assert!(s.drawn("workflow-leave-prompt"));
}

#[gpui::test]
fn the_canvas_pans_zooms_and_fits(cx: &mut TestAppContext) {
    let _home = Home::with_project("bu-view", "alpha");
    let mut s = Settings::open_builder(cx, "task");
    let before = s.bounds("builder-node-backlog").unwrap();
    let mid = s.center("builder-canvas");
    // A scroll pans.
    s.scroll(mid, -30.0, -40.0, Modifiers::none());
    let after = s.bounds("builder-node-backlog").unwrap();
    assert_eq!((after.left() - before.left(), after.top() - before.top()), (px(-30.0), px(-40.0)));
    // A drag on the background pans too.
    let canvas = s.bounds("builder-canvas").unwrap();
    let empty = gpui::point(canvas.left() + px(12.0), canvas.bottom() - px(12.0));
    s.mouse_down(empty);
    s.mouse_move(gpui::point(empty.x + px(20.0), empty.y), true);
    s.mouse_up(gpui::point(empty.x + px(20.0), empty.y));
    assert_eq!(s.bounds("builder-node-backlog").unwrap().left() - after.left(), px(20.0));

    // ⌘-scroll zooms about the pointer: the node under it stays put.
    let review = s.center("builder-node-review");
    s.scroll(review, 0.0, 50.0, Modifiers::command());
    let zoomed = s.bounds("builder-node-review").unwrap();
    assert!(zoomed.size.width > px(216.0));
    assert!(zoomed.contains(&review));

    // The zoom picker's 50%, then Fit.
    s.click("builder-zoom");
    s.click("builder-zoom-50");
    assert_eq!(s.bounds("builder-node-review").unwrap().size.width, px(108.0));
    s.click("builder-fit");
    let graph = s.bounds("builder-graph").unwrap();
    let canvas = s.bounds("builder-canvas").unwrap();
    assert!(graph.left() >= canvas.left() && graph.right() <= canvas.right() + px(1.0));
    assert!(graph.top() >= canvas.top() && graph.bottom() <= canvas.bottom() + px(1.0), "{graph:?} in {canvas:?}");
}

#[gpui::test]
fn twenty_statuses_and_forty_transitions_draw_in_a_frame(cx: &mut TestAppContext) {
    use crate::views::workflow_graph::big_fixture;
    let home = Home::with_project("bu-big", "alpha");
    std::fs::write(home.workflows().join("statuses.yaml"), big_fixture::statuses()).unwrap();
    std::fs::write(home.workflows().join("big.yaml"), big_fixture::workflow()).unwrap();
    let mut s = Settings::open_builder(cx, "big");
    assert_eq!(s.draft().statuses.len(), 20);
    assert!(s.drawn("builder-node-stage-19") && s.drawn("builder-edge-label-39"));
    // Debug-build frames on a shared machine: a loose bound that still
    // catches a layout gone quadratic.
    let start = std::time::Instant::now();
    for _ in 0..10 {
        s.h.draw();
    }
    let each = start.elapsed() / 10;
    assert!(each < std::time::Duration::from_millis(250), "{each:?} a frame");
    // Switching views keeps the draft as it is.
    s.edit(|e, window, cx| e.show_mode(Mode::Form, window, cx));
    assert!(s.drawn("workflow-form"));
}

// ---- Settings ▸ Agents (`dt-agents-settings-page`) ---------------------------

use crate::views::agents::AgentsPage;
use shelbi_state::agents_config::AgentsConfig;

impl Settings {
    /// Settings on Agents (⌘6), its page loaded from the test's home.
    fn open_agents(cx: &mut TestAppContext) -> Settings {
        let mut s = Settings::open(cx);
        s.open_settings();
        s.key("cmd-6");
        let page = s.agents();
        s.update(|_, window, cx| page.update(cx, |p, cx| p.load(window, cx)));
        s.settle();
        s
    }

    fn agents(&self) -> Entity<AgentsPage> {
        self.read(|w, cx| match &w.route {
            Route::Settings(view) => view.read(cx).agents(),
            _ => panic!("Settings is open"),
        })
    }

    fn edited_agents(&self) -> Vec<String> {
        self.read(|w, _| w.edited_agents.clone())
    }

    /// The page's agents, by name.
    fn agent_names(&self) -> Vec<String> {
        let page = self.agents();
        self.read(|_, cx| page.read(cx).state().agents().into_iter().map(|a| a.name).collect())
    }
}

fn agents_dir(home: &Home) -> std::path::PathBuf {
    home.root.join("projects").join("alpha").join("agents")
}

#[gpui::test]
fn the_agents_page_lists_the_projects_agents(cx: &mut TestAppContext) {
    let _home = crate::views::agents_state::tests::home_for("page-rows", "alpha");
    let mut s = Settings::open_agents(cx);
    assert_eq!((s.page(), s.selected_row()), (Some(SettingsPage::Agents), Some(SettingsPage::Agents)));
    let workflows = s.bounds("settings-nav-workflows").unwrap();
    assert!(workflows.bottom() <= s.bounds("settings-nav-agents").unwrap().top(), "after Workflows");

    assert_eq!(s.agent_names(), ["orchestrator", "developer", "review", "qa", "security", "adversarial"]);
    for selector in [
        "agent-row-orchestrator",
        "agent-row-developer",
        "agent-row-adversarial",
        "agent-customized-orchestrator",
        "agent-customized-review",
        "agent-recommended-developer",
        "agent-status-orchestrator-backlog",
        "agent-status-orchestrator-todo",
        "agent-status-developer-in-progress",
        "agent-status-review-review",
        "agents-shared-preview",
    ] {
        assert!(s.drawn(selector), "{selector}");
    }
    assert!(!s.drawn("agent-customized-developer"), "still the shipped default");
    let badge = s.bounds("agent-customized-orchestrator").unwrap();
    assert!(s.bounds("agent-row-orchestrator").unwrap().contains(&badge.center()));

    // The footnote's link goes to Workflows, sidebar and all.
    s.click("agents-workflows-link");
    assert_eq!((s.page(), s.selected_row()), (Some(SettingsPage::Workflows), Some(SettingsPage::Workflows)));
}

#[gpui::test]
fn the_edit_button_and_a_row_click_open_the_agent(cx: &mut TestAppContext) {
    let home = crate::views::agents_state::tests::home_for("page-edit", "alpha");
    let mut s = Settings::open_agents(cx);
    s.click("agent-edit-developer");
    assert!(s.drawn("agent-editor"), "the details editor opens");
    s.click("agent-editor-back");
    s.click("agent-row-qa");
    assert!(s.drawn("agent-editor"));
    assert_eq!(s.edited_agents(), ["developer", "qa"]);
    assert_eq!(s.agent_title(), "qa");
    assert_eq!(opened(&agents_dir(&home).join("qa").join("agent.yaml")), 0, "no file opens");
}

#[gpui::test]
fn a_new_agent_is_created_and_opened(cx: &mut TestAppContext) {
    let home = crate::views::agents_state::tests::home_for("page-new", "alpha");
    let mut s = Settings::open_agents(cx);
    s.click("agents-new");
    assert!(s.drawn("agent-new-name"));
    let draft = s.read(|w, cx| match &w.route {
        Route::Settings(view) => view.read(cx).agents().read(cx).draft_input(),
        _ => unreachable!(),
    });
    s.enter_text(draft.clone(), "developer");
    assert!(s.drawn("agent-new-error"), "a taken name stays on the new row");
    s.enter_text(draft, "docs");
    assert!(agents_dir(&home).join("docs").join("agent.yaml").exists());
    assert_eq!(s.edited_agents(), ["docs"], "it opens once created");
    assert_eq!(s.agent_title(), "docs");
    s.click("agent-editor-back");
    assert!(!s.drawn("agent-new-name"), "the new row closes");
    assert!(s.drawn("agent-row-docs"));
}

#[gpui::test]
fn the_menu_duplicates_resets_and_deletes_with_their_guards(cx: &mut TestAppContext) {
    let home = crate::views::agents_state::tests::home_for("page-menu", "alpha");
    let dir = agents_dir(&home);
    let mut s = Settings::open_agents(cx);

    // A shipped agent can't be deleted, and one still the default can't be
    // reset: both items are inert.
    s.click("agent-more-developer");
    assert!(s.above("agent-edit-yaml-developer", "agent-duplicate-developer"), "Edit YAML comes first");
    s.click("agent-reset-developer");
    s.click("agent-delete-developer");
    assert!(!s.drawn("agent-confirm"), "nothing to confirm");
    // Duplicate types the copy's name into the new row.
    s.click("agent-duplicate-developer");
    let draft = s.read(|w, cx| match &w.route {
        Route::Settings(view) => view.read(cx).agents().read(cx).draft_input(),
        _ => unreachable!(),
    });
    assert_eq!(s.read(|_, cx| draft.read(cx).value().to_string()), "developer-copy");
    s.h.key("enter");
    s.saved();
    assert!(dir.join("developer-copy").join("instructions.md").exists());
    // The copy opens in the details editor; Back shows its row.
    assert_eq!(s.agent_title(), "developer-copy");
    s.click("agent-editor-back");
    assert!(s.drawn("agent-row-developer-copy"));

    // A customized shipped agent resets, once confirmed.
    s.click("agent-more-orchestrator");
    s.click("agent-reset-orchestrator");
    assert!(s.drawn("agent-confirm"));
    s.click("agent-confirm-cancel");
    assert!(!s.drawn("agent-confirm"));
    assert!(s.drawn("agent-customized-orchestrator"));
    s.click("agent-more-orchestrator");
    s.click("agent-reset-orchestrator");
    s.click("agent-confirm-reset");
    s.saved();
    assert!(!std::fs::read_to_string(dir.join("orchestrator/instructions.md")).unwrap().contains("release notes"));
    assert!(!s.drawn("agent-customized-orchestrator"), "the badge goes");

    // The user's own agent deletes, once confirmed.
    s.click("agent-more-developer-copy");
    s.click("agent-delete-developer-copy");
    s.click("agent-confirm-delete");
    s.saved();
    assert!(!dir.join("developer-copy").exists());
    assert!(!s.drawn("agent-row-developer-copy"));
}

#[gpui::test]
fn edit_yaml_on_an_agent_opens_its_manifest_and_the_page_rereads_it(cx: &mut TestAppContext) {
    let home = crate::views::agents_state::tests::home_for("page-edit-yaml", "alpha");
    let file = agents_dir(&home).join("security").join("agent.yaml");
    let mut s = Settings::open_agents(cx);
    s.click("agent-more-security");
    s.click("agent-edit-yaml-security");
    s.saved();
    assert_eq!(opened(&file), 1);
    assert!(!s.drawn("agent-menu"), "the menu closes");
    assert!(s.edited_agents().is_empty());

    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace("Reviews a diff for defensive security issues", "Edited by hand")).unwrap();
    s.come_back();
    let page = s.agents();
    let description = s.read(|_, cx| {
        page.read(cx).state().agents().into_iter().find(|a| a.name == "security").and_then(|a| a.description)
    });
    assert_eq!(description.as_deref(), Some("Edited by hand"));
    assert!(s.drawn("agent-customized-security"), "a hand edit customizes it");
}

#[gpui::test]
fn edit_instructions_edits_and_saves_the_shared_instructions(cx: &mut TestAppContext) {
    let home = crate::views::agents_state::tests::home_for("page-shared", "alpha");
    let path = agents_dir(&home).join("_shared").join("preamble.md");
    let mut s = Settings::open_agents(cx);
    s.click("agents-edit-instructions");
    assert!(s.drawn("agents-shared-editor") && !s.drawn("agents-shared-preview"));
    assert!(!s.drawn("agents-edit-instructions"), "Cancel and Save stand in its place");
    let header = s.bounds("agents-shared-save").unwrap();
    assert!(header.bottom() <= s.bounds("agents-shared-editor").unwrap().top(), "above the text area");
    let page = s.agents();
    let input = s.read(|_, cx| page.read(cx).shared_input().unwrap());
    assert_eq!(s.read(|_, cx| input.read(cx).value().to_string()), crate::views::agents_state::fixture::PREAMBLE);

    // Cancel keeps the file.
    s.update(|_, window, cx| input.update(cx, |t, cx| t.set_value("Thrown away.", window, cx)));
    s.click("agents-shared-cancel");
    assert!(s.drawn("agents-shared-preview"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), crate::views::agents_state::fixture::PREAMBLE);

    s.click("agents-edit-instructions");
    let input = s.read(|_, cx| page.read(cx).shared_input().unwrap());
    s.update(|_, window, cx| input.update(cx, |t, cx| t.set_value("## Ground rules\n\nBe kind.\n", window, cx)));
    s.click("agents-shared-save");
    s.saved();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "## Ground rules\n\nBe kind.\n");
    assert!(s.drawn("agents-shared-preview"), "back to the preview");
    assert_eq!(AgentsConfig::load("alpha").unwrap().shared_instructions(), "## Ground rules\n\nBe kind.\n");
}

use crate::views::agent_editor::AgentEditor;
use crate::views::agent_editor_state::{Change as AgentChange, Field as AgentField};

impl Settings {
    /// Settings ▸ Agents with agent `name` open in the details editor (by
    /// its row's edit button).
    fn open_agent(cx: &mut TestAppContext, name: &'static str) -> Settings {
        let mut s = Settings::open_agents(cx);
        let selector: &'static str = Box::leak(format!("agent-edit-{name}").into_boxed_str());
        s.click(selector);
        assert!(s.drawn("agent-editor"), "the editor opened");
        s
    }

    fn agent_editor(&self) -> Option<Entity<AgentEditor>> {
        self.read(|w, cx| match &w.route {
            Route::Settings(view) => view.read(cx).agent_editor(),
            _ => None,
        })
    }

    fn with_agent_editor<R>(&self, f: impl FnOnce(&AgentEditor, &App) -> R) -> R {
        let editor = self.agent_editor().expect("the agent editor is open");
        self.read(|_, cx| f(editor.read(cx), cx))
    }

    fn edit_agent(&mut self, f: impl FnOnce(&mut AgentEditor, &mut Window, &mut Context<AgentEditor>)) {
        let editor = self.agent_editor().expect("the agent editor is open");
        self.update(|_, window, cx| editor.update(cx, |e, cx| f(e, window, cx)));
        self.settle();
    }

    fn agent_dirty(&self) -> bool {
        self.with_agent_editor(|e, _| e.state().is_some_and(|s| s.is_dirty()))
    }

    fn agent_title(&self) -> String {
        self.with_agent_editor(|e, _| e.state().map(|s| s.name().to_string()).unwrap_or_default())
    }

    fn agent_error(&self, field: AgentField) -> Option<String> {
        self.with_agent_editor(|e, _| e.state().and_then(|s| s.error_at(&field)).map(str::to_string))
    }

    /// Type `text` into a text area as the user would: focused, emptied,
    /// then typed.
    fn type_into_area(&mut self, input: Entity<gpui_kit::component::input::TextareaState>, text: &str) {
        self.update(|_, window, cx| {
            let focus = gpui::Focusable::focus_handle(input.read(cx), cx);
            window.focus(&focus, cx);
            input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        });
        self.h.cx.simulate_input(text);
        self.settle();
    }
}

fn agent_on_disk(name: &str) -> shelbi_state::agents_config::Agent {
    AgentsConfig::load("alpha").unwrap().agent(name).unwrap().clone()
}

#[gpui::test]
fn the_agent_editor_takes_the_window_without_the_sidebar(cx: &mut TestAppContext) {
    let _home = crate::views::agents_state::tests::home_for("ad-shell", "alpha");
    let mut s = Settings::open_agent(cx, "developer");
    assert!(!s.drawn("column-settings") && !s.drawn("leading-column"), "no sidebar");
    let titlebar = s.bounds("agent-editor-titlebar").unwrap();
    assert_eq!((titlebar.top(), titlebar.size.height), (px(0.0), px(52.0)), "the window's top row");
    let back = s.bounds("agent-editor-back").unwrap();
    assert_eq!(back.left(), px(sidebar::BESIDE_TRAFFIC_LIGHTS), "beside the traffic lights");
    assert_eq!(s.agent_title(), "developer");
    assert!(s.drawn("agent-editor-badge-default"), "a shipped agent as shipped");
    assert!(s.drawn("agent-editor-save") && !s.drawn("agent-editor-discard"), "nothing to discard yet");
    for selector in [
        "agent-name",
        "agent-description",
        "agent-instructions",
        "agent-runner",
        "agent-model",
        "agent-effort",
        "agent-permissions",
        "agent-assignment-task-in-progress",
        "agent-workflows-link",
        "agent-editor-footer",
    ] {
        assert!(s.drawn(selector), "{selector}");
    }
    // The two columns: Instructions left of Runner preferences.
    assert!(s.bounds("agent-instructions").unwrap().right() < s.bounds("agent-runner").unwrap().left());
    let text = s.with_agent_editor(|e, cx| e.instructions_field().read(cx).value().to_string());
    assert_eq!(text, agent_on_disk("developer").instructions());

    // ⌘N doesn't switch pages under it; Back returns to the page, the
    // sidebar with it.
    s.key("cmd-1");
    assert!(s.drawn("agent-editor"));
    s.click("agent-editor-back");
    assert!(!s.drawn("agent-editor") && s.drawn("settings-agents") && s.drawn("column-settings"));
    assert_eq!((s.page(), s.selected_row()), (Some(SettingsPage::Agents), Some(SettingsPage::Agents)));

    // A customized agent says so.
    s.click("agent-edit-orchestrator");
    assert!(s.drawn("agent-editor-badge-customized"));
}

#[gpui::test]
fn agent_edits_wait_for_save_and_discard_reverts_them(cx: &mut TestAppContext) {
    let home = crate::views::agents_state::tests::home_for("ad-save", "alpha");
    let yaml = agents_dir(&home).join("developer").join("agent.yaml");
    let before = std::fs::read_to_string(&yaml).unwrap();
    let mut s = Settings::open_agent(cx, "developer");

    // A permission mode from its picker: dirty, nothing written.
    s.click("agent-permissions");
    assert!(s.drawn("agent-permissions-menu"));
    s.click("agent-permissions-acceptEdits");
    assert!(!s.drawn("agent-permissions-menu"));
    assert!(s.agent_dirty() && s.drawn("agent-editor-discard"));
    assert_eq!(std::fs::read_to_string(&yaml).unwrap(), before);

    // Discard reverts it.
    s.click("agent-editor-discard");
    assert!(!s.agent_dirty() && !s.drawn("agent-editor-discard"));

    // A description and instructions typed, then saved.
    let description = s.with_agent_editor(|e, _| e.description_field());
    s.type_into(description, "Ships one task at a time.");
    let instructions = s.with_agent_editor(|e, _| e.instructions_field());
    s.type_into_area(instructions, "# Developer\n\nShip it.\n");
    assert!(s.agent_dirty());
    s.click("agent-editor-save");
    s.saved();
    assert!(!s.agent_dirty());
    let a = agent_on_disk("developer");
    assert_eq!(a.description(), Some("Ships one task at a time."));
    assert_eq!(a.instructions(), "# Developer\n\nShip it.\n");
    assert!(std::fs::read_to_string(&yaml).unwrap().contains("##"), "the file's comments stay");
    assert!(s.drawn("agent-editor-badge-customized"), "edited now");
}

#[gpui::test]
fn runner_pickers_write_their_keys_and_the_defaults_remove_them(cx: &mut TestAppContext) {
    let _home = crate::views::agents_state::tests::home_for("ad-runner", "alpha");
    let mut s = Settings::open_agent(cx, "developer");
    s.click("agent-model");
    s.click("agent-model-opus");
    s.click("agent-effort");
    s.click("agent-effort-high");
    s.click("agent-editor-save");
    s.saved();
    let a = agent_on_disk("developer");
    assert_eq!(a.model(shelbi_core::RunnerKind::Claude), Some("opus"));
    assert_eq!(a.reasoning_effort(shelbi_core::RunnerKind::Claude), Some(shelbi_core::ReasoningEffort::High));

    s.click("agent-model");
    s.click("agent-model-default");
    s.click("agent-effort");
    s.click("agent-effort-default");
    s.click("agent-runner");
    s.click("agent-runner-default");
    s.click("agent-editor-save");
    s.saved();
    let a = agent_on_disk("developer");
    assert_eq!((a.model(shelbi_core::RunnerKind::Claude), a.reasoning_effort(shelbi_core::RunnerKind::Claude)), (None, None));
    assert_eq!(a.preferred_runner(), None);

    // A generic runner takes neither: both pickers are off.
    s.click("agent-runner");
    s.click("agent-runner-generic");
    s.click("agent-model");
    assert!(!s.drawn("agent-model-menu"), "no model for a generic runner");
}

#[gpui::test]
fn leaving_the_agent_editor_with_edits_asks_first(cx: &mut TestAppContext) {
    let _home = crate::views::agents_state::tests::home_for("ad-leave", "alpha");
    let mut s = Settings::open_agent(cx, "developer");
    s.edit_agent(|e, window, cx| e.make(AgentChange::Permissions(Some("plan".into())), window, cx));

    // Esc asks; Cancel stays with the edits.
    s.h.focus_window();
    s.key("escape");
    assert!(s.drawn("agent-leave-prompt"));
    s.click("agent-leave-cancel");
    assert!(!s.drawn("agent-leave-prompt") && s.drawn("agent-editor") && s.agent_dirty());

    // Back asks; Discard leaves without writing.
    s.click("agent-editor-back");
    s.click("agent-leave-discard");
    assert!(!s.drawn("agent-editor"));
    assert_eq!(agent_on_disk("developer").permissions_mode(), None);

    // Save writes, then leaves.
    s.click("agent-edit-developer");
    s.edit_agent(|e, window, cx| e.make(AgentChange::Permissions(Some("plan".into())), window, cx));
    s.click("agent-editor-back");
    s.click("agent-leave-save");
    s.saved();
    assert!(!s.drawn("agent-editor"));
    assert_eq!(agent_on_disk("developer").permissions_mode(), Some("plan"));

    // The shared instructions' link goes back to the page too.
    s.click("agent-edit-developer");
    s.click("agent-shared-instructions-link");
    assert!(!s.drawn("agent-editor") && s.drawn("agents-shared-preview"));
}

#[gpui::test]
fn skills_add_and_remove_from_the_editor(cx: &mut TestAppContext) {
    let home = crate::views::agents_state::tests::home_for("ad-skills", "alpha");
    let mut s = Settings::open_agent(cx, "security");
    let count = |s: &Settings| s.with_agent_editor(|e, _| e.state().map_or(0, |st| st.skills().len()));
    let before = count(&s);

    s.click("agent-add-skill");
    assert!(s.drawn("agent-skill-form"));
    let (name, description, _) = s.with_agent_editor(|e, _| e.skill_fields().unwrap());
    s.type_into(name, "triage");
    // No description: refused in the form, which stays open.
    let add = s.bounds("agent-skill-add").unwrap();
    s.h.cx.simulate_click(add.center(), gpui::Modifiers::none());
    s.settle();
    assert!(s.agent_error(AgentField::NewSkill).is_some() && s.drawn("agent-skill-form"));
    s.type_into(description, "Sort new issues.");
    let add = s.bounds("agent-skill-add").unwrap();
    s.h.cx.simulate_click(add.center(), gpui::Modifiers::none());
    s.settle();
    assert!(!s.drawn("agent-skill-form"), "the form closes once added");
    assert_eq!(count(&s), before + 1);
    assert!(s.drawn("agent-skill-triage"));
    let file = agents_dir(&home).join("security").join("skills").join("triage").join("SKILL.md");
    assert!(!file.exists(), "not before Save");
    s.click("agent-editor-save");
    s.saved();
    assert!(std::fs::read_to_string(&file).unwrap().contains("description: Sort new issues."));

    // Remove, then Save.
    s.click("agent-skill-remove-triage");
    assert!(!s.drawn("agent-skill-triage") && s.agent_dirty());
    s.click("agent-editor-save");
    s.saved();
    assert!(!file.exists());

    // Cancel drops the form.
    s.click("agent-add-skill");
    s.click("agent-skill-cancel");
    assert!(!s.drawn("agent-skill-form"));
}

#[gpui::test]
fn assignment_rows_open_their_workflow_and_the_link_shows_workflows(cx: &mut TestAppContext) {
    let _home = crate::views::agents_state::tests::home_for("ad-assign", "alpha");
    let mut s = Settings::open_agent(cx, "developer");
    // The workflow editor opens over the Workflows page's model.
    let page = s.workflows();
    s.update(|_, window, cx| page.update(cx, |p, cx| p.load(window, cx)));
    s.settle();

    s.click("agent-assignment-task-in-progress");
    assert!(!s.drawn("agent-editor") && s.drawn("workflow-editor"));
    assert_eq!(s.title(), "task");
    // Closing it lands on Workflows, its sidebar row selected.
    s.click("workflow-editor-back");
    assert_eq!((s.page(), s.selected_row()), (Some(SettingsPage::Workflows), Some(SettingsPage::Workflows)));

    // Manage assignments in Workflows.
    s.key("cmd-6");
    s.click("agent-edit-developer");
    s.click("agent-workflows-link");
    assert!(!s.drawn("agent-editor") && s.drawn("settings-workflows"));
    assert_eq!((s.page(), s.selected_row()), (Some(SettingsPage::Workflows), Some(SettingsPage::Workflows)));

    // With unsaved edits, a row asks first.
    s.key("cmd-6");
    s.click("agent-edit-developer");
    s.edit_agent(|e, window, cx| e.make(AgentChange::Permissions(Some("plan".into())), window, cx));
    s.click("agent-assignment-task-in-progress");
    assert!(s.drawn("agent-leave-prompt") && !s.drawn("workflow-editor"));
    s.click("agent-leave-discard");
    assert!(s.drawn("workflow-editor"));
}

#[gpui::test]
fn the_agent_menu_guards_reset_and_delete_and_duplicates(cx: &mut TestAppContext) {
    let home = crate::views::agents_state::tests::home_for("ad-menu", "alpha");
    let dir = agents_dir(&home);
    let mut s = Settings::open_agent(cx, "developer");

    // A shipped agent as shipped: Reset and Delete are inert.
    s.click("agent-editor-more");
    assert!(s.drawn("agent-editor-menu"));
    s.click("agent-editor-reset");
    s.click("agent-editor-delete");
    assert!(!s.drawn("agent-delete-prompt") && !s.agent_dirty());
    s.h.focus_window();
    s.key("escape");
    assert!(!s.drawn("agent-editor-menu") && s.drawn("agent-editor"), "Esc closes the menu first");

    // Edit YAML opens the manifest.
    s.click("agent-editor-more");
    s.click("agent-editor-edit-yaml");
    s.saved();
    assert_eq!(opened(&dir.join("developer").join("agent.yaml")), 1);

    // Duplicate saves a copy and edits it; a copy can be deleted, once
    // confirmed.
    s.click("agent-editor-more");
    s.click("agent-editor-duplicate");
    s.saved();
    assert_eq!(s.agent_title(), "developer-copy");
    assert!(dir.join("developer-copy").join("instructions.md").exists());
    assert!(!s.drawn("agent-editor-badge-default") && !s.drawn("agent-editor-badge-customized"), "the user's own");
    s.click("agent-editor-more");
    s.click("agent-editor-delete");
    assert!(s.drawn("agent-delete-prompt"));
    s.click("agent-delete-cancel");
    assert!(dir.join("developer-copy").exists());
    s.click("agent-editor-more");
    s.click("agent-editor-delete");
    s.click("agent-delete-confirm");
    s.saved();
    assert!(!s.drawn("agent-editor") && !dir.join("developer-copy").exists());

    // A customized agent resets into the draft.
    s.click("agent-edit-orchestrator");
    s.click("agent-editor-more");
    s.click("agent-editor-reset");
    assert!(s.agent_dirty());
    s.click("agent-editor-save");
    s.saved();
    assert!(s.drawn("agent-editor-badge-default"));
}

#[gpui::test]
fn a_rename_is_read_only_for_shipped_agents_and_waits_for_save(cx: &mut TestAppContext) {
    let home = crate::views::agents_state::tests::home_for("ad-rename", "alpha");
    AgentsConfig::load("alpha").unwrap().create_agent("docs", None).unwrap().save().unwrap();
    let mut s = Settings::open_agent(cx, "docs");
    let name = s.with_agent_editor(|e, _| e.name_field());
    s.type_into(name, "writer");
    assert!(s.agent_dirty());
    assert!(agents_dir(&home).join("docs").exists(), "not before Save");
    s.click("agent-editor-save");
    s.saved();
    assert_eq!(s.agent_title(), "writer");
    assert!(agents_dir(&home).join("writer").join("agent.yaml").exists());

    // A shipped agent's name can't change: typing does nothing.
    s.click("agent-editor-back");
    s.click("agent-edit-qa");
    let name = s.with_agent_editor(|e, _| e.name_field());
    s.update(|_, window, cx| {
        let focus = gpui::Focusable::focus_handle(name.read(cx), cx);
        window.focus(&focus, cx);
    });
    s.h.cx.simulate_input("tester");
    s.settle();
    assert!(!s.agent_dirty());
    assert_eq!(s.with_agent_editor(|e, cx| e.name_field().read(cx).value().to_string()), "qa");
}

// ---- Settings ▸ Workspaces (`dt-workspace-settings-index`) -------------------

use crate::views::workspaces::WorkspacesPage;
use crate::views::workspaces_state::tests::{write_pool, POOL};
use crate::views::workspaces_state::{probe_key, Intent, Load};

impl Settings {
    /// Settings on Workspaces (⌘5), its page loaded from the test's home.
    fn open_workspaces(cx: &mut TestAppContext) -> Settings {
        let mut s = Settings::open(cx);
        s.open_settings();
        s.key("cmd-5");
        s.load_workspaces();
        s
    }

    fn load_workspaces(&mut self) {
        let page = self.workspaces();
        self.update(|_, window, cx| page.update(cx, |p, cx| p.load(window, cx)));
        self.saved();
    }

    fn workspaces(&self) -> Entity<WorkspacesPage> {
        self.read(|w, cx| match &w.route {
            Route::Settings(view) => view.read(cx).workspaces(),
            _ => panic!("Settings is open"),
        })
    }

    fn pool_forms(&self) -> Vec<Intent> {
        self.read(|w, _| w.pool_forms.clone())
    }

    fn workspace_tags(&self, name: &str) -> Vec<String> {
        let page = self.workspaces();
        self.read(|_, cx| match page.read(cx).state().load() {
            Load::Loaded(index) => index
                .workspaces
                .iter()
                .find(|w| w.name == name)
                .map(|w| w.tags.iter().map(|t| t.name.clone()).collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        })
    }
}

/// A pool small enough that both sections fit the test window.
const SMALL_POOL: &str = "\
machines:
  - name: This Mac
    kind: local
    work_dir: /tmp/my-project
  - name: Build server
    kind: ssh
    host: build-server
    work_dir: /home/me/my-project
    tags: [linux]
workspaces:
  - name: alpha
    machine: This Mac
    tags: [frontend]
  - name: charlie
    machine: Build server
";

#[gpui::test]
fn workspaces_sits_before_workflows_on_its_own_chord(cx: &mut TestAppContext) {
    let mut s = Settings::open(cx);
    s.open_settings();
    let switcher = s.bounds("sidebar-project-title").unwrap();
    let workspaces = s.bounds("settings-nav-workspaces").unwrap();
    let workflows = s.bounds("settings-nav-workflows").unwrap();
    assert!(switcher.bottom() < workspaces.top() && workspaces.bottom() < workflows.top());
    s.key("cmd-5");
    assert_eq!((s.page(), s.selected_row()), (Some(SettingsPage::Workspaces), Some(SettingsPage::Workspaces)));
    assert!(s.drawn("settings-page-workspaces") && s.drawn("workspaces-heading"));
    s.key("cmd-3");
    assert_eq!(s.page(), Some(SettingsPage::Terminal), "⌘3 stays Terminal's");
    s.click("settings-nav-workspaces");
    assert_eq!(s.page(), Some(SettingsPage::Workspaces));

    // Back from it returns to the view, as from any page.
    s.key("escape");
    assert_eq!(s.page(), None);
}

#[gpui::test]
fn the_workspaces_page_lists_the_pool_then_its_machines(cx: &mut TestAppContext) {
    let home = Home::with_project("ws-rows", "alpha");
    write_pool(&home.root, "alpha", POOL);
    let mut s = Settings::open(cx);
    s.open_settings();
    s.key("cmd-5");
    assert!(s.drawn("workspaces-loading"), "loading until the read lands");
    s.load_workspaces();
    assert!(!s.drawn("workspaces-loading"));
    for name in ["alpha", "bravo", "review", "charlie", "delta"] {
        let selector: &'static str = Box::leak(format!("workspace-row-{name}").into_boxed_str());
        assert!(s.drawn(selector), "{name}");
    }
    let alpha = s.bounds("workspace-row-alpha").unwrap();
    let delta = s.bounds("workspace-row-delta").unwrap();
    assert!(alpha.top() < delta.top(), "in declaration order");
    assert_eq!(s.workspace_tags("charlie"), ["linux"], "the machine's tag is charlie's");
    assert!(s.drawn("workspace-tag-charlie-linux"));
    assert!(s.drawn("workspace-tags-bravo"));
    assert!(s.drawn("workspace-slot-review"));
    let table = s.bounds("workspaces-table").unwrap();
    let heading = s.bounds("machines-heading").unwrap();
    assert!(table.bottom() < heading.top(), "Machines under the table");
    assert!(!s.drawn("workspaces-empty") && !s.drawn("workspaces-failed"));
}

#[gpui::test]
fn a_remote_that_cant_be_reached_keeps_its_configuration(cx: &mut TestAppContext) {
    let home = Home::with_project("ws-down", "alpha");
    write_pool(&home.root, "alpha", SMALL_POOL);
    let mut s = Settings::open_workspaces(cx);
    let page = s.workspaces();
    let status = |s: &Settings| {
        s.read(|_, cx| {
            let state = page.read(cx).state();
            let m = state.index().unwrap().machines[1].clone();
            state.status(&m).label()
        })
    };
    assert_eq!(status(&s), "Checking…");
    assert!(s.drawn("machine-status-Build server") && s.drawn("machine-dir-Build server"));
    s.update(|_, _, cx| {
        page.update(cx, |p, cx| {
            p.state_mut().set_reachability(std::collections::HashMap::from([(
                probe_key("Build server", "build-server"),
                shelbi_core::Reachability::Unreachable {
                    error: "ssh: connect to host build-server port 22: Operation timed out".into(),
                },
            )]));
            cx.notify();
        })
    });
    s.settle();
    assert_eq!(status(&s), "Unreachable");
    assert!(s.drawn("machine-error-Build server"), "SSH's reason under the machine");
    assert!(s.drawn("machine-dir-Build server") && s.drawn("machine-configure-Build server"));
    assert!(s.drawn("workspace-row-charlie"), "its workspaces stay listed");
    assert_eq!(status(&s), "Unreachable");
    s.key("cmd-4");
    assert_eq!(s.page(), Some(SettingsPage::Workflows), "the rest of Settings carries on");
}

#[gpui::test]
fn the_buttons_ask_the_window_for_their_forms(cx: &mut TestAppContext) {
    let home = Home::with_project("ws-intents", "alpha");
    let file = write_pool(&home.root, "alpha", SMALL_POOL);
    let mut s = Settings::open_workspaces(cx);
    s.click("workspaces-new");
    s.click("workspace-edit-alpha");
    s.click("workspace-row-charlie");
    s.click("workspace-more-alpha");
    assert!(s.drawn("workspace-menu"));
    s.click("workspace-menu-remove-alpha");
    assert!(!s.drawn("workspace-menu"), "the menu closes");
    s.click("machines-add");
    s.key("escape");
    s.click("machine-configure-Build server");
    s.saved();
    assert_eq!(
        s.pool_forms(),
        [
            Intent::NewWorkspace,
            Intent::EditWorkspace("alpha".into()),
            Intent::EditWorkspace("charlie".into()),
            Intent::RemoveWorkspace("alpha".into()),
            Intent::AddMachine,
            Intent::ConfigureMachine("Build server".into()),
        ]
    );
    // Until the workspace forms take theirs, each opens the file the pool
    // lives in; the machine intents open the machine form.
    assert_eq!(opened(&file), 4);
    assert!(s.drawn("workspaces-notice"));
    assert!(s.drawn("machine-form"));
}

#[gpui::test]
fn an_edit_shows_on_return_and_refreshes_the_window(cx: &mut TestAppContext) {
    let home = Home::with_project("ws-reread", "alpha");
    let file = write_pool(&home.root, "alpha", SMALL_POOL);
    let mut s = Settings::open_workspaces(cx);
    s.click("workspace-edit-alpha");
    s.saved();
    assert_eq!(s.read(|w, _| w.pool_refreshes), 0, "nothing changed yet");
    let edited = std::fs::read_to_string(&file).unwrap().replace("tags: [frontend]", "tags: [frontend, web]");
    std::fs::write(&file, edited).unwrap();
    s.come_back();
    assert_eq!(s.workspace_tags("alpha"), ["frontend", "web"]);
    assert_eq!(s.read(|w, _| w.pool_refreshes), 1, "the window rereads its feed");

    // A reread with nothing new doesn't.
    s.click("workspace-edit-alpha");
    s.come_back();
    assert_eq!(s.read(|w, _| w.pool_refreshes), 1);
}

#[gpui::test]
fn an_empty_pool_and_a_broken_config_have_their_own_states(cx: &mut TestAppContext) {
    let home = Home::with_project("ws-states", "alpha");
    let file = write_pool(&home.root, "alpha", "machines:\n  - name: hub\n    kind: local\n    work_dir: /tmp\nworkspaces: []\n");
    let mut s = Settings::open_workspaces(cx);
    assert!(s.drawn("workspaces-empty"), "no workspaces yet");
    assert!(s.drawn("machine-row-hub") && s.drawn("machine-status-hub"));
    assert!(!s.drawn("machine-error-hub"));

    std::fs::write(&file, "machines: [not yaml").unwrap();
    s.load_workspaces();
    assert!(s.drawn("workspaces-error"), "a failed reread keeps the rows, with its error");
    assert!(s.drawn("workspaces-empty"));

    // A fresh page over the broken file: the error and Retry in place of
    // the table, no machines, and the rest of Settings still works.
    s.key("escape");
    s.open_settings();
    let page = s.workspaces();
    s.load_workspaces();
    assert!(s.read(|_, cx| matches!(page.read(cx).state().load(), Load::Failed(_))));
    assert!(s.drawn("workspaces-failed") && s.drawn("workspaces-retry"));
    assert!(!s.drawn("workspaces-table") && !s.drawn("machines-heading"));
    write_pool(&home.root, "alpha", SMALL_POOL);
    s.click("workspaces-retry");
    s.saved();
    assert!(s.drawn("workspace-row-alpha") && s.drawn("machines-heading"));
}

#[gpui::test]
fn switching_projects_reloads_workspaces_for_the_new_project(cx: &mut TestAppContext) {
    let home = Home::with_project("ws-switch", "alpha");
    write_pool(&home.root, "alpha", SMALL_POOL);
    crate::views::workflows_state::tests::write_project(&home.root, "beta");
    let (hub, _feeds) = crate::feeds::tests::canned_hub();
    cx.update(|cx| cx.set_global(hub));
    let mut s = Settings::open_workspaces(cx);
    let before = s.workspaces();
    assert!(s.drawn("workspace-row-charlie"));
    s.update(|w, window, cx| w.switch_to("beta", window, cx));
    s.saved();
    assert_eq!(s.page(), Some(SettingsPage::Workspaces), "still on Workspaces");
    let after = s.workspaces();
    assert_ne!(before.entity_id(), after.entity_id(), "a fresh page");
    assert_eq!(s.read(|_, cx| after.read(cx).state().project().to_string()), "beta");
    assert!(!s.drawn("workspace-row-charlie"), "alpha's rows went with its page");
    s.load_workspaces();
    assert!(s.drawn("workspace-row-alpha") && !s.drawn("workspace-row-charlie"), "beta's one workspace");
}

#[gpui::test]
fn a_long_name_and_path_clip_inside_their_column(cx: &mut TestAppContext) {
    let home = Home::with_project("ws-long", "alpha");
    let long = "a-very-long-workspace-name-that-keeps-going-well-past-its-column-and-then-some-more";
    write_pool(
        &home.root,
        "alpha",
        &format!(
            "machines:\n  - name: hub\n    kind: local\n    work_dir: /tmp/{0}/{0}/{0}\nworkspaces:\n  - name: {long}\n    machine: hub\n",
            "deeply-nested-directory-name-for-the-project"
        ),
    );
    let mut s = Settings::open_workspaces(cx);
    let name: &'static str = Box::leak(format!("workspace-name-{long}").into_boxed_str());
    let path: &'static str = Box::leak(format!("workspace-path-{long}").into_boxed_str());
    let tags: &'static str = Box::leak(format!("workspace-tags-{long}").into_boxed_str());
    let tags = s.bounds(tags).expect("the tags column");
    for selector in [name, path] {
        let b = s.bounds(selector).expect(selector);
        assert!(b.right() < tags.left(), "{selector} stops short of the columns after it");
    }
    let row: &'static str = Box::leak(format!("workspace-row-{long}").into_boxed_str());
    let table = s.bounds("workspaces-table").unwrap();
    assert!(s.bounds(row).unwrap().right() <= table.right());
}

// ---- Add machine / Configure machine (`dt-machine-settings-forms`) ---------

use crate::views::machine_form::tests::{recording_readiness, save_with, QuietOps};
use crate::views::machine_form::{Engine as MachineEngine, MachineFormView};
use crate::views::machine_form_state::{Check, CheckOutcome, Field as MachineField, ReadinessIntent};

impl Settings {
    /// Settings ▸ Workspaces over `pool`, the machine form saving through
    /// the shared service over `ops`.
    fn open_machines(cx: &mut TestAppContext, ops: QuietOps) -> Settings {
        cx.update(|cx| {
            cx.set_global(MachineEngine {
                save: save_with(ops),
                readiness: recording_readiness(CheckOutcome::Reachable).0,
            })
        });
        Settings::open_workspaces(cx)
    }

    fn machine_form(&self) -> Option<Entity<MachineFormView>> {
        self.read(|w, cx| match &w.route {
            Route::Settings(view) => view.read(cx).machine_form(),
            _ => None,
        })
    }

    fn form(&self) -> Entity<MachineFormView> {
        self.machine_form().expect("the machine form is open")
    }

    fn form_text(&self, field: MachineField) -> String {
        let form = self.form();
        self.read(|_, cx| form.read(cx).input(field).read(cx).value().to_string())
    }

    fn type_in_form(&mut self, field: MachineField, text: &str) {
        let input = {
            let form = self.form();
            self.read(|_, cx| form.read(cx).input(field).clone())
        };
        self.type_into(input, text);
    }

    fn form_focused(&mut self, field: MachineField) -> bool {
        let form = self.form();
        let handle = self.read(|_, cx| gpui::Focusable::focus_handle(form.read(cx).input(field).read(cx), cx));
        self.h.cx.update(|window, _| handle.is_focused(window))
    }

    fn machine_names(&self) -> Vec<String> {
        let page = self.workspaces();
        self.read(|_, cx| match page.read(cx).state().load() {
            Load::Loaded(index) => index.machines.iter().map(|m| m.name.clone()).collect(),
            _ => Vec::new(),
        })
    }
}

#[gpui::test]
fn add_machine_saves_a_validated_ssh_draft(cx: &mut TestAppContext) {
    let home = Home::with_project("mf-add", "alpha");
    let file = write_pool(&home.root, "alpha", SMALL_POOL);
    let mut s = Settings::open_machines(cx, QuietOps::default());
    s.click("machines-add");
    assert!(s.drawn("machine-form-layer") && s.drawn("machine-form"));
    assert!(s.drawn("machine-host") && s.drawn("machine-check"), "an SSH draft");
    for field in [MachineField::Name, MachineField::Host, MachineField::WorkDir, MachineField::Tags] {
        assert_eq!(s.form_text(field), "", "no sample values");
    }
    assert!(s.form_focused(MachineField::Name), "the keyboard starts on Name");
    assert_eq!(s.pool_forms(), [Intent::AddMachine]);
    assert_eq!(opened(&file), 0, "no YAML file opened");

    s.type_in_form(MachineField::Name, "GPU box");
    s.type_in_form(MachineField::Host, "gpu-alias");
    s.type_in_form(MachineField::WorkDir, "/srv/my project");
    s.type_in_form(MachineField::Tags, "gpu, linux");
    let refreshes = s.read(|w, _| w.pool_refreshes);
    s.click("machine-submit");
    s.saved();

    assert!(s.machine_form().is_none(), "the form closed");
    let p = shelbi_state::load_project("alpha").unwrap();
    let m = p.machine("GPU box").expect("declared");
    assert_eq!(m.kind, shelbi_core::MachineKind::Ssh);
    assert_eq!(m.host.as_deref(), Some("gpu-alias"), "an SSH config alias, as typed");
    assert_eq!(m.work_dir, std::path::PathBuf::from("/srv/my project"));
    assert_eq!(m.tags, ["gpu", "linux"]);
    assert_eq!(s.machine_names(), ["This Mac", "Build server", "GPU box"], "the page reread");
    assert!(s.drawn("machine-row-GPU box"));
    assert_eq!(s.read(|w, _| w.pool_refreshes), refreshes + 1, "the sidebar follows");
    assert!(s.window_focused(), "the keys go back where they were");
}

#[gpui::test]
fn configure_opens_the_machines_values_and_saves_its_tags_for_its_workspaces(cx: &mut TestAppContext) {
    let home = Home::with_project("mf-configure", "alpha");
    let file = write_pool(
        &home.root,
        "alpha",
        &SMALL_POOL.replace("    tags: [linux]\n", "    tags: [linux]\n    forward: tcp # pinned by hand\n"),
    );
    let mut s = Settings::open_machines(cx, QuietOps::default());
    s.click("machine-configure-Build server");
    assert_eq!(s.form_text(MachineField::Name), "Build server");
    assert_eq!(s.form_text(MachineField::Host), "build-server");
    assert_eq!(s.form_text(MachineField::WorkDir), "/home/me/my-project");
    assert_eq!(s.form_text(MachineField::Tags), "linux");
    let description = s.read(|_, cx| s.form().read(cx).state().description());
    assert_eq!(description, "Build server runs 1 workspace for this project.");

    s.type_in_form(MachineField::Tags, "linux, gpu");
    s.click("machine-submit");
    s.saved();
    assert!(s.machine_form().is_none());
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("forward: tcp # pinned by hand"), "the field the form doesn't own survives:\n{text}");
    let p = shelbi_state::load_project("alpha").unwrap();
    assert_eq!(p.machine("Build server").unwrap().forward, Some(shelbi_core::ForwardMode::Tcp));
    assert_eq!(s.workspace_tags("charlie"), ["gpu", "linux"], "charlie inherits the new tag");
}

#[gpui::test]
fn this_macs_form_has_no_ssh_and_chooses_its_folder(cx: &mut TestAppContext) {
    let home = Home::with_project("mf-local", "alpha");
    write_pool(&home.root, "alpha", SMALL_POOL);
    let mut s = Settings::open_machines(cx, QuietOps::default());
    s.click("machine-configure-This Mac");
    assert!(s.drawn("machine-name") && s.drawn("machine-work-dir") && s.drawn("machine-browse"));
    assert!(!s.drawn("machine-host") && !s.drawn("machine-check"), "nothing SSH");
    assert_eq!(s.form_text(MachineField::WorkDir), "/tmp/my-project");
    let form = s.form();
    let picked = std::env::temp_dir().join("My Projects").join("app");
    s.update(|_, window, cx| form.update(cx, |f, cx| f.folder_picked(Some(&picked), window, cx)));
    s.settle();
    s.click("machine-submit");
    s.saved();
    let p = shelbi_state::load_project("alpha").unwrap();
    let m = p.machine("This Mac").unwrap();
    assert_eq!((m.kind, m.work_dir.clone()), (shelbi_core::MachineKind::Local, picked), "still local");
}

#[gpui::test]
fn a_local_tilde_expands_here_and_a_remote_one_is_refused(cx: &mut TestAppContext) {
    let home = Home::with_project("mf-tilde", "alpha");
    let file = write_pool(&home.root, "alpha", SMALL_POOL);
    let before = std::fs::read_to_string(&file).unwrap();
    let mut s = Settings::open_machines(cx, QuietOps::default());
    s.click("machines-add");
    s.type_in_form(MachineField::Name, "box");
    s.type_in_form(MachineField::WorkDir, "~/my project");
    s.click("machine-submit");
    s.saved();
    assert!(s.machine_form().is_some(), "the draft stays");
    assert!(s.drawn("machine-work-dir-error"), "never expanded against this Mac's home");
    assert_eq!(s.form_text(MachineField::WorkDir), "~/my project");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "nothing written");
    s.key("escape");

    s.click("machine-configure-This Mac");
    s.type_in_form(MachineField::WorkDir, "~/my project");
    s.click("machine-submit");
    s.saved();
    let home_dir = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let p = shelbi_state::load_project("alpha").unwrap();
    assert_eq!(p.machine("This Mac").unwrap().work_dir, home_dir.join("my project"));
}

#[gpui::test]
fn a_duplicate_name_keeps_the_draft_with_its_error(cx: &mut TestAppContext) {
    let home = Home::with_project("mf-dup", "alpha");
    let file = write_pool(&home.root, "alpha", SMALL_POOL);
    let before = std::fs::read_to_string(&file).unwrap();
    let mut s = Settings::open_machines(cx, QuietOps::default());
    s.click("machines-add");
    s.type_in_form(MachineField::Name, "build server");
    s.type_in_form(MachineField::Host, "other-host");
    s.type_in_form(MachineField::WorkDir, "/srv/p");
    s.key("enter");
    s.saved();
    assert!(s.drawn("machine-name-error"), "on the field");
    assert!(s.form_focused(MachineField::Name), "the keyboard goes to it");
    assert_eq!(s.form_text(MachineField::Host), "other-host", "the draft stays");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    s.type_in_form(MachineField::Name, "build server 2");
    assert!(!s.drawn("machine-name-error"), "editing clears it");
}

#[gpui::test]
fn moving_a_machine_with_an_active_workspace_is_refused(cx: &mut TestAppContext) {
    let home = Home::with_project("mf-unsafe", "alpha");
    let file = write_pool(&home.root, "alpha", SMALL_POOL);
    let before = std::fs::read_to_string(&file).unwrap();
    let ops = QuietOps {
        active: Some(("charlie".into(), "t-1".into())),
        ..QuietOps::default()
    };
    let mut s = Settings::open_machines(cx, ops);
    s.click("machine-configure-Build server");
    s.type_in_form(MachineField::WorkDir, "/srv/elsewhere");
    s.click("machine-submit");
    s.saved();
    assert!(s.drawn("machine-form-error"), "why, over the actions");
    assert_eq!(s.form_text(MachineField::WorkDir), "/srv/elsewhere");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "no workspace orphaned");

    // Its rename alone is safe: its workspaces follow it.
    s.type_in_form(MachineField::WorkDir, "/home/me/my-project");
    s.type_in_form(MachineField::Name, "Builder");
    s.click("machine-submit");
    s.saved();
    assert!(s.machine_form().is_none());
    let p = shelbi_state::load_project("alpha").unwrap();
    assert_eq!(p.workspace("charlie").unwrap().machine, "Builder");
}

#[gpui::test]
fn a_change_made_elsewhere_meanwhile_is_a_conflict(cx: &mut TestAppContext) {
    let home = Home::with_project("mf-conflict", "alpha");
    let file = write_pool(&home.root, "alpha", SMALL_POOL);
    let mut s = Settings::open_machines(cx, QuietOps::default());
    s.click("machine-configure-Build server");
    let elsewhere = std::fs::read_to_string(&file).unwrap().replace("tags: [linux]", "tags: [linux, arm]");
    std::fs::write(&file, &elsewhere).unwrap();
    s.type_in_form(MachineField::Tags, "x86");
    s.click("machine-submit");
    s.saved();
    assert!(s.drawn("machine-form-error"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), elsewhere, "the other edit isn't overwritten");
    assert_eq!(s.form_text(MachineField::Tags), "x86");
}

#[gpui::test]
fn cancel_and_escape_discard_the_draft(cx: &mut TestAppContext) {
    let home = Home::with_project("mf-cancel", "alpha");
    let file = write_pool(&home.root, "alpha", SMALL_POOL);
    let before = std::fs::read_to_string(&file).unwrap();
    let mut s = Settings::open_machines(cx, QuietOps::default());
    s.click("machines-add");
    s.type_in_form(MachineField::Name, "box");
    s.key("escape");
    assert!(s.machine_form().is_none());
    assert_eq!(s.page(), Some(SettingsPage::Workspaces), "Esc closed only the form");
    assert!(s.window_focused());
    s.click("machine-configure-Build server");
    s.type_in_form(MachineField::Tags, "zzz");
    s.click("machine-cancel");
    assert!(s.machine_form().is_none());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);

    // A fresh form doesn't remember the discarded draft.
    s.click("machines-add");
    assert_eq!(s.form_text(MachineField::Name), "");
}

#[gpui::test]
fn the_keyboard_moves_through_the_form(cx: &mut TestAppContext) {
    let home = Home::with_project("mf-keys", "alpha");
    write_pool(&home.root, "alpha", SMALL_POOL);
    let mut s = Settings::open_machines(cx, QuietOps::default());
    s.click("machines-add");
    s.key("tab");
    assert!(s.form_focused(MachineField::Host));
    s.key("tab");
    assert!(s.form_focused(MachineField::WorkDir));
    s.key("shift-tab");
    s.key("shift-tab");
    assert!(s.form_focused(MachineField::Name));
    s.key("cmd-4");
    assert_eq!(s.page(), Some(SettingsPage::Workspaces), "the sidebar's chords wait for the form");
    assert!(s.machine_form().is_some());
}

#[gpui::test]
fn a_connection_check_writes_nothing_and_goes_stale_with_the_host(cx: &mut TestAppContext) {
    let home = Home::with_project("mf-check", "alpha");
    let file = write_pool(&home.root, "alpha", SMALL_POOL);
    let before = std::fs::read_to_string(&file).unwrap();
    let (readiness, seen) = recording_readiness(CheckOutcome::NeedsCli {
        message: "shelbi isn’t installed on build-server".into(),
    });
    cx.update(|cx| {
        cx.set_global(MachineEngine {
            save: save_with(QuietOps::default()),
            readiness,
        })
    });
    let mut s = Settings::open_workspaces(cx);
    s.click("machine-configure-Build server");
    let check = |s: &Settings| s.read(|_, cx| s.form().read(cx).state().check().clone());
    assert_eq!(check(&s), Check::NotChecked, "never the successful fixture");
    assert!(!s.drawn("machine-install-cli"));
    s.click("machine-test");
    s.saved();
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].intent, ReadinessIntent::Test);
    assert_eq!(requests[0].target.host, "build-server");
    assert_eq!(requests[0].target.machine.as_deref(), Some("Build server"));
    assert!(s.drawn("machine-install-cli"), "the check asks for the CLI");
    s.click("machine-install-cli");
    s.saved();
    assert_eq!(seen.lock().unwrap()[1].intent, ReadinessIntent::InstallCli);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "checks never touch the config");

    s.type_in_form(MachineField::Host, "build-2");
    assert_eq!(check(&s), Check::NotChecked, "a new host isn't checked");
    assert!(!s.drawn("machine-install-cli"));
}

#[gpui::test]
fn switching_projects_closes_the_form(cx: &mut TestAppContext) {
    let home = Home::with_project("mf-switch", "alpha");
    write_pool(&home.root, "alpha", SMALL_POOL);
    crate::views::workflows_state::tests::write_project(&home.root, "beta");
    let (hub, _feeds) = crate::feeds::tests::canned_hub();
    cx.update(|cx| cx.set_global(hub));
    let mut s = Settings::open_machines(cx, QuietOps::default());
    s.click("machines-add");
    s.type_in_form(MachineField::Name, "box");
    s.update(|w, window, cx| w.switch_to("beta", window, cx));
    s.saved();
    assert!(s.machine_form().is_none(), "alpha's draft went with its page");
    assert!(!s.drawn("machine-form-layer"));
    let beta = shelbi_state::load_project("beta").unwrap();
    assert!(beta.machine("box").is_none());
}

#[gpui::test]
fn an_in_repo_project_saves_machines_to_its_local_half(cx: &mut TestAppContext) {
    let home = Home::with_project("mf-in-repo", "alpha");
    let projects = home.root.join("projects");
    let repo = home.root.join("repo");
    std::fs::create_dir_all(repo.join(".shelbi")).unwrap();
    let shared = "name: My project\ndefault_branch: main\ndefault_workflow: task\norchestrator:\n  runner: claude\n\
                  agent_runners:\n  claude:\n    command: claude\n    flags: []\n";
    std::fs::write(repo.join(".shelbi/project.yaml"), shared).unwrap();
    let local = projects.join("alpha").join("local.yaml");
    std::fs::write(&local, format!("repo: {}\n{SMALL_POOL}", repo.display())).unwrap();
    std::fs::remove_file(projects.join("alpha.yaml")).unwrap();
    let mut s = Settings::open_machines(cx, QuietOps::default());
    s.click("machines-add");
    s.type_in_form(MachineField::Name, "arm box");
    s.type_in_form(MachineField::Host, "arm");
    s.type_in_form(MachineField::WorkDir, "/srv/p");
    s.click("machine-submit");
    s.saved();
    assert!(s.machine_form().is_none());
    assert!(std::fs::read_to_string(&local).unwrap().contains("arm box"));
    assert_eq!(std::fs::read_to_string(repo.join(".shelbi/project.yaml")).unwrap(), shared, "the committed half is untouched");
    assert!(s.drawn("machine-row-arm box"));
}
