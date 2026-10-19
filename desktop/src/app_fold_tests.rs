//! The sidebar's sections folding open and closed (`dt-sidebar-section-
//! animation`): a click on a header folds its rows closed (the rows below
//! gliding up) or open, a second click mid-fold turns it around from where
//! it is, Reduce Motion and a restore land at once, and the keys, chords
//! and selection follow what is collapsed, not what is still drawn.
//!
//! The window is the smoke test's headless one ([`Harness`]: the queued
//! review `T-9` is ⌘1, the working workspace `alpha-1` ⌘2) with an injected
//! clock and Reduce Motion setting.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{px, Bounds, Pixels, TestAppContext};
use shelbi_app::view::{ReviewRow, ReviewState};

use super::smoke::Harness;
use super::*;
use crate::theme::fold::FOLD;

const QUEUED: &str = "sidebar-section-Queued for Review";
const QUEUED_BODY: &str = "sidebar-body-queued";

struct Fold {
    h: Harness,
    now: Rc<Cell<Instant>>,
    reduce_motion: Rc<Cell<bool>>,
}

impl Fold {
    fn open(cx: &mut TestAppContext) -> Fold {
        let mut h = Harness::open(cx);
        let now = Rc::new(Cell::new(Instant::now()));
        let reduce_motion = Rc::new(Cell::new(false));
        let (n, r) = (now.clone(), reduce_motion.clone());
        h.window.update(&mut h.cx, |w, _| {
            w.clock = Rc::new(move || n.get());
            w.reduce_motion = Rc::new(move || r.get());
        });
        h.focus_window();
        h.take_calls();
        Fold { h, now, reduce_motion }
    }

    fn advance(&mut self, by: Duration) {
        self.now.set(self.now.get() + by);
        self.h.draw();
    }

    fn update<R>(&mut self, f: impl FnOnce(&mut ShelbiWindow, &mut Context<ShelbiWindow>) -> R) -> R {
        let window = self.h.window.clone();
        let r = self.h.cx.update(|_, cx| window.update(cx, |w, cx| f(w, cx)));
        self.h.cx.run_until_parked();
        self.h.draw();
        r
    }

    /// A click on `selector`, then the frame it draws (the clock unmoved).
    fn click(&mut self, selector: &'static str) {
        self.h.click_selector(selector);
        self.h.draw();
    }

    fn bounds(&mut self, selector: &'static str) -> Option<Bounds<Pixels>> {
        self.h.cx.debug_bounds(selector)
    }

    fn height(&mut self, selector: &'static str) -> f32 {
        self.bounds(selector).map_or(0.0, |b| b.size.height.as_f32())
    }

    fn top(&mut self, selector: &'static str) -> f32 {
        self.bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn")).top().as_f32()
    }

    fn animating(&self) -> bool {
        let now = self.now.get();
        self.h.window.read_with(&self.h.cx, |w, _| w.folds.animating(now))
    }

    fn selected(&self) -> Option<SidebarTarget> {
        self.h.window.read_with(&self.h.cx, |w, cx| w.selected(cx))
    }
}

#[gpui::test]
fn a_section_folds_closed_and_the_rows_below_glide_up(cx: &mut TestAppContext) {
    let mut f = Fold::open(cx);
    let full = f.height(QUEUED_BODY);
    assert!(full > 20.0, "the open body has its rows' height: {full}");
    let ws_open = f.top("sidebar-section-Workspaces");

    f.click(QUEUED);
    assert!(f.animating(), "the fold runs");
    assert!(f.bounds("sidebar-review-T-9").is_some(), "its rows are drawn while it closes");
    assert!((f.height(QUEUED_BODY) - full).abs() < 0.5, "it starts at its full height");

    f.advance(Duration::from_millis(50));
    let mid = f.height(QUEUED_BODY);
    assert!(mid > 0.0 && mid < full - 1.0, "part-way: {mid} of {full}");
    let ws_mid = f.top("sidebar-section-Workspaces");
    assert!(ws_mid < ws_open, "the next section glides up: {ws_mid} vs {ws_open}");

    f.advance(FOLD);
    assert!(!f.animating(), "settled");
    assert!(f.bounds("sidebar-review-T-9").is_none(), "closed: its rows are gone");
    assert!(f.bounds(QUEUED).is_some(), "the header stays");
    let ws_closed = f.top("sidebar-section-Workspaces");
    assert!((ws_open - ws_closed - full).abs() < 0.5, "it moved up by the body's height");
    assert!(ws_closed < ws_mid);
}

#[gpui::test]
fn a_section_folds_open_from_closed(cx: &mut TestAppContext) {
    let mut f = Fold::open(cx);
    let full = f.height(QUEUED_BODY);
    f.click(QUEUED);
    f.advance(FOLD);

    f.click(QUEUED);
    assert!(f.bounds("sidebar-review-T-9").is_some(), "the rows are drawn at once, clipped");
    assert!(f.height(QUEUED_BODY) < 0.5, "from nothing");
    f.advance(Duration::from_millis(50));
    let mid = f.height(QUEUED_BODY);
    assert!(mid > 0.5 && mid < full, "part-way: {mid} of {full}");
    f.advance(FOLD);
    assert!(!f.animating());
    assert!((f.height(QUEUED_BODY) - full).abs() < 0.5, "back to its height");
}

#[gpui::test]
fn a_click_mid_fold_turns_it_around_without_a_jump(cx: &mut TestAppContext) {
    let mut f = Fold::open(cx);
    let full = f.height(QUEUED_BODY);
    f.click(QUEUED);
    f.advance(Duration::from_millis(40));
    let at_turn = f.height(QUEUED_BODY);
    assert!(at_turn > 0.0 && at_turn < full);

    f.click(QUEUED);
    let after = f.height(QUEUED_BODY);
    assert!((after - at_turn).abs() < 0.5, "no jump: {at_turn} -> {after}");
    f.advance(Duration::from_millis(30));
    assert!(f.height(QUEUED_BODY) > after, "heading open again");
    f.advance(FOLD);
    assert!(!f.animating());
    assert!((f.height(QUEUED_BODY) - full).abs() < 0.5, "open");
    let saved = f
        .h
        .window
        .read_with(&f.h.cx, |w, cx| w.view_state_to_save(cx))
        .map(|(_, s)| s)
        .unwrap();
    assert!(saved.collapsed_sections.is_empty(), "remembered open");
}

#[gpui::test]
fn reduce_motion_folds_at_once(cx: &mut TestAppContext) {
    let mut f = Fold::open(cx);
    f.reduce_motion.set(true);
    f.click(QUEUED);
    assert!(!f.animating());
    assert!(f.bounds("sidebar-review-T-9").is_none(), "closed in the same frame");
    f.click(QUEUED);
    assert!(!f.animating());
    assert!(f.bounds("sidebar-review-T-9").is_some(), "open in the same frame");
}

#[gpui::test]
fn a_restore_lands_without_a_fold(cx: &mut TestAppContext) {
    let mut f = Fold::open(cx);
    // Mid-fold, too: the restore wins and nothing keeps folding.
    f.click(QUEUED);
    let mut saved = f
        .h
        .window
        .read_with(&f.h.cx, |w, cx| w.view_state_to_save(cx))
        .map(|(_, s)| s)
        .unwrap();
    saved.collapsed_sections = vec!["queued".into(), "workspaces".into()];
    f.update(|w, cx| {
        w.adopt_view_state(saved);
        cx.notify();
    });
    assert!(!f.animating(), "a restore doesn't animate");
    assert!(f.bounds("sidebar-review-T-9").is_none());
    assert!(f.bounds("sidebar-ws-alpha-1").is_none());
}

#[gpui::test]
fn the_keys_skip_a_closing_sections_rows(cx: &mut TestAppContext) {
    let mut f = Fold::open(cx);
    // Activity's row, the last nav row, just above the queued review.
    f.update(|w, cx| {
        let row = w.target_at(2).unwrap();
        w.select_row(row, cx);
        cx.notify();
    });
    f.click(QUEUED);
    assert!(f.animating());
    f.update(|w, cx| w.step_selection(true, cx));
    assert_eq!(
        f.selected(),
        Some(SidebarTarget::Workspace("alpha-1".into())),
        "Down skips the review folding away"
    );
    f.update(|w, cx| w.step_selection(false, cx));
    assert_eq!(f.selected(), Some(SidebarTarget::View(View::Activity)), "and so does Up");

    // A click on a row folding away does nothing.
    f.h.take_calls();
    f.click("sidebar-review-T-9");
    assert!(f.h.take_calls().is_empty(), "a closing row isn't clickable");

    // The chords keep their rows mid-fold.
    f.h.key("cmd-2");
    assert!(
        f.h.take_calls().contains(&HostCall::ShowView(View::Session("alpha-1".into()))),
        "⌘2 is still alpha-1"
    );
    f.h.key("cmd-1");
    assert!(
        f.h.take_calls().contains(&HostCall::LoadReview("T-9".into())),
        "⌘1 is still the review"
    );
}

#[gpui::test]
fn the_selection_rides_with_its_row_through_a_fold(cx: &mut TestAppContext) {
    let mut f = Fold::open(cx);
    f.h.key("cmd-2");
    f.h.take_calls();
    f.advance(Duration::from_secs(1));
    let before = f.top("sidebar-selection");
    let row = f.top("sidebar-ws-alpha-1");
    f.click(QUEUED);
    f.advance(Duration::from_millis(50));
    assert_eq!(f.selected(), Some(SidebarTarget::Workspace("alpha-1".into())));
    assert!(f.bounds("sliding-chip").is_none(), "the chip stays on its row, not sliding");
    let mid = f.top("sidebar-selection");
    assert!(mid < before, "it moves up with its row");
    assert!((mid - f.top("sidebar-ws-alpha-1") - (before - row)).abs() < 0.5);
    f.advance(FOLD);
    assert!((f.top("sidebar-selection") - f.top("sidebar-ws-alpha-1") - (before - row)).abs() < 0.5);
}

#[gpui::test]
fn a_row_arriving_mid_fold_doesnt_break_it(cx: &mut TestAppContext) {
    let mut f = Fold::open(cx);
    f.click(QUEUED);
    f.advance(Duration::from_millis(40));
    let before = f.height(QUEUED_BODY);
    // A board refresh brings a second queued review mid-fold.
    f.update(|w, cx| {
        w.model.reviews.push(ReviewRow {
            task_id: "T-10".into(),
            title: "Second".into(),
            branch: "jlong/T-10".into(),
            location: None,
            workspace: None,
            state: ReviewState::Pending,
            number: None,
        });
        cx.notify();
    });
    assert!(f.animating(), "still folding");
    assert!(f.bounds("sidebar-review-T-10").is_some(), "drawn inside the closing body");
    f.advance(Duration::from_millis(20));
    let later = f.height(QUEUED_BODY);
    assert!(later > 0.0, "still part-way: {later} (was {before})");
    f.advance(FOLD);
    assert!(!f.animating());
    assert!(f.bounds("sidebar-review-T-9").is_none());
    assert!(f.bounds("sidebar-review-T-10").is_none());

    // And the other way: a review leaving an opening body.
    f.click(QUEUED);
    f.advance(Duration::from_millis(40));
    f.update(|w, cx| {
        w.model.reviews.retain(|r| r.task_id != "T-10");
        cx.notify();
    });
    f.advance(FOLD);
    assert!(f.bounds("sidebar-review-T-9").is_some());
    assert!(f.bounds("sidebar-review-T-10").is_none());
    let one = f.height(QUEUED_BODY);
    assert!(one > 20.0 && px(one) < px(200.0));
}

#[gpui::test]
fn a_machine_group_folds_too(cx: &mut TestAppContext) {
    let mut f = Fold::open(cx);
    // A second machine: the workspaces group under their machines.
    f.update(|w, cx| {
        let mut ws = w.model.workspaces[0].clone();
        ws.name = "box-1".into();
        ws.machine = "box".into();
        w.model.workspaces.push(ws);
        cx.notify();
    });
    let body = "sidebar-body-machine-box";
    let full = f.height(body);
    assert!(full > 20.0, "{full}");
    f.click("sidebar-machine-box");
    assert!(f.animating());
    assert!(f.bounds("sidebar-ws-box-1").is_some(), "drawn while it closes");
    f.advance(Duration::from_millis(50));
    let mid = f.height(body);
    assert!(mid > 0.0 && mid < full, "part-way: {mid} of {full}");
    // A collapsed group's rows take no chord (they aren't in the shared
    // rows), from the fold's first frame: ⌘3 would have been box-1.
    f.h.key("cmd-3");
    assert!(f.h.take_calls().is_empty());
    f.advance(FOLD);
    assert!(!f.animating());
    assert!(f.bounds("sidebar-ws-box-1").is_none(), "closed");
    assert!(f.bounds("sidebar-machine-box").is_some(), "the group's header stays");
}

#[gpui::test]
fn the_scroll_stays_in_range_through_a_fold(cx: &mut TestAppContext) {
    let mut f = Fold::open(cx);
    // Enough workspaces to scroll the sidebar's rows.
    f.update(|w, cx| {
        let first = w.model.workspaces[0].clone();
        for n in 2..40 {
            let mut ws = first.clone();
            ws.name = format!("alpha-{n}");
            ws.current_task = None;
            w.model.workspaces.push(ws);
        }
        cx.notify();
    });
    let scroll = f.h.window.read_with(&f.h.cx, |w, _| w.sidebar_scroll.clone());
    scroll.set_offset(gpui::point(px(0.0), px(-100_000.0)));
    f.h.draw();
    let max = scroll.max_offset().y;
    assert!(max > px(0.0), "the rows scroll");
    assert_eq!(scroll.offset().y, -max, "at the bottom");

    // Fold the long section from the bottom: the scroll follows the
    // shrinking rows, never past their end, and ends at the top. (Its
    // header is scrolled out of view: toggle it as its click does.)
    f.update(|w, cx| w.toggle_section(sidebar::Section::Workspaces, cx));
    assert!(f.animating());
    let start = scroll.offset().y;
    let mut last = start;
    for _ in 0..6 {
        f.advance(Duration::from_millis(40));
        let (offset, max) = (scroll.offset().y, scroll.max_offset().y);
        assert!(offset <= px(0.0) && offset >= -max, "in range: {offset:?} of {max:?}");
        assert!(offset >= last, "it only eases toward the top: {offset:?} after {last:?}");
        last = offset;
    }
    assert!(last > start, "it moved");
    f.advance(FOLD);
    assert_eq!(scroll.offset().y, px(0.0));
    assert!(f.bounds("sidebar-section-Workspaces").is_some());
}
