//! The design pass's window behavior (`dt-design-pass-taskpage`): the
//! project switcher's menu floats over the sidebar (nothing below it moves),
//! takes the keyboard and closes on Esc or a click outside; the nav
//! selection's chip slides between rows and lands, instantly under Reduce
//! Motion.
//!
//! The window is the smoke test's headless one ([`Harness`]) with an injected
//! clock and Reduce Motion setting, and a project catalog installed in
//! memory (the switcher never reads the disk here).

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{Modifiers, TestAppContext};

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

    /// Open the switcher as its button does (without the catalog's disk
    /// refresh).
    fn open_switcher(&mut self) {
        self.update(|w, _, cx| {
            let now = (w.clock)();
            w.switcher.open(0, now);
            cx.notify();
        });
        self.advance(Duration::from_millis(400));
    }

    fn switcher_open(&self) -> bool {
        self.h.window.read_with(&self.h.cx, |w, _| w.switcher.is_open())
    }

    fn bounds(&mut self, selector: &'static str) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.h.cx.debug_bounds(selector)
    }
}

#[gpui::test]
fn the_switcher_menu_floats_over_the_sidebar(cx: &mut TestAppContext) {
    let mut d = Design::open(cx);
    let search = d.bounds("sidebar-search").unwrap();
    let issues = d.bounds("sidebar-nav-Issues").unwrap();
    let button = d.bounds("sidebar-project-title").unwrap();
    assert_eq!(button.size.height, px(28.0), "the component's 28pt trigger");

    d.open_switcher();
    let menu = d.bounds("sidebar-switcher").expect("the menu shows");
    assert_eq!(d.bounds("sidebar-search").unwrap(), search, "the search button doesn't move");
    assert_eq!(d.bounds("sidebar-nav-Issues").unwrap(), issues, "the nav doesn't move");
    assert_eq!(menu.top(), button.bottom() + px(6.0), "6px under the button");
    assert!(menu.size.width >= button.size.width, "at least the trigger's width");
    for name in ["sidebar-project-alpha", "sidebar-project-beta", "sidebar-project-gamma", "sidebar-add-project"] {
        assert!(d.bounds(name).is_some(), "{name} is listed");
    }
}

#[gpui::test]
fn the_switcher_slides_in_and_out(cx: &mut TestAppContext) {
    let mut d = Design::open(cx);
    d.update(|w, _, cx| {
        let now = (w.clock)();
        w.switcher.open(0, now);
        cx.notify();
    });
    let button = d.bounds("sidebar-project-title").unwrap();
    let opening = d.bounds("sidebar-switcher").expect("drawn from its first frame");
    assert!(opening.top() > button.bottom() + px(6.0), "starts lower, sliding up");
    d.advance(Duration::from_millis(400));
    assert_eq!(d.bounds("sidebar-switcher").unwrap().top(), button.bottom() + px(6.0));

    d.update(|w, _, cx| {
        let now = (w.clock)();
        w.switcher.close(now);
        cx.notify();
    });
    assert!(d.bounds("sidebar-switcher").is_some(), "still fading out");
    d.advance(Duration::from_millis(400));
    assert!(d.bounds("sidebar-switcher").is_none(), "gone once faded");
}

#[gpui::test]
fn the_switcher_takes_the_keyboard(cx: &mut TestAppContext) {
    let mut d = Design::open(cx);
    d.h.focus_window();
    d.open_switcher();
    d.h.key("down");
    d.h.key("down");
    assert_eq!(d.h.window.read_with(&d.h.cx, |w, _| w.switcher.highlight), 2);
    d.h.key("enter");
    let calls = d.h.take_calls();
    assert!(
        calls.contains(&HostCall::SwitchProject("gamma".into())),
        "Enter switches to the keyboard's project: {calls:?}"
    );

    d.open_switcher();
    // Up from the top wraps to "Add project…".
    d.h.key("up");
    assert_eq!(d.h.window.read_with(&d.h.cx, |w, _| w.switcher.highlight), 3);
    d.h.key("escape");
    assert!(!d.switcher_open(), "Esc closes");
}

#[gpui::test]
fn a_click_outside_closes_the_switcher(cx: &mut TestAppContext) {
    let mut d = Design::open(cx);
    d.open_switcher();
    // The main area, clear of the menu (which floats over the nav).
    let sidebar = d.bounds("sidebar").unwrap();
    let elsewhere = gpui::point(sidebar.right() + px(80.0), sidebar.center().y);
    d.h.cx.simulate_click(elsewhere, Modifiers::none());
    d.h.cx.run_until_parked();
    assert!(!d.switcher_open());
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
