//! The view through its real paths: keys down the focus path, clicks on what's
//! drawn, the session's worker threads (over a patch in memory).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowAppearance};
use shelbi_app::diff::{HunkFilter, TargetKind, ThreadPart, ViewMode};
use shelbi_app::theme::ThemePreference;

use super::canned::{Opened, SAMPLE_PATCH};
use super::*;

struct Host {
    view: gpui::Entity<DiffView>,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().child(self.view.clone())
    }
}

struct Harness {
    view: gpui::Entity<DiffView>,
    cx: VisualTestContext,
    opened: Opened,
}

fn target() -> DiffTarget {
    DiffTarget { project: "demo".into(), workspace: "review-1".into(), kind: TargetKind::Review, task: Some("t".into()) }
}

fn open_with(cx: &mut TestAppContext, patch: &str, place: FilePlace) -> Harness {
    cx.update(|cx| {
        cx.set_global(Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark));
        gpui_kit::init(cx);
    });
    let opened: Opened = Arc::default();
    let deps = DiffDeps::canned(patch, opened.clone());
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            let view = cx.new(|cx| DiffView::new(target(), place.clone(), &deps, cx));
            cx.new(|_| Host { view })
        })
        .unwrap()
    });
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    let view = window.root(&mut vcx).unwrap().read_with(&vcx, |h, _| h.view.clone());
    vcx.update(|window, cx| {
        let handle = view.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    });
    let mut h = Harness { view, cx: vcx, opened };
    h.wait(|s| !s.loading());
    h.draw();
    h
}

fn open(cx: &mut TestAppContext) -> Harness {
    open_with(cx, SAMPLE_PATCH, FilePlace::Local("/wt".into()))
}

impl Harness {
    /// Let the worker threads land until `done`.
    fn wait(&mut self, done: impl Fn(&DiffState) -> bool) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            self.look();
            if self.view.read_with(&self.cx, |v, _| done(v.state())) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the worker threads didn't land");
    }

    /// What the workers finished, in.
    fn look(&mut self) {
        self.cx.run_until_parked();
        self.cx.update(|_, cx| self.view.update(cx, |v, cx| v.poll_now(cx)));
        self.cx.run_until_parked();
    }

    fn draw(&mut self) {
        self.look();
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        self.cx.run_until_parked();
    }

    fn keys(&mut self, keys: &str) {
        self.cx.simulate_keystrokes(keys);
        self.cx.run_until_parked();
        self.draw();
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        self.draw();
        self.cx.debug_bounds(selector).is_some()
    }

    fn click(&mut self, selector: &'static str) {
        self.draw();
        let at = self.cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn")).center();
        self.cx.simulate_click(at, Modifiers::none());
        self.cx.run_until_parked();
    }

    fn read<R>(&self, f: impl FnOnce(&DiffView) -> R) -> R {
        self.view.read_with(&self.cx, |v, _| f(v))
    }

    fn cursor(&self) -> usize {
        self.read(|v| v.state().cursor())
    }

    fn file(&self) -> Option<usize> {
        self.read(|v| v.state().current_file())
    }

    fn mode(&self) -> &'static str {
        self.read(|v| v.state().mode().label())
    }

    fn row_count(&self) -> usize {
        self.read(|v| v.state().table().len())
    }
}

#[gpui::test]
fn it_opens_on_the_first_file_with_its_files_listed(cx: &mut TestAppContext) {
    let mut h = open(cx);
    assert_eq!(h.read(|v| v.state().changeset().files.len()), 5);
    assert!(h.drawn("diff-file-list"));
    assert!(h.drawn("diff-body"));
    assert!(h.drawn("diff-status"));
    assert!(h.drawn("diff-file-4"), "every file has its row");
    assert_eq!(files_summary(5, 8, 7), "Files 5 · +8 −7");
    assert_eq!(h.read(|v| v.state().status()), "NORMAL · unified · all lines · 1/5 · ? help");
}

#[gpui::test]
fn nothing_to_show_says_so(cx: &mut TestAppContext) {
    let mut h = open_with(cx, "", FilePlace::Local("/wt".into()));
    assert!(h.drawn("diff-empty"));
    assert!(!h.drawn("diff-file-list"));
}

#[gpui::test]
fn the_movement_keys_reach_the_state(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.keys("j");
    assert_eq!(h.cursor(), 1);
    h.keys("k");
    assert_eq!(h.cursor(), 0);
    // Counts, two-key bindings, shifted letters.
    h.keys("3 j");
    assert_eq!(h.cursor(), 3);
    h.keys("g g");
    assert_eq!(h.cursor(), 0);
    h.keys("shift-g");
    assert_eq!(h.cursor(), h.row_count() - 1);
    h.keys("g g");
    // Files.
    h.keys("l");
    assert_eq!(h.file(), Some(1));
    h.keys("h");
    assert_eq!(h.file(), Some(0));
    h.keys("ctrl-n");
    assert_eq!(h.file(), Some(1));
    h.keys("g g");
    // Pages, half pages.
    h.keys("ctrl-d");
    assert!(h.cursor() > 0);
    h.keys("ctrl-u");
    assert_eq!(h.cursor(), 0);
    h.keys("space");
    assert!(h.cursor() > 0);
    h.keys("b");
    assert_eq!(h.cursor(), 0);
    h.keys("ctrl-f");
    assert!(h.cursor() > 0);
    h.keys("pageup");
    assert_eq!(h.cursor(), 0);
    // Changes, and empty lines.
    h.keys("] h");
    let first_change = h.cursor();
    assert!(first_change > 0);
    h.keys("] h");
    assert!(h.cursor() > first_change);
    h.keys("[ h");
    assert_eq!(h.cursor(), first_change);
    h.keys("g g");
    h.keys("}");
    assert!(h.cursor() > 0, "the blank context line");
    h.keys("{");
    assert!(h.cursor() > 0, "none above it: it stays put");
}

#[gpui::test]
fn the_view_centers_on_the_cursor_and_the_cursor_on_the_view(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &long_patch(), FilePlace::Local("/wt".into()));
    h.keys("1 0 0 j");
    assert_eq!(h.cursor(), 100);
    h.keys("z z");
    let (top, height) = h.read(|v| (v.state().top(), v.state().visible_rows().len()));
    assert_eq!(top, 100 - height / 2);
    h.keys("g g");
    h.keys("shift-m");
    assert_eq!(h.cursor(), h.read(|v| v.state().visible_rows().len()) / 2);
}

#[gpui::test]
fn character_finds_move_along_the_line(cx: &mut TestAppContext) {
    let mut h = open(cx);
    // review.rs: `        self.task = Some(id);`
    h.keys("l");
    h.keys("] h");
    assert_eq!(h.read(|v| v.state().col()), 0);
    h.keys("f s");
    let first = h.read(|v| v.state().col());
    assert!(first > 0);
    h.keys(";");
    assert!(h.read(|v| v.state().col()) > first);
    h.keys("shift-f s");
    assert_eq!(h.read(|v| v.state().col()), first);
    h.keys("t =");
    assert!(h.read(|v| v.state().col()) > 0);
}

#[gpui::test]
fn the_view_keys_change_what_is_drawn(cx: &mut TestAppContext) {
    let mut h = open(cx);
    let unified = h.row_count();
    h.keys("s");
    assert_eq!(h.read(|v| v.state().view()), ViewMode::Split);
    assert!(h.row_count() < unified, "edits pair up");
    h.keys("s");
    assert_eq!(h.read(|v| v.state().view()), ViewMode::Unified);
    h.keys("tab");
    assert_eq!(h.read(|v| v.state().filter()), HunkFilter::Additions);
    h.keys("tab");
    assert_eq!(h.read(|v| v.state().filter()), HunkFilter::Deletions);
    h.keys("shift-tab");
    assert_eq!(h.read(|v| v.state().filter()), HunkFilter::Additions);
    h.keys("shift-tab");
    assert_eq!(h.read(|v| v.state().filter()), HunkFilter::All);
    h.keys("shift-b");
    assert!(h.read(|v| v.state().blame_on()));
    h.wait(|s| {
        let path = &s.changeset().files[0].path;
        s.blame_for(path).is_some()
    });
    h.draw();
}

#[gpui::test]
fn folds_hide_and_show_rows(cx: &mut TestAppContext) {
    let mut h = open(cx);
    let all = h.row_count();
    h.keys("] h");
    h.keys("z c");
    assert!(h.row_count() < all);
    h.keys("z o");
    assert_eq!(h.row_count(), all);
    h.keys("z a");
    assert!(h.row_count() < all);
    h.keys("z a");
    assert_eq!(h.row_count(), all);
    h.keys("z shift-c");
    assert!(h.row_count() < all);
    h.keys("z shift-o");
    assert_eq!(h.row_count(), all);
    h.keys("z shift-m");
    assert_eq!(h.row_count(), 5, "every file down to its header");
    h.keys("z shift-r");
    assert_eq!(h.row_count(), all);
}

#[gpui::test]
fn a_search_prompts_then_finds_across_files(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.keys("/");
    assert_eq!(h.mode(), "SEARCH");
    assert!(h.drawn("diff-search-prompt"));
    // A capital reaches the query as typed: smart case.
    h.keys("g o n e");
    assert_eq!(h.read(|v| match v.state().mode() {
        Mode::Search { query } => query.clone(),
        _ => String::new(),
    }), "gone");
    h.keys("enter");
    assert_eq!(h.mode(), "NORMAL");
    assert_eq!(h.read(|v| v.state().match_count()), 1);
    assert_eq!(h.file(), Some(2), "the deleted file");
    h.keys("escape");
    assert_eq!(h.read(|v| v.state().match_count()), 0, "Esc lets go of the marks");
    h.keys("/");
    h.keys("s e l f enter");
    let many = h.read(|v| v.state().match_count());
    assert!(many > 1);
    assert_eq!(h.read(|v| v.state().current_match()), Some(1));
    h.keys("n");
    assert_eq!(h.read(|v| v.state().current_match()), Some(2));
    h.keys("shift-n");
    assert_eq!(h.read(|v| v.state().current_match()), Some(1));
}

#[gpui::test]
fn the_file_palette_jumps_and_goes_over_to_commands(cx: &mut TestAppContext) {
    let mut h = open(cx);
    // The diff's Ctrl-p, ahead of the window's palette.
    h.keys("ctrl-p");
    assert_eq!(h.mode(), "PALETTE");
    assert!(h.drawn("diff-palette"));
    h.keys("o l d");
    assert!(h.drawn("diff-palette-selected"));
    h.keys("enter");
    assert_eq!(h.mode(), "NORMAL");
    assert_eq!(h.file(), Some(2));

    h.keys("ctrl-p");
    h.keys(">");
    assert!(h.read(|v| matches!(v.state().mode(), Mode::Palette(p) if p.mode == shelbi_app::diff::palette::PaletteMode::Commands)));
    h.keys("backspace");
    assert!(h.read(|v| matches!(v.state().mode(), Mode::Palette(p) if p.mode == shelbi_app::diff::palette::PaletteMode::Files)));
    h.keys("escape");
    assert_eq!(h.mode(), "NORMAL");
}

#[gpui::test]
fn a_palette_row_clicks(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.keys(":");
    assert!(h.drawn("diff-palette"));
    // Unified or split is the first command.
    h.click("diff-palette-row-0");
    assert_eq!(h.mode(), "NORMAL");
    assert_eq!(h.read(|v| v.state().view()), ViewMode::Split);
}

#[gpui::test]
fn the_commit_picker_reads_one_commit(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.click("diff-commits");
    assert_eq!(h.mode(), "PALETTE");
    assert!(h.drawn("diff-palette-row-3"), "all, uncommitted, two commits");
    h.click("diff-palette-row-2");
    h.wait(|s| !s.loading() && s.scope_label() == "abc1234");
    assert_eq!(h.read(|v| v.state().changeset().scope.clone()), shelbi_app::diff::CommitScope::Commit("abc1234def".into()));
}

#[gpui::test]
fn help_lists_the_live_bindings_and_any_key_closes_it(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.keys("?");
    assert_eq!(h.mode(), "HELP");
    assert!(h.drawn("diff-help"));
    assert!(h.drawn("diff-help-CursorDown"));
    assert!(h.drawn("diff-help-SendToAgent"));
    h.keys("j");
    assert_eq!(h.mode(), "HELP", "j scrolls");
    h.keys("q");
    assert_eq!(h.mode(), "NORMAL");
    assert!(!h.drawn("diff-help"));
}

#[gpui::test]
fn the_file_list_clicks_and_holds_the_keyboard(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.click("diff-file-3");
    assert_eq!(h.file(), Some(3));
    assert!(h.read(DiffView::list_focus));
    h.keys("j");
    assert_eq!(h.file(), Some(4), "j steps through the files");
    h.keys("k");
    assert_eq!(h.file(), Some(3));
    // Space on the list marks it viewed.
    h.keys("space");
    assert!(h.read(|v| v.state().is_viewed(&v.state().changeset().files[3])));
    h.keys("space");
    assert!(!h.read(|v| v.state().is_viewed(&v.state().changeset().files[3])));
    // Enter goes back to the rows, where Space pages.
    h.keys("enter");
    assert!(!h.read(DiffView::list_focus));
    let before = h.cursor();
    h.keys("g g");
    h.keys("space");
    assert_ne!(h.cursor(), before);
}

#[gpui::test]
fn a_check_click_marks_the_file_without_moving(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.click("diff-file-check-1");
    assert_eq!(h.file(), Some(0), "the cursor stays");
    assert!(h.read(|v| v.state().is_viewed(&v.state().changeset().files[1])));
    h.click("diff-file-check-1");
    assert!(!h.read(|v| v.state().is_viewed(&v.state().changeset().files[1])));
}

#[gpui::test]
fn the_list_key_hides_it_and_opening_it_hands_it_the_keyboard(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.keys("\\");
    assert!(!h.drawn("diff-file-list"));
    assert!(!h.read(DiffView::list_focus));
    h.keys("\\");
    assert!(h.drawn("diff-file-list"));
    assert!(h.read(DiffView::list_focus));
    h.keys("j");
    assert_eq!(h.file(), Some(1));
    h.keys("escape");
    assert!(!h.read(DiffView::list_focus));
    assert!(h.drawn("diff-file-list"), "Esc only gives back the keyboard");
}

/// One long added file: more rows than any window.
fn long_patch() -> String {
    let lines: String = (0..400).map(|i| format!("+let line_{i} = {i};\n")).collect();
    format!("diff --git a/long.rs b/long.rs\nnew file mode 100644\n--- /dev/null\n+++ b/long.rs\n@@ -0,0 +1,400 @@\n{lines}")
}

#[gpui::test]
fn a_click_places_the_cursor_and_the_wheel_scrolls(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &long_patch(), FilePlace::Local("/wt".into()));
    h.draw();
    let geometry = h.read(|v| v.geometry);
    let row_h = geometry.row_h;
    assert!(row_h > px(0.0));
    let at = gpui::point(geometry.bounds.origin.x + px(200.0), geometry.bounds.origin.y + row_h * 3.5);
    h.cx.simulate_click(at, Modifiers::none());
    h.cx.run_until_parked();
    assert_eq!(h.cursor(), 3);
    assert!(h.read(|v| v.state().table().len() > v.state().visible_rows().len()), "the rows overflow");

    h.cx.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), -(row_h * 2.0))),
        ..Default::default()
    });
    h.cx.run_until_parked();
    assert_eq!(h.read(|v| v.state().top()), 2);
    assert_eq!(h.cursor(), 3, "still in view, so it stays");
    // A sideways stroke is the long lines', not the rows'.
    h.cx.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(-40.0), px(-1.0))),
        ..Default::default()
    });
    h.cx.run_until_parked();
    assert_eq!(h.read(|v| v.state().top()), 2);
}

#[gpui::test]
fn a_chevron_click_folds_the_header(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.draw();
    let geometry = h.read(|v| v.geometry);
    let all = h.row_count();
    let at = gpui::point(geometry.bounds.origin.x + px(4.0), geometry.bounds.origin.y + geometry.row_h * 1.5);
    h.cx.simulate_click(at, Modifiers::none());
    h.cx.run_until_parked();
    assert!(h.row_count() < all, "the hunk's header folded it");
}

#[gpui::test]
fn ctrl_g_opens_the_file_at_the_line(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.keys("l");
    h.keys("] h");
    h.keys("ctrl-g");
    let opened = h.opened.lock().unwrap().clone();
    assert_eq!(opened, [("/wt/crates/shelbi-app/src/review.rs".to_string(), Some(122))]);
    assert!(h.read(|v| v.state().flash().is_some_and(|f| f.starts_with("Opened"))));
}

#[gpui::test]
fn a_file_elsewhere_says_why_it_cant_open(cx: &mut TestAppContext) {
    let mut h = open_with(cx, SAMPLE_PATCH, FilePlace::Elsewhere("It's on gpu".into()));
    h.keys("] h");
    h.keys("ctrl-g");
    assert!(h.opened.lock().unwrap().is_empty());
    assert_eq!(h.read(|v| v.state().flash().map(str::to_string)), Some("It's on gpu".to_string()));
}

#[gpui::test]
fn visual_lines_yank_to_the_clipboard(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.keys("l");
    h.keys("] h");
    h.keys("v");
    assert_eq!(h.mode(), "VISUAL");
    h.keys("j");
    h.keys("y");
    assert_eq!(h.mode(), "NORMAL");
    let clip = h.cx.read_from_clipboard().and_then(|c| c.text()).unwrap();
    assert_eq!(clip, "        self.task = Some(id);\n        let fresh = self.running.insert(id.to_string());");
}

#[gpui::test]
fn a_comment_opens_its_editor_and_lands_on_its_line(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.keys("] h");
    let rows = h.row_count();
    h.keys("enter");
    assert_eq!(h.mode(), "COMMENT");
    assert!(h.drawn("diff-draft"));
    h.keys("n i c e");
    h.keys("enter");
    assert_eq!(h.mode(), "NORMAL");
    assert!(!h.drawn("diff-draft"));
    assert_eq!(h.row_count(), rows + 1, "the thread's row");
    assert_eq!(h.read(|v| v.state().comments()[0].body.clone()), "nice");
    assert_eq!(h.read(|v| v.state().comments()[0].author.name.clone()), "jlong");
    // Its keys, on the thread's row.
    h.keys("r");
    assert_eq!(h.mode(), "COMMENT");
    h.keys("o k enter");
    h.keys("g g");
    h.keys("] c");
    assert!(h.read(|v| matches!(v.state().table().rows[v.state().cursor()], Row::Thread { .. })));
    h.keys("shift-y");
    assert!(h.cx.read_from_clipboard().and_then(|c| c.text()).unwrap().contains("nice"));
    h.keys("d");
    assert!(h.read(|v| v.state().comments().is_empty()));
}

#[gpui::test]
fn a_click_off_the_palette_closes_it_and_one_on_it_doesnt(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.keys("ctrl-p");
    h.click("diff-palette-query");
    assert_eq!(h.mode(), "PALETTE");
    h.draw();
    let overlay = h.cx.debug_bounds("diff-overlay").unwrap();
    let below = gpui::point(overlay.center().x, overlay.bottom() - px(4.0));
    h.cx.simulate_click(below, Modifiers::none());
    h.cx.run_until_parked();
    assert_eq!(h.mode(), "NORMAL");
}

#[gpui::test]
fn esc_with_nothing_held_gives_the_keyboard_back(cx: &mut TestAppContext) {
    let mut h = open(cx);
    let released = Rc::new(std::cell::Cell::new(0));
    let sink = released.clone();
    h.cx.update(|_, cx| {
        cx.subscribe(&h.view, move |_, ev: &DiffViewEvent, _| {
            if *ev == DiffViewEvent::ReleaseFocus {
                sink.set(sink.get() + 1);
            }
        })
        .detach();
    });
    h.keys("v");
    h.keys("escape");
    assert_eq!(released.get(), 0, "it let go of the selection");
    h.keys("3");
    h.keys("escape");
    assert_eq!(released.get(), 0, "it let go of the count");
    h.keys("escape");
    assert_eq!(released.get(), 1);
}

#[gpui::test]
fn the_window_keeps_what_the_diff_has_no_use_for(cx: &mut TestAppContext) {
    let h = open(cx);
    let wants = |h: &Harness, spec: &str| {
        let chord = KeyChord::parse(spec).unwrap();
        h.cx.read(|cx| h.view.read(cx).wants_key(chord, cx))
    };
    assert!(wants(&h, "ctrl-p"), "the file palette");
    assert!(wants(&h, "?"), "help");
    assert!(wants(&h, "j"));
    assert!(wants(&h, "g"), "starts a two-key binding");
    assert!(!wants(&h, "ctrl-space"), "the window's palette");
    assert!(!wants(&h, "super-k"), "the window's palette");
    assert!(!wants(&h, "super-1"), "a tab's chord");
    assert!(!wants(&h, "ctrl-1"), "the sidebar's row chord");
    assert!(!wants(&h, "ctrl-h"), "the focus move");
    assert!(!wants(&h, "esc"), "nothing held to let go of");
}

// ---- Sending and reverting -------------------------------------------------

fn open_kind(cx: &mut TestAppContext, kind: TargetKind) -> (Harness, Arc<Mutex<Vec<DiffViewEvent>>>) {
    cx.update(|cx| {
        cx.set_global(Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark));
        gpui_kit::init(cx);
    });
    let opened: Opened = Arc::default();
    let deps = DiffDeps::canned(SAMPLE_PATCH, opened.clone());
    let events: Arc<Mutex<Vec<DiffViewEvent>>> = Arc::default();
    let heard = events.clone();
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            let target = DiffTarget { kind, ..target() };
            let view = cx.new(|cx| DiffView::new(target, FilePlace::Local("/wt".into()), &deps, cx));
            cx.subscribe(&view, move |_, ev: &DiffViewEvent, _| heard.lock().unwrap().push(ev.clone())).detach();
            cx.new(|_| Host { view })
        })
        .unwrap()
    });
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    let view = window.root(&mut vcx).unwrap().read_with(&vcx, |h, _| h.view.clone());
    vcx.update(|window, cx| {
        let handle = view.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    });
    let mut h = Harness { view, cx: vcx, opened };
    h.wait(|s| !s.loading());
    h.draw();
    (h, events)
}

/// A comment on the first changed line, through the keys.
fn comment_on_a_line(h: &mut Harness) {
    h.keys("j j j");
    h.keys("enter");
    assert_eq!(h.mode(), "COMMENT");
    h.keys("w h y enter");
    h.wait(|s| s.comments().len() == 1);
}

#[gpui::test]
fn gy_in_a_workspace_delivers_and_marks_them_sent(cx: &mut TestAppContext) {
    let (mut h, events) = open_kind(cx, TargetKind::Workspace);
    comment_on_a_line(&mut h);
    h.keys("g shift-y");
    h.wait(|s| s.comments()[0].sent);
    assert_eq!(h.read(|v| v.state().flash().map(str::to_string)).as_deref(), Some("Sent 1 comment to the agent"));
    assert!(!events.lock().unwrap().iter().any(|e| matches!(e, DiffViewEvent::Reject(_))));
}

#[gpui::test]
fn gy_in_a_review_asks_its_host_for_reject(cx: &mut TestAppContext) {
    let (mut h, events) = open_kind(cx, TargetKind::Review);
    comment_on_a_line(&mut h);
    h.keys("g shift-y");
    let asked = events.lock().unwrap().iter().find_map(|e| match e {
        DiffViewEvent::Reject(reason) => Some(reason.clone()),
        _ => None,
    });
    assert!(asked.expect("a reject").contains("jlong: why"));
    assert!(!h.read(|v| v.state().comments()[0].sent), "not until the reject lands");
    // `gA` is the Reviewer's.
    h.keys("g shift-a");
    h.wait(|s| s.comments()[0].sent);
    assert_eq!(h.read(|v| v.state().flash().map(str::to_string)).as_deref(), Some("Sent 1 comment to the Reviewer"));
}

#[gpui::test]
fn revert_asks_on_the_status_line_and_a_review_has_none(cx: &mut TestAppContext) {
    let (mut h, _) = open_kind(cx, TargetKind::Workspace);
    h.keys("j j g shift-r");
    h.wait(|s| matches!(s.mode(), Mode::Confirm(_)));
    assert!(h.drawn("diff-revert-confirm"));
    h.keys("n");
    assert_eq!(h.mode(), "NORMAL");
    assert!(!h.drawn("diff-revert-confirm"));
    h.keys("g shift-r");
    h.wait(|s| matches!(s.mode(), Mode::Confirm(_)));
    h.keys("y");
    h.wait(|s| s.flash().is_some_and(|f| f.starts_with("Undid")));

    let (mut review, _) = open_kind(cx, TargetKind::Review);
    review.keys("j j g shift-r");
    assert_eq!(review.mode(), "NORMAL");
    assert!(review.read(|v| v.state().flash().unwrap_or("").contains("checkout of the branch")));
    review.keys("?");
    assert!(!review.drawn("diff-help-RevertFile"), "a review doesn't list it");
    assert!(review.drawn("diff-help-SendToReviewer"));
}

// ---- The pull request ------------------------------------------------------

fn open_pr(cx: &mut TestAppContext, pr: Result<shelbi_app::diff::pr::PrDetails, String>) -> Harness {
    cx.update(|cx| {
        cx.set_global(Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark));
        gpui_kit::init(cx);
    });
    let opened: Opened = Arc::default();
    let deps = DiffDeps::canned_with(SAMPLE_PATCH, Some(pr), opened.clone());
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            let view = cx.new(|cx| DiffView::new(target(), FilePlace::Local("/wt".into()), &deps, cx));
            cx.new(|_| Host { view })
        })
        .unwrap()
    });
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    let view = window.root(&mut vcx).unwrap().read_with(&vcx, |h, _| h.view.clone());
    vcx.update(|window, cx| {
        let handle = view.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    });
    let mut h = Harness { view, cx: vcx, opened };
    h.wait(|s| !s.loading() && (s.pr().details.is_some() || s.pr().why.is_some()));
    h.draw();
    h
}

fn pr_text(h: &Harness, part: shelbi_app::diff::PrPart) -> String {
    h.read(|v| v.state().pr_row(part).iter().map(|s| s.text.clone()).collect())
}

fn to_thread(h: &mut Harness) {
    let row = h
        .read(|v| v.state().table().rows.iter().position(|r| matches!(r, Row::Thread { .. })))
        .expect("a thread's row");
    h.cx.update(|_, cx| h.view.update(cx, |v, cx| {
        v.session.state_mut().click_row(row);
        cx.notify();
    }));
    h.draw();
}

#[gpui::test]
fn the_block_and_the_thread_are_rows_of_the_diff(cx: &mut TestAppContext) {
    use shelbi_app::diff::PrPart;
    let mut h = open_pr(cx, Ok(super::canned::sample_pr()));
    let rows = h.read(|v| v.state().table().rows.clone());
    assert_eq!(rows[0], Row::Pr { part: PrPart::Title });
    assert_eq!(h.cursor(), 0, "the block is what a new view opens on");
    assert_eq!(
        pr_text(&h, PrPart::Title),
        "▾ #2155 Record a task's PR  jlong · jlong/task-pr-metadata → main"
    );
    assert_eq!(
        pr_text(&h, PrPart::Status),
        "✓ approved by alice · ✗ changes requested by bob · checks ✓1 ✗1 ●1 (lint) · 1 thread open"
    );
    let head = rows.iter().position(|r| matches!(r, Row::Thread { part: ThreadPart::Head, .. })).unwrap();
    let Row::Line { file, hunk, line } = rows[head - 1] else { panic!("{:?}", rows[head - 1]) };
    assert!(h.read(|v| v.state().line(file, hunk, line).unwrap().text.contains("self.running.insert")));
    let Row::Thread { thread, .. } = rows[head] else { panic!() };
    assert_eq!(h.read(|v| v.state().thread_tag(thread).unwrap().0), "[unresolved]");
    // Painting them is the custom element's: it has to get through a frame.
    h.keys("shift-g");
    assert!(h.drawn("diff-body"));
}

#[gpui::test]
fn o_and_the_chevron_fold_the_block(cx: &mut TestAppContext) {
    use shelbi_app::diff::PrPart;
    let mut h = open_pr(cx, Ok(super::canned::sample_pr()));
    h.keys("o");
    assert!(h.read(|v| v.state().pr().folded));
    assert!(matches!(h.read(|v| v.state().table().rows[2]), Row::File { .. }));
    h.keys("o");
    assert_eq!(h.read(|v| v.state().table().rows[2]), Row::Pr { part: PrPart::Gap });
    let body = h.cx.debug_bounds("diff-body").expect("the body").origin;
    h.cx.simulate_click(body + gpui::point(gpui::px(4.0), gpui::px(4.0)), Modifiers::none());
    h.cx.run_until_parked();
    assert!(h.read(|v| v.state().pr().folded), "the chevron");
}

#[gpui::test]
fn i_shows_the_panel_and_esc_closes_it(cx: &mut TestAppContext) {
    let mut h = open_pr(cx, Ok(super::canned::sample_pr()));
    h.keys("i");
    assert_eq!(h.mode(), "PR INFO");
    assert!(h.drawn("diff-pr-info"));
    assert!(h.drawn("diff-pr-info-row-0"));
    h.keys("j");
    h.keys("escape");
    assert_eq!(h.mode(), "NORMAL");
    assert!(!h.drawn("diff-pr-info"));
}

#[gpui::test]
fn x_goes_to_github_and_says_what_came_back(cx: &mut TestAppContext) {
    let mut h = open_pr(cx, Ok(super::canned::sample_pr()));
    to_thread(&mut h);
    h.keys("x");
    h.wait(|s| s.flash() == Some("Done on GitHub"));
}

#[gpui::test]
fn enter_replies_and_the_editor_says_where_it_goes(cx: &mut TestAppContext) {
    let mut h = open_pr(cx, Ok(super::canned::sample_pr()));
    to_thread(&mut h);
    h.keys("enter");
    assert_eq!(h.mode(), "COMMENT");
    assert!(h.drawn("diff-draft"));
    h.keys("h i enter");
    assert_eq!(h.mode(), "NORMAL");
    h.wait(|s| s.flash() == Some("Done on GitHub"));
}

#[gpui::test]
fn r_and_c_write_through_gh_with_the_verdict_tab_picks(cx: &mut TestAppContext) {
    use shelbi_app::diff::draft::DraftTarget;
    use shelbi_app::diff::ReviewEvent;
    let mut h = open_pr(cx, Ok(super::canned::sample_pr()));
    h.keys("shift-c");
    assert!(h.read(|v| v.state().comment_on_github()));
    h.keys("shift-r");
    assert_eq!(h.mode(), "COMMENT");
    h.keys("tab");
    let picked = h.read(|v| match v.state().mode() {
        Mode::Comment(draft) => draft.target.clone(),
        other => panic!("{other:?}"),
    });
    assert_eq!(picked, DraftTarget::Review { event: ReviewEvent::Approve });
    assert!(h.drawn("diff-draft"));
    h.keys("enter");
    h.wait(|s| s.flash() == Some("Done on GitHub"));
}

#[gpui::test]
fn github_silent_the_diff_still_opens_and_the_block_says_why(cx: &mut TestAppContext) {
    use shelbi_app::diff::PrPart;
    let mut h = open_pr(cx, Err("Can't reach GitHub".to_string()));
    assert_eq!(h.read(|v| v.state().changeset().files.len()), 5);
    assert_eq!(pr_text(&h, PrPart::Status), "PR details aren't here: Can't reach GitHub");
    assert!(h.drawn("diff-body"));
    h.keys("shift-r");
    assert_eq!(h.mode(), "NORMAL");
    assert_eq!(
        h.read(|v| v.state().flash().map(str::to_string)).as_deref(),
        Some("The pull request isn't here: Can't reach GitHub")
    );
}

#[gpui::test]
fn the_help_lists_the_pull_request_keys(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.keys("?");
    assert!(h.drawn("diff-help-ToggleResolved"));
    assert!(h.drawn("diff-help-PrInfo"));
}
