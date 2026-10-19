//! The Activity feed's virtualized list (`dt-activity-perf`): the feed's day
//! headers and rows, and the rows under them, are one gpui `list`, which
//! builds only the items in or near view. A feed of thousands of events
//! (after "Load older" a few times, or on a busy day) then costs a frame what
//! its visible screenful does.
//!
//! [`FeedList::sync`] tells the list what changed when the feed does, by the
//! items' keys ([`ItemKey`]): the span where items came or went (new events
//! at the top, older ones at the bottom) is spliced, and an item that only
//! reads differently (its relative time a minute on, its task's tag) is
//! re-measured in place. Every other item keeps its measured height, and the
//! list keeps its scroll position on the item it was on, so a live refresh
//! never moves what is on screen.

use gpui::{px, ListAlignment, ListState};

use super::activity_state::ItemKey;
use super::board_list::changed;

/// How far past the view the feed measures items (they are built only when
/// in view).
const OVERDRAW: f32 = 400.0;

pub(super) struct FeedList {
    pub state: ListState,
    keys: Vec<ItemKey>,
}

impl FeedList {
    pub fn new() -> Self {
        FeedList {
            state: ListState::new(0, ListAlignment::Top, px(OVERDRAW)),
            keys: Vec::new(),
        }
    }

    /// Bring the list in line with `keys` (one per item, in order). A no-op
    /// when nothing changed.
    pub fn sync(&mut self, keys: &[ItemKey]) {
        if self.keys == keys {
            return;
        }
        let old: Vec<u64> = self.keys.iter().map(|k| k.ident).collect();
        let new: Vec<u64> = keys.iter().map(|k| k.ident).collect();
        let (range, count) = changed(&old, &new);
        // The items either side of the span are the same items: those that
        // read differently are re-measured where they now are.
        let after = |j: usize| j + count - range.len();
        let stale: Vec<usize> = (0..range.start)
            .filter(|&i| self.keys[i].content != keys[i].content)
            .chain(
                (range.end..self.keys.len())
                    .filter(|&j| self.keys[j].content != keys[after(j)].content)
                    .map(after),
            )
            .collect();
        self.state.splice(range, count);
        for run in runs(&stale) {
            self.state.remeasure_items(run);
        }
        self.keys = keys.to_vec();
    }
}

/// Sorted indices as contiguous ranges.
fn runs(ixs: &[usize]) -> Vec<std::ops::Range<usize>> {
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    for &ix in ixs {
        match out.last_mut() {
            Some(r) if r.end == ix => r.end += 1,
            _ => out.push(ix..ix + 1),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{
        div, point, prelude::*, px, size, Bounds, Context, ListOffset, TestAppContext, VisualTestContext,
        Window,
    };

    fn key(ident: u64, content: u64) -> ItemKey {
        ItemKey { ident, content }
    }

    fn keys(range: std::ops::Range<u64>) -> Vec<ItemKey> {
        range.map(|i| key(i, i)).collect()
    }

    #[test]
    fn runs_coalesce() {
        assert_eq!(runs(&[1, 2, 3, 7, 9, 10]), vec![1..4, 7..8, 9..11]);
        assert!(runs(&[]).is_empty());
    }

    #[test]
    fn new_events_at_the_top_keep_the_scroll_on_its_row() {
        let mut list = FeedList::new();
        list.sync(&keys(0..100));
        assert_eq!(list.state.item_count(), 100);
        list.state.scroll_to(ListOffset {
            item_ix: 40,
            offset_in_item: px(12.0),
        });
        // Two new rows under the first item (the "Today" header).
        let mut grown = vec![key(0, 0), key(1000, 1000), key(1001, 1001)];
        grown.extend(keys(1..100));
        list.sync(&grown);
        assert_eq!(list.state.item_count(), 102);
        let top = list.state.logical_scroll_top();
        assert_eq!((top.item_ix, top.offset_in_item), (42, px(12.0)), "still on the same row");
        // The same feed again changes nothing.
        list.sync(&grown);
        assert_eq!(list.state.logical_scroll_top().item_ix, 42);
    }

    #[test]
    fn a_row_that_only_reads_differently_stays_put() {
        let mut list = FeedList::new();
        list.sync(&keys(0..50));
        list.state.scroll_to(ListOffset {
            item_ix: 20,
            offset_in_item: px(5.0),
        });
        // The top row's time moved on, and the row scrolled to: the same
        // items, re-measured in place.
        let mut aged = keys(0..50);
        aged[1].content = 77;
        aged[20].content = 78;
        list.sync(&aged);
        assert_eq!(list.state.item_count(), 50);
        let top = list.state.logical_scroll_top();
        assert_eq!((top.item_ix, top.offset_in_item), (20, px(5.0)));
    }

    /// The feed the way the view draws it, counting the items it builds.
    struct Feed {
        list: FeedList,
        keys: Vec<ItemKey>,
        built: Rc<Cell<usize>>,
    }

    impl Render for Feed {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.list.sync(&self.keys);
            let built = self.built.clone();
            div().flex().flex_col().size_full().child(
                gpui::list(self.list.state.clone(), move |ix, _, _| {
                    built.set(built.get() + 1);
                    // Day headers and rows differ in height.
                    let h = if ix % 25 == 0 { 40.0 } else { 30.0 };
                    div().h(px(h)).into_any_element()
                })
                .with_sizing_behavior(gpui::ListSizingBehavior::Auto)
                .flex_1(),
            )
        }
    }

    #[gpui::test]
    fn a_long_feed_builds_only_the_rows_near_view(cx: &mut TestAppContext) {
        let built = Rc::new(Cell::new(0));
        let b = built.clone();
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(900.0), px(800.0)));
        let window = cx.update(|cx| {
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|_| Feed {
                        list: FeedList::new(),
                        keys: keys(0..6000),
                        built: b,
                    })
                },
            )
            .unwrap()
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let draw = |cx: &mut VisualTestContext| {
            built.set(0);
            cx.update(|window, cx| window.draw(cx).clear(cx));
            built.get()
        };
        let first = draw(&mut cx);
        assert!(first < 60, "the first frame built {first} of 6000 items");
        let again = draw(&mut cx);
        assert!(again <= 30, "a redraw builds the visible items only: {again}");
        // A refresh with a new row at the top, scrolled down the feed: the
        // view stays on its row and builds only what's in view.
        let view = window.root(&mut cx).unwrap();
        view.update(&mut cx, |v, _| {
            v.list.state.scroll_to(ListOffset {
                item_ix: 3000,
                offset_in_item: px(0.0),
            })
        });
        draw(&mut cx);
        let row_top = view.read_with(&cx, |v, _| v.list.state.bounds_for_item(3000).map(|b| b.top()));
        view.update(&mut cx, |v, _| {
            let mut grown = vec![key(0, 0), key(9000, 9000)];
            grown.extend(keys(1..6000));
            v.keys = grown;
        });
        let refreshed = draw(&mut cx);
        assert!(refreshed < 60, "the refresh built {refreshed}");
        let moved = view.read_with(&cx, |v, _| v.list.state.bounds_for_item(3001).map(|b| b.top()));
        assert_eq!(moved, row_top, "the row on screen didn't move");
    }
}
