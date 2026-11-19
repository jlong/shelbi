//! A tab drag's live gap (`tab-drag-gap`): while a project tab is dragged
//! the other tabs slide aside to open an empty slot, the dragged tab's
//! width, where it will land, as Safari's and Chrome's tabs do.
//!
//! The toolkit-free part of the tab bar's drag ([`crate::views::tab_bar`]
//! draws it):
//!
//! - [`TabReorder`]: which tab is dragged, the gap the pointer is over, and
//!   the drop that moves the tab there ([`ProjectTabs::move_to`]).
//! - [`TabLayout`]: what the bar learned as it painted, each tab's slot
//!   width (the tab and the rule after it) and where the first one starts.
//!   The gap under the pointer is judged against the tabs' resting places
//!   from these ([`TabLayout::gap_at`]), never their shifted ones, so a tab
//!   sliding out from under the pointer can't hand the gap back and forth.
//! - The slide: every tab is laid out where it rests and drawn at an x
//!   offset ([`TabFrame::offset`]). During a drag each tab's target is its
//!   place in the order the drop would make, so the dragged tab's own slot
//!   (drawn empty, "lifted") travels to the landing gap and the tabs it
//!   passes step the other way. A tab eases to a new target over
//!   [`TAB_SLIDE`] from wherever it is drawn now, so reversing mid-slide
//!   turns around smoothly instead of restarting. After a drop the tabs'
//!   targets are their new resting places (where they already are), and the
//!   dropped tab settles from under the ghost into its slot; after a cancel
//!   they slide back. Under Reduce Motion every tab is placed at its target
//!   at once: the slot still opens, the tabs just reflow.
//! - Across windows (`tab-tearoff`) a bar doesn't keep its own idea of the
//!   drag: it follows the app's drag session ([`BarRole`],
//!   [`TabReorder::follow`]). The bar the tab is over shows the gap; the
//!   bar the tab came from closes its slot while the tab is elsewhere
//!   (`away`); every other bar shows none. The `+` after the tabs slides
//!   with them ([`TabFrame::tail_offset`]), so a gap at the end opens too.
//! - The tabs a drag moves are the strip ([`ProjectTabs::strip`]): the
//!   project tabs. The New tab is the `+` slot (`new-tab-plus-morph`), pinned
//!   past them: it never drags, and no gap opens over or after it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::WeakEntity;

use super::tab_bar::TabGhost;
use crate::project_tabs::ProjectTabs;

/// The `+` and the rule after it, sliding with the tabs: its key among the
/// slides (no project is named with a NUL).
const TAIL: &str = "\0tail";

/// What a tab drag asks of one window's bar (`tab-tearoff`). The app's drag
/// session decides for every bar at once, so no bar can be left showing a
/// gap the tab has moved on from.
#[derive(Debug, Clone, PartialEq)]
pub enum BarRole {
    /// No drag over this bar: a gap another window's drag opened here
    /// closes. (This bar's own drag, if any, is left alone: it ends with its
    /// drop or cancel.)
    Idle,
    /// The drag is of this bar's own tab. `home`: the tab is over this bar,
    /// over gap `gap`; away from it (over another bar, or torn off) its slot
    /// closes, the tabs after it sliding in.
    Own { home: bool, gap: Option<usize> },
    /// Another window's tab is over this bar, over gap `gap`: the bar opens
    /// a slot `slot_w` wide there (the tab pressed `grab_x` into it).
    Host { project: String, slot_w: f32, grab_x: f32, gap: Option<usize> },
}

/// How long a tab takes to slide to a new place.
pub const TAB_SLIDE: Duration = Duration::from_millis(150);

/// The tab slide's ease-out (cubic): quick off the mark, a soft landing.
pub fn ease_out(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

/// What the bar learned as it painted, shared with its paint-time probes,
/// and the start of a drag (as gpui's drag constructor saw it).
#[derive(Debug, Default)]
pub struct TabLayout {
    /// The window x where the first tab's slot starts (past the strip's
    /// leading rule).
    pub first_x: f32,
    /// Each tab's slot width, by project: the tab and its trailing rule.
    widths: HashMap<String, f32>,
    /// How far into the dragged tab the pointer pressed: the ghost is drawn
    /// that far left of the pointer.
    pub grab_x: f32,
    /// How far down the dragged tab the pointer pressed.
    pub grab_y: f32,
    /// Where the dragged tab sat in the window as its drag began.
    pub tab_origin: (f32, f32),
    /// The dragged tab's ghost: a drag lasts while it lives (gpui keeps it
    /// for the drag only), so another kind of drag isn't taken for a tab's.
    pub ghost: Option<WeakEntity<TabGhost>>,
    /// The bar's bounds in the window as last painted (`None` while the
    /// window draws no tab bar): where another window's dragged tab looks
    /// for it (`tab-tearoff`).
    pub bar: Option<gpui::Bounds<f32>>,
    /// The `+` slot's width as last painted, its rule included (`None`
    /// before it has painted): wider while it is the New tab.
    pub tail_w: Option<f32>,
    /// The New tab's label (and ⌘N badge) at its natural width, as last
    /// painted: how far the `+` slot widens for it.
    pub plus_label_w: Option<f32>,
}

/// The layout as the bar and its probes share it.
pub type SharedLayout = Rc<RefCell<TabLayout>>;

impl TabLayout {
    /// The dragged tab's ghost is still up.
    pub fn ghost_alive(&self) -> bool {
        self.ghost.as_ref().is_some_and(|g| g.upgrade().is_some())
    }

    /// `project`'s slot measured `width` wide as it painted.
    pub fn set_width(&mut self, project: &str, width: f32) {
        match self.widths.get_mut(project) {
            Some(w) => *w = width,
            None => {
                self.widths.insert(project.to_string(), width);
            }
        }
    }

    /// The `+` slot's width as last painted, else `plus_w` (a lone `+`'s).
    pub fn tail_w(&self, plus_w: f32) -> f32 {
        self.tail_w.unwrap_or(plus_w)
    }

    /// `project`'s slot width as last painted (0 before it has painted).
    pub fn width(&self, project: &str) -> f32 {
        self.widths.get(project).copied().unwrap_or(0.0)
    }

    /// Each of `order`'s slots' left edge, from the first slot's (0).
    fn lefts<'a>(&self, order: impl IntoIterator<Item = &'a str>) -> Vec<(&'a str, f32)> {
        let mut x = 0.0;
        order
            .into_iter()
            .map(|p| {
                let left = x;
                x += self.width(p);
                (p, left)
            })
            .collect()
    }

    /// The drop gap under window x `x` (0 before the first tab, the tab count
    /// after the last), judged against `order`'s resting places: a tab's left
    /// half is the gap before it, its right half the gap after, the bar
    /// before the first tab is the start, and past the `+` (`plus_w` wide,
    /// right after the last tab) the end. Over the `+`, no gap.
    pub fn gap_at(&self, order: &[String], x: f32, plus_w: f32) -> Option<usize> {
        let x = x - self.first_x;
        if x < 0.0 {
            return Some(0);
        }
        let mut left = 0.0;
        for (ix, project) in order.iter().enumerate() {
            let w = self.width(project);
            if x < left + w {
                return Some(ix + usize::from(x >= left + w / 2.0));
            }
            left += w;
        }
        (x >= left + plus_w).then_some(order.len())
    }
}

/// One tab's slide toward `to` (strip-local x, from the first slot's left),
/// from `from`, begun at `start`.
#[derive(Debug, Clone, Copy)]
struct Slide {
    from: f32,
    to: f32,
    start: Instant,
}

impl Slide {
    fn at(&self, now: Instant) -> f32 {
        let t = now.saturating_duration_since(self.start).as_secs_f32() / TAB_SLIDE.as_secs_f32();
        self.from + (self.to - self.from) * ease_out(t)
    }

    fn done(&self, now: Instant) -> bool {
        (self.to - self.from).abs() < 0.5 || now.saturating_duration_since(self.start) >= TAB_SLIDE
    }
}

/// How the tabs draw this frame.
#[derive(Debug, Default, PartialEq)]
pub struct TabFrame {
    offsets: HashMap<String, f32>,
    /// The tab being dragged: its slot is drawn empty.
    pub lifted: Option<String>,
    /// The dragged tab is away from this bar (over another, or torn off):
    /// its slot isn't drawn at all, rule and all.
    pub away: bool,
    /// A tab is still sliding: draw another frame.
    pub animating: bool,
}

impl TabFrame {
    /// How far right of its resting place `project`'s tab draws.
    pub fn offset(&self, project: &str) -> f32 {
        self.offsets.get(project).copied().unwrap_or(0.0)
    }

    /// How far right of its resting place the `+` (and the rule after it)
    /// draws: left by the slot of a tab gone from the bar, right by the gap
    /// another window's tab opens.
    pub fn tail_offset(&self) -> f32 {
        self.offset(TAIL)
    }
}

/// A tab drag's state, owned by the window: the dragged tab, the drop gap the
/// pointer is over, and the tabs' slides. A drag that ends off the bar never
/// hears its drop, so the window reads it only while a drag is active
/// ([`Self::frame`]'s `active`), and the next drag's start resets it.
#[derive(Debug, Default)]
pub struct TabReorder {
    project: Option<String>,
    slot: Option<usize>,
    /// The dropped tab and the ghost's strip-local x at the drop, until the
    /// next frame starts it settling from there.
    settle: Option<(String, f32)>,
    slides: HashMap<String, Slide>,
    /// What the bar measured as it painted (shared with its probes).
    pub layout: SharedLayout,
    /// Another window's tab drag is over this bar (`tab-tearoff`): the bar
    /// opens its gap though the drag isn't this window's.
    hosting: bool,
    /// That tab, when this bar has none for its project: the project and its
    /// slot's width (the gap's).
    incoming: Option<(String, f32)>,
    /// This bar's dragged tab is over another bar, or torn off: its slot
    /// closes.
    away: bool,
}

impl TabReorder {
    /// A drag of `project`'s tab began, over no gap yet.
    pub fn start(&mut self, project: &str) {
        self.project = Some(project.to_string());
        self.slot = None;
        self.settle = None;
        self.hosting = false;
        self.incoming = None;
        self.away = false;
    }

    /// Another window's drag of `project`'s tab (its slot `slot_w` wide,
    /// pressed `grab_x` into it) came over this bar, over no gap yet. When
    /// this bar has a tab for the project too, that tab is the one lifted.
    pub fn host(&mut self, tabs: &ProjectTabs, project: &str, slot_w: f32, grab_x: f32) {
        self.start(project);
        self.hosting = true;
        if !tabs.contains(project) {
            self.project = None;
            self.incoming = Some((project.to_string(), slot_w));
        }
        self.layout.borrow_mut().grab_x = grab_x;
    }

    /// The other window's drag left this bar (or ended): its gap closes.
    pub fn unhost(&mut self) {
        if self.hosting {
            self.cancel();
        }
    }

    /// Another window's drag is over this bar.
    pub fn hosting(&self) -> bool {
        self.hosting
    }

    /// The drag ends with no drop here (the tab went to another window, or
    /// the drag was cancelled): the tabs slide back.
    pub fn cancel(&mut self) {
        self.project = None;
        self.slot = None;
        self.hosting = false;
        self.incoming = None;
        self.away = false;
    }

    /// The project another window's drag is over this bar with.
    fn hosted(&self) -> Option<&str> {
        if !self.hosting {
            return None;
        }
        self.incoming.as_ref().map(|(p, _)| p.as_str()).or(self.project.as_deref())
    }

    /// What this bar shows, to tell whether following the session changed it.
    fn shown(&self) -> (Option<String>, Option<usize>, Option<String>, bool) {
        (self.project.clone(), self.slot, self.hosted().map(str::to_string), self.away)
    }

    /// Show what the drag session asks of this bar (`role`). Whether that
    /// changed what it shows.
    pub fn follow(&mut self, tabs: &ProjectTabs, role: BarRole) -> bool {
        let before = self.shown();
        match role {
            BarRole::Idle => self.unhost(),
            BarRole::Own { home, gap } => {
                if self.hosting {
                    // Never both: hosting is another window's drag.
                    self.cancel();
                }
                if self.project.is_some() {
                    self.away = !home;
                    self.over(if home { gap } else { None });
                }
            }
            BarRole::Host { project, slot_w, grab_x, gap } => {
                if self.hosted() != Some(project.as_str()) {
                    self.host(tabs, &project, slot_w, grab_x);
                }
                self.over(gap);
            }
        }
        self.shown() != before
    }

    /// The bar shows a drop gap: another window's tab is over it, or its own
    /// dragged tab is (its slot open, wherever it is).
    #[cfg(test)]
    pub fn gap_open(&self) -> bool {
        self.hosting || (self.project.is_some() && !self.away)
    }

    /// Another window's tab dropped here, with the pointer at window x
    /// `pointer_x`: it takes the gap it is over (the end when it is over
    /// none), settling from where its ghost was. Whether the tabs changed
    /// (a lifted tab of this bar's own lands as [`Self::drop`] does).
    pub fn accept(&mut self, tabs: &mut ProjectTabs, pointer_x: f32) -> bool {
        self.hosting = false;
        let Some((project, width)) = self.incoming.take() else {
            return self.drop(tabs, pointer_x);
        };
        let slot = self.slot.take().unwrap_or(tabs.strip().len());
        {
            let mut layout = self.layout.borrow_mut();
            // Its slot is the gap's width until it paints.
            layout.set_width(&project, width);
            self.settle = Some((project.clone(), pointer_x - layout.grab_x - layout.first_x));
        }
        tabs.insert_at(&project, slot)
    }

    /// The pointer is over drop gap `slot` (`None`: no gap). Whether that
    /// changed.
    pub fn over(&mut self, slot: Option<usize>) -> bool {
        let changed = (self.project.is_some() || self.incoming.is_some()) && self.slot != slot;
        if changed {
            self.slot = slot;
        }
        changed
    }

    /// The dragged tab's project.
    #[cfg(test)]
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    /// The dragged tab's ghost is still up: the drag gpui has is this one.
    pub fn ghost_alive(&self) -> bool {
        self.layout.borrow().ghost_alive()
    }

    /// Where the dragged tab lands if dropped now (a [`ProjectTabs::move_to`]
    /// index): the gap the pointer is over, when dropping there would move
    /// the tab.
    pub fn landing(&self, tabs: &ProjectTabs) -> Option<usize> {
        tabs.landing(self.project.as_deref()?, self.slot?)
    }

    /// The drop, with the pointer at window x `pointer_x`: move the dragged
    /// tab into the gap it is over, end the drag, and have the tab settle
    /// from where its ghost was. Whether the tabs changed.
    pub fn drop(&mut self, tabs: &mut ProjectTabs, pointer_x: f32) -> bool {
        let (project, slot) = (self.project.take(), self.slot.take());
        let Some(project) = project else {
            return false;
        };
        {
            let layout = self.layout.borrow();
            self.settle = Some((project.clone(), pointer_x - layout.grab_x - layout.first_x));
        }
        slot.and_then(|slot| tabs.landing(&project, slot))
            .is_some_and(|to| tabs.move_to(&project, to))
    }

    /// This frame's tab offsets at `now`, sliding each tab toward its target:
    /// during a drag (`active`) its place in the order the drop would make,
    /// otherwise its resting place. Under `reduce_motion` every tab is placed
    /// at its target.
    pub fn frame(&mut self, tabs: &ProjectTabs, active: bool, now: Instant, reduce_motion: bool) -> TabFrame {
        let dragged = self
            .project
            .clone()
            .filter(|p| active && tabs.contains(p));
        let settle = self.settle.take();
        let incoming = self.incoming.as_ref().filter(|_| active).map(|(_, w)| *w);
        if dragged.is_none() && incoming.is_none() && settle.is_none() && self.slides.is_empty() {
            return TabFrame::default();
        }
        let order: Vec<&str> = tabs.strip().iter().map(String::as_str).collect();
        let layout = self.layout.borrow();
        let rest: HashMap<&str, f32> = layout.lefts(order.iter().copied()).into_iter().collect();
        let lifted = order.iter().copied().find(|p| dragged.as_deref() == Some(*p));
        let away = lifted.is_some() && self.away;
        let target_order: Vec<&str> = match (lifted, self.landing(tabs).filter(|_| lifted.is_some())) {
            // Gone from the bar: the others close up, its slot (undrawn)
            // past them.
            (Some(project), _) if away => {
                let mut moved: Vec<&str> = order.iter().copied().filter(|p| *p != project).collect();
                moved.push(project);
                moved
            }
            (Some(project), Some(to)) => {
                let mut moved: Vec<&str> = order.iter().copied().filter(|p| *p != project).collect();
                moved.insert(to.min(moved.len()), project);
                moved
            }
            _ => order.clone(),
        };
        let targets: HashMap<&str, f32> = match (incoming, self.slot) {
            // Another window's tab: a gap its width opens before the tab at
            // `slot`.
            (Some(width), Some(slot)) => {
                let mut x = 0.0;
                order
                    .iter()
                    .enumerate()
                    .map(|(ix, p)| {
                        if ix == slot {
                            x += width;
                        }
                        let left = x;
                        x += layout.width(p);
                        (*p, left)
                    })
                    .collect()
            }
            _ => layout.lefts(target_order).into_iter().collect(),
        };
        // The `+` rests past every slot; it follows the tabs' end.
        let total: f32 = order.iter().map(|p| layout.width(p)).sum();
        let tail = match (lifted, incoming, self.slot) {
            (Some(project), _, _) if away => total - layout.width(project),
            (_, Some(width), Some(_)) => total + width,
            _ => total,
        };
        drop(layout);

        self.slides.retain(|p, _| p == TAIL || rest.contains_key(p.as_str()));
        let mut frame = TabFrame {
            lifted: dragged,
            away,
            ..TabFrame::default()
        };
        let slide = retarget(&mut self.slides, TAIL, total, tail, now, reduce_motion);
        frame.animating |= !slide.done(now);
        let x = if slide.done(now) { slide.to } else { slide.at(now) };
        frame.offsets.insert(TAIL.to_string(), x - total);
        for project in &order {
            let target = targets[project];
            let slide = match settle.as_ref().filter(|(p, _)| p == project) {
                Some(&(_, ghost)) if !reduce_motion => {
                    let slide = Slide { from: ghost, to: target, start: now };
                    self.slides.insert(project.to_string(), slide);
                    slide
                }
                _ => retarget(&mut self.slides, project, rest[project], target, now, reduce_motion),
            };
            frame.animating |= !slide.done(now);
            let x = if slide.done(now) { slide.to } else { slide.at(now) };
            frame.offsets.insert(project.to_string(), x - rest[project]);
        }
        if frame.lifted.is_none() && incoming.is_none() && !frame.animating {
            self.slides.clear();
        }
        frame
    }
}

/// Point `project`'s slide at `target`: unchanged if it is headed there,
/// otherwise from where it is drawn now, or from where it rests (`rest`)
/// when it had no slide yet (placed there at once under `reduce_motion`).
fn retarget(
    slides: &mut HashMap<String, Slide>,
    project: &str,
    rest: f32,
    target: f32,
    now: Instant,
    reduce_motion: bool,
) -> Slide {
    let slide = match slides.get(project) {
        Some(s) if (s.to - target).abs() < 0.01 => *s,
        _ if reduce_motion => Slide { from: target, to: target, start: now },
        Some(s) => Slide {
            from: if s.done(now) { s.to } else { s.at(now) },
            to: target,
            start: now,
        },
        None => Slide { from: rest, to: target, start: now },
    };
    slides.insert(project.to_string(), slide);
    slide
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLUS: f32 = 53.0;

    fn tabs(names: &[&str]) -> ProjectTabs {
        ProjectTabs::new(names.iter().map(|n| n.to_string()))
    }

    /// web 100, api 60, docs 140, starting at x 100.
    fn reorder() -> (TabReorder, ProjectTabs) {
        let r = TabReorder::default();
        {
            let mut l = r.layout.borrow_mut();
            l.first_x = 100.0;
            l.set_width("web", 100.0);
            l.set_width("api", 60.0);
            l.set_width("docs", 140.0);
        }
        (r, tabs(&["web", "api", "docs"]))
    }

    fn offsets(f: &TabFrame) -> [f32; 3] {
        ["web", "api", "docs"].map(|p| f.offset(p))
    }

    #[test]
    fn the_gap_follows_the_resting_halves() {
        let (r, t) = reorder();
        let l = r.layout.borrow();
        let order = t.order();
        // Slots: web 100..200, api 200..260, docs 260..400, + 400..453.
        assert_eq!(l.gap_at(order, 40.0, PLUS), Some(0), "before the first tab");
        assert_eq!(l.gap_at(order, 149.0, PLUS), Some(0), "web's left half");
        assert_eq!(l.gap_at(order, 150.0, PLUS), Some(1), "web's right half");
        assert_eq!(l.gap_at(order, 229.0, PLUS), Some(1), "api's left half");
        assert_eq!(l.gap_at(order, 231.0, PLUS), Some(2));
        assert_eq!(l.gap_at(order, 399.0, PLUS), Some(3), "docs's right half");
        assert_eq!(l.gap_at(order, 420.0, PLUS), None, "over the +");
        assert_eq!(l.gap_at(order, 460.0, PLUS), Some(3), "past the +");
    }

    #[test]
    fn the_gap_holds_still_while_the_tabs_slide_under_the_pointer() {
        // Dragging web right over api's right half: api slides left under
        // the pointer, but the gap is judged where api rests, so it stays.
        let (mut r, t) = reorder();
        r.start("web");
        let now = Instant::now();
        let x = 240.0;
        let gap = r.layout.borrow().gap_at(t.order(), x, PLUS);
        assert_eq!(gap, Some(2));
        r.over(gap);
        let f = r.frame(&t, true, now, true);
        assert_eq!(offsets(&f), [60.0, -100.0, 0.0], "api in web's place, the slot after it");
        for _ in 0..3 {
            let again = r.layout.borrow().gap_at(t.order(), x, PLUS);
            assert!(!r.over(again), "no flicker");
            assert_eq!(r.frame(&t, true, now, true), f);
        }
    }

    #[test]
    fn the_slot_opens_at_the_landing_gap_in_the_dragged_tabs_width() {
        let (mut r, t) = reorder();
        let now = Instant::now();
        r.start("docs");
        let f = r.frame(&t, true, now, true);
        assert_eq!(f.lifted.as_deref(), Some("docs"));
        assert_eq!(offsets(&f), [0.0; 3], "over no gap: its own slot stays open");
        // Over web's left half: web and api step right by docs's width, the
        // empty slot (docs's) at the start.
        r.over(Some(0));
        let f = r.frame(&t, true, now, true);
        assert_eq!(offsets(&f), [140.0, 140.0, -160.0]);
        // Its own gaps leave everything at rest.
        r.over(Some(2));
        assert_eq!(offsets(&r.frame(&t, true, now, true)), [0.0; 3]);
    }

    #[test]
    fn a_tab_slides_from_where_it_is_drawn_when_the_gap_reverses() {
        let (mut r, t) = reorder();
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        r.start("docs");
        r.frame(&t, true, t0, false);
        r.over(Some(1)); // api steps right 140.
        let f = r.frame(&t, true, t0, false);
        assert!(f.animating);
        assert_eq!(f.offset("api"), 0.0, "starts where it rests");
        let mid = r.frame(&t, true, ms(50), false).offset("api");
        assert!(mid > 0.0 && mid < 140.0, "{mid}");
        // Back over docs's own gap halfway: api turns around from `mid`.
        r.over(Some(3));
        let turned = r.frame(&t, true, ms(50), false);
        assert_eq!(turned.offset("api"), mid, "no snap back to its old place");
        let later = r.frame(&t, true, ms(100), false).offset("api");
        assert!(later < mid && later > 0.0, "{later}");
        let done = r.frame(&t, true, ms(50 + 150), false);
        assert_eq!(done.offset("api"), 0.0);
        assert!(!done.animating);
    }

    #[test]
    fn a_drop_lands_in_the_slot_and_the_tab_settles_from_its_ghost() {
        let (mut r, mut t) = reorder();
        let t0 = Instant::now();
        r.start("docs");
        r.over(Some(0));
        r.frame(&t, true, t0, true);
        // The ghost's left edge at x 130 (pointer 150, pressed 20 in).
        r.layout.borrow_mut().grab_x = 20.0;
        assert!(r.drop(&mut t, 150.0));
        assert_eq!(t.order(), ["docs", "web", "api"]);
        assert_eq!(r.project(), None, "the drop ends it");
        let f = r.frame(&t, false, t0, false);
        assert_eq!(f.lifted, None);
        assert_eq!(f.offset("web"), 0.0, "already in its new place: no jump");
        assert_eq!(f.offset("api"), 0.0);
        assert_eq!(f.offset("docs"), 30.0, "under the ghost");
        assert!(f.animating);
        let f = r.frame(&t, false, t0 + TAB_SLIDE, false);
        assert_eq!(f.offset("docs"), 0.0);
        assert!(!f.animating);
        assert_eq!(r.frame(&t, false, t0 + TAB_SLIDE, false), TabFrame::default(), "settled");
    }

    #[test]
    fn a_cancelled_drag_slides_the_tabs_back() {
        let (mut r, t) = reorder();
        let t0 = Instant::now();
        r.start("docs");
        r.over(Some(0));
        r.frame(&t, true, t0, false);
        let f = r.frame(&t, true, t0 + TAB_SLIDE, false);
        assert_eq!(offsets(&f), [140.0, 140.0, -160.0]);
        // Released off the bar: no drop, the drag just ends.
        let f = r.frame(&t, false, t0 + TAB_SLIDE, false);
        assert_eq!(f.lifted, None, "the tab shows again");
        assert_eq!(offsets(&f), [140.0, 140.0, -160.0], "from where they are");
        assert!(f.animating);
        let f = r.frame(&t, false, t0 + TAB_SLIDE * 2, false);
        assert_eq!(offsets(&f), [0.0; 3]);
        assert_eq!(t.order(), ["web", "api", "docs"]);
        // A drop over no gap (the `+`) moves nothing.
        let mut t = t;
        r.start("web");
        r.over(Some(3));
        r.over(None);
        assert!(!r.drop(&mut t, 420.0));
        assert_eq!(t.order(), ["web", "api", "docs"]);
    }

    #[test]
    fn reduce_motion_reflows_without_sliding() {
        let (mut r, t) = reorder();
        let t0 = Instant::now();
        r.start("web");
        r.frame(&t, true, t0, true);
        r.over(Some(3));
        let f = r.frame(&t, true, t0, true);
        assert_eq!(offsets(&f), [200.0, -100.0, -100.0]);
        assert!(!f.animating);
        let f = r.frame(&t, false, t0, true);
        assert_eq!(offsets(&f), [0.0; 3]);
        assert!(!f.animating);
    }

    #[test]
    fn a_new_drag_forgets_the_last_ones_gap() {
        let (mut r, mut t) = reorder();
        assert!(!r.over(Some(0)), "no drag yet");
        r.start("web");
        r.over(Some(3));
        r.start("web");
        assert_eq!(r.landing(&t), None);
        assert!(!r.drop(&mut t, 0.0));
        assert_eq!(t.order(), ["web", "api", "docs"]);
    }

    fn host(project: &str, gap: Option<usize>) -> BarRole {
        BarRole::Host { project: project.into(), slot_w: 80.0, grab_x: 10.0, gap }
    }

    #[test]
    fn away_from_its_bar_the_dragged_tabs_slot_closes_and_the_plus_follows() {
        let (mut r, t) = reorder();
        let t0 = Instant::now();
        r.start("web");
        assert!(r.follow(&t, BarRole::Own { home: true, gap: Some(3) }));
        let f = r.frame(&t, true, t0, true);
        assert!(r.gap_open());
        assert!(!f.away);
        assert_eq!(offsets(&f), [200.0, -100.0, -100.0], "its slot at the end");
        assert_eq!(f.tail_offset(), 0.0);
        // Over another bar: api and docs close up over web's slot, the `+`
        // with them; web's slot (undrawn) goes past them.
        assert!(r.follow(&t, BarRole::Own { home: false, gap: None }));
        assert!(!r.gap_open());
        let f = r.frame(&t, true, t0, true);
        assert!(f.away);
        assert_eq!(f.lifted.as_deref(), Some("web"));
        assert_eq!(offsets(&f), [200.0, -100.0, -100.0]);
        assert_eq!(f.tail_offset(), -100.0, "the + closes web's slot");
        assert!(!r.follow(&t, BarRole::Own { home: false, gap: None }), "no change");
        // Home again, over the start: the gap opens there.
        assert!(r.follow(&t, BarRole::Own { home: true, gap: Some(0) }));
        assert!(r.gap_open());
        let f = r.frame(&t, true, t0, true);
        assert!(!f.away);
        assert_eq!(offsets(&f), [0.0; 3]);
        assert_eq!(f.tail_offset(), 0.0);
    }

    #[test]
    fn a_hosted_gap_opens_closes_and_opens_at_the_end_too() {
        let (mut r, t) = reorder();
        let t0 = Instant::now();
        assert!(!r.follow(&t, BarRole::Idle), "nothing to close");
        assert!(r.follow(&t, host("ops", Some(1))));
        assert!(r.gap_open());
        let f = r.frame(&t, true, t0, true);
        assert_eq!(offsets(&f), [0.0, 80.0, 80.0], "api and docs step right");
        assert_eq!(f.tail_offset(), 80.0, "so does the +");
        // Past the last tab: the gap at the end, the + stepping aside.
        assert!(r.follow(&t, host("ops", Some(3))));
        let f = r.frame(&t, true, t0, true);
        assert_eq!(offsets(&f), [0.0; 3]);
        assert_eq!(f.tail_offset(), 80.0, "the gap at the end shows");
        // The tab moved on: the gap closes and everything slides home.
        assert!(r.follow(&t, BarRole::Idle));
        assert!(!r.gap_open());
        let f = r.frame(&t, false, t0, true);
        assert_eq!(offsets(&f), [0.0; 3]);
        assert_eq!(f.tail_offset(), 0.0);
        // And back, as often as it goes back and forth.
        for _ in 0..3 {
            assert!(r.follow(&t, host("ops", Some(0))));
            assert_eq!(offsets(&r.frame(&t, true, t0, true)), [80.0; 3]);
            assert!(r.follow(&t, BarRole::Idle));
            assert_eq!(offsets(&r.frame(&t, false, t0, true)), [0.0; 3]);
        }
    }

    #[test]
    fn a_hosted_drop_lands_in_the_gap_it_showed() {
        let (mut r, mut t) = reorder();
        r.follow(&t, host("ops", Some(2)));
        assert!(r.accept(&mut t, 0.0));
        assert_eq!(t.order(), ["web", "api", "ops", "docs"]);
        assert!(!r.hosting());
        // Over the `+` the session asks for the end: it lands there.
        let (mut r, mut t) = reorder();
        r.follow(&t, host("ops", Some(3)));
        assert!(r.accept(&mut t, 0.0));
        assert_eq!(t.order(), ["web", "api", "docs", "ops"]);
    }

    #[test]
    fn a_hosted_gap_slides_open_from_where_the_tabs_rest() {
        let (mut r, t) = reorder();
        let t0 = Instant::now();
        r.follow(&t, host("ops", Some(0)));
        let f = r.frame(&t, true, t0, false);
        assert_eq!(offsets(&f), [0.0; 3], "no jump on entry");
        assert!(f.animating);
        let f = r.frame(&t, true, t0 + TAB_SLIDE, false);
        assert_eq!(offsets(&f), [80.0; 3]);
        assert_eq!(f.tail_offset(), 80.0);
    }

    #[test]
    fn the_ease_lands_softly() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.5) > 0.8, "most of the way by halfway");
    }
}
