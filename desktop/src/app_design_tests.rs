//! The design pass's window behavior (`dt-design-pass-taskpage`): the nav
//! selection's chip slides between rows and lands, instantly under Reduce
//! Motion. (The project switcher it also covered gave way to the tab bar's
//! `+` menu: `app_tabs_tests`.)
//!
//! The window is the smoke test's headless one ([`Harness`]) with an injected
//! clock and Reduce Motion setting, and a project catalog installed in
//! memory.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::TestAppContext;

use super::smoke::Harness;
use super::*;
use crate::projects::ProjectEntry;

struct Design {
    h: Harness,
    now: Rc<Cell<Instant>>,
    reduce_motion: Rc<Cell<bool>>,
}

impl Design {
    fn open(cx: &mut TestAppContext) -> Design {
        let mut h = Harness::open(cx);
        let entry = |name: &str| ProjectEntry {
            name: name.into(),
            label: name.into(),
        };
        h.cx.update(|_, cx| {
            cx.set_global(Catalog::with(vec![entry("alpha"), entry("beta"), entry("gamma")], Vec::new()))
        });
        let now = Rc::new(Cell::new(Instant::now()));
        let reduce_motion = Rc::new(Cell::new(false));
        let (n, r) = (now.clone(), reduce_motion.clone());
        h.window.update(&mut h.cx, |w, _| {
            w.clock = Rc::new(move || n.get());
            w.reduce_motion = Rc::new(move || r.get());
        });
        h.draw();
        Design { h, now, reduce_motion }
    }

    fn advance(&mut self, by: Duration) {
        self.now.set(self.now.get() + by);
        self.h.draw();
    }

    fn update<R>(&mut self, f: impl FnOnce(&mut ShelbiWindow, &mut Window, &mut Context<ShelbiWindow>) -> R) -> R {
        let window = self.h.window.clone();
        let r = self.h.cx.update(|win, cx| window.update(cx, |w, cx| f(w, win, cx)));
        self.h.cx.run_until_parked();
        self.h.draw();
        r
    }

    fn bounds(&mut self, selector: &'static str) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.h.cx.debug_bounds(selector)
    }
}

#[gpui::test]
fn the_nav_chip_slides_to_the_new_row(cx: &mut TestAppContext) {
    let mut d = Design::open(cx);
    d.update(|w, _, cx| {
        let row = w.target_at(0).unwrap();
        w.select_row(row, cx);
        cx.notify();
    });
    assert!(d.bounds("sliding-chip").is_none(), "at rest the row draws its own chip");
    let to = d.bounds("sidebar-nav-Issues").unwrap();
    d.update(|w, _, cx| {
        let row = w.target_at(1).unwrap();
        w.select_row(row, cx);
        cx.notify();
    });
    // The slide's first frame sits on the old row (above Issues).
    let start = d.bounds("sliding-chip").expect("the chip slides");
    assert!(start.top() < to.top() - px(30.0), "starts on the old row: {:?}", start.top());
    d.advance(Duration::from_millis(60));
    let mid = d.bounds("sliding-chip").expect("still sliding");
    assert!(
        mid.top() > start.top() && mid.top() < to.top(),
        "part-way: {:?} between {:?} and {:?}",
        mid.top(),
        start.top(),
        to.top()
    );
    d.advance(Duration::from_millis(600));
    assert!(d.bounds("sliding-chip").is_none(), "landed: the row has it again");
}

#[gpui::test]
fn reduce_motion_moves_the_chip_at_once(cx: &mut TestAppContext) {
    let mut d = Design::open(cx);
    d.reduce_motion.set(true);
    d.update(|w, _, cx| {
        let row = w.target_at(1).unwrap();
        w.select_row(row, cx);
        cx.notify();
    });
    assert!(d.bounds("sliding-chip").is_none(), "no slide under Reduce Motion");
    let selected = d.bounds("sidebar-selection").unwrap();
    let issues = d.bounds("sidebar-nav-Issues").unwrap();
    assert!(issues.contains(&selected.origin), "the Issues row has the chip");
}
