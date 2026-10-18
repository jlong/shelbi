//! The board's virtualized columns (`dt-board-perf`): each expanded column's
//! cards, and the rows under them, are one gpui `list`, which builds only the
//! items in or near view and measures them as they come in. A Done column of
//! a thousand cards then costs a frame what its visible dozen do, where
//! building every card of every column on every frame (a hover, a scroll
//! tick, any notify in the window) made the board lag.
//!
//! The list keeps its scroll position as an item and an offset into it, so
//! measuring a card that scrolls in above the view never moves what is on
//! screen. [`ColumnList::sync`] tells the list which items changed when the
//! board's model does, keeping the measured heights of the rest and the
//! scroll position on the card it was on.

use std::ops::Range;

use gpui::{px, ListAlignment, ListOffset, ListState};
use shelbi_app::IssueColumn;

/// How far past the view a column measures cards (they are built only when
/// in view).
const OVERDRAW: f32 = 400.0;

/// One item of a column's list: its cards in order, then the rows under them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Item {
    Card(String),
    /// An empty column's placeholder (or "Loading board…").
    Empty,
    /// "Loading history…" under the Done column.
    LoadingHistory,
    /// The Done column's "Load more".
    LoadMore,
}

/// The rows a column shows under its cards, in order.
pub(super) fn trailing(empty: bool, is_done: bool, done_loading: bool, has_more: bool) -> Vec<Item> {
    let mut rows = Vec::new();
    if empty && !(is_done && done_loading) {
        rows.push(Item::Empty);
    }
    if is_done && done_loading {
        rows.push(Item::LoadingHistory);
    }
    if is_done && has_more {
        rows.push(Item::LoadMore);
    }
    rows
}

/// A column's list and the items it was last told about.
pub(super) struct ColumnList {
    pub state: ListState,
    items: Vec<Item>,
}

impl ColumnList {
    pub fn new() -> Self {
        ColumnList {
            state: ListState::new(0, ListAlignment::Top, px(OVERDRAW)),
            items: Vec::new(),
        }
    }

    /// Bring the list in line with `col`'s cards and `rows`. A no-op (and no
    /// allocation) when nothing changed.
    pub fn sync(&mut self, col: &IssueColumn, rows: &[Item]) {
        if same(&self.items, col, rows) {
            return;
        }
        let new: Vec<Item> = col
            .cards
            .iter()
            .map(|c| Item::Card(c.id.clone()))
            .chain(rows.iter().cloned())
            .collect();
        let top = self.state.logical_scroll_top();
        let anchor = self.items.get(top.item_ix).cloned();
        let (range, count) = changed(&self.items, &new);
        self.state.splice(range, count);
        // Stay on the card the view was on, wherever it moved.
        if let Some(ix) = anchor.and_then(|a| new.iter().position(|i| *i == a)) {
            if ix != self.state.logical_scroll_top().item_ix || top.offset_in_item != px(0.0) {
                self.state.scroll_to(ListOffset {
                    item_ix: ix,
                    offset_in_item: top.offset_in_item,
                });
            }
        }
        self.items = new;
    }
}

impl ColumnList {
    /// Scroll item `ix` into view by the least scroll (the keyboard's
    /// selection), as the column's scroll container did: nothing when it is
    /// in view, its top to the view's top when it is above, its bottom to
    /// the view's bottom when it runs past. A card far below the view (Up
    /// from the first card wraps to the last) has no measured position yet:
    /// it is scrolled to the top, and the list fills the view upward from it
    /// when it is near the end.
    pub fn reveal(&self, ix: usize) {
        let top = self.state.logical_scroll_top().item_ix;
        let view = self.state.viewport_bounds();
        let to_top = || {
            self.state.scroll_to(ListOffset {
                item_ix: ix,
                offset_in_item: px(0.0),
            })
        };
        match self.state.bounds_for_item(ix) {
            _ if ix < top => to_top(),
            Some(b) if b.top() < view.top() => to_top(),
            Some(b) if b.bottom() > view.bottom() => {
                self.state.scroll_by((b.bottom() - view.bottom()).min(b.top() - view.top()))
            }
            Some(_) => {}
            None => to_top(),
        }
    }
}

fn same(old: &[Item], col: &IssueColumn, rows: &[Item]) -> bool {
    old.len() == col.cards.len() + rows.len()
        && old
            .iter()
            .zip(col.cards.iter())
            .all(|(o, c)| matches!(o, Item::Card(id) if *id == c.id))
        && old[col.cards.len()..] == *rows
}

/// The old items' range that `new` replaces and how many items replace it:
/// everything between the longest common prefix and suffix.
pub(super) fn changed(old: &[Item], new: &[Item]) -> (Range<usize>, usize) {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    (prefix..old.len() - suffix, new.len() - prefix - suffix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{
        div, point, prelude::*, px, size, Bounds, Context, TestAppContext, VisualTestContext, Window,
    };
    use shelbi_app::IssueCard;
    use shelbi_core::StatusCategory;

    fn card(id: &str) -> IssueCard {
        IssueCard {
            id: id.into(),
            title: id.into(),
            summary: String::new(),
            blocked: false,
            branch: None,
            workflow: None,
            workflow_name: "task".into(),
            assignee: None,
            priority: 1,
            depends_on: Vec::new(),
            status_id: "done".into(),
            stored_status: "done".into(),
            updated_at: String::new(),
        }
    }

    fn column(ids: impl IntoIterator<Item = String>) -> IssueColumn {
        IssueColumn {
            status_id: "done".into(),
            status_name: "Done".into(),
            label: "DONE".into(),
            category: StatusCategory::Done,
            collapsed: false,
            cards: ids.into_iter().map(|id| card(&id)).collect(),
        }
    }

    fn ids(range: std::ops::Range<usize>) -> Vec<String> {
        range.map(|i| format!("c{i}")).collect()
    }

    fn items(ids: &[&str]) -> Vec<Item> {
        ids.iter().map(|id| Item::Card(id.to_string())).collect()
    }

    #[test]
    fn changed_spans_only_what_differs() {
        let old = items(&["a", "b", "c", "d"]);
        assert_eq!(changed(&old, &old), (4..4, 0), "nothing");
        assert_eq!(changed(&old, &items(&["a", "b", "x", "c", "d"])), (2..2, 1), "an insert");
        assert_eq!(changed(&old, &items(&["a", "c", "d"])), (1..2, 0), "a removal");
        assert_eq!(changed(&old, &items(&["a", "x", "c", "d"])), (1..2, 1), "a replacement");
        assert_eq!(changed(&old, &items(&["d", "a", "b", "c"])), (0..4, 4), "a rotation");
        assert_eq!(changed(&[], &items(&["a"])), (0..0, 1), "the first load");
        assert_eq!(changed(&items(&["a", "a"]), &items(&["a"])), (1..2, 0), "repeats");
    }

    #[test]
    fn trailing_rows_follow_the_column() {
        use Item::*;
        assert_eq!(trailing(true, false, false, false), vec![Empty]);
        assert_eq!(trailing(false, false, false, false), vec![]);
        // The Done column: loading its history instead of the empty row,
        // then its Load more.
        assert_eq!(trailing(true, true, true, true), vec![LoadingHistory, LoadMore]);
        assert_eq!(trailing(false, true, false, true), vec![LoadMore]);
        assert_eq!(trailing(true, true, false, false), vec![Empty]);
    }

    #[test]
    fn sync_tells_the_list_of_each_change_and_keeps_the_scroll_on_its_card() {
        let mut list = ColumnList::new();
        list.sync(&column(ids(0..100)), &[Item::LoadMore]);
        assert_eq!(list.state.item_count(), 101);
        // Scrolled to card 40, 12px into it.
        list.state.scroll_to(ListOffset {
            item_ix: 40,
            offset_in_item: px(12.0),
        });
        // A card lands at the top (the Done column is newest first).
        let mut newer = vec!["new".to_string()];
        newer.extend(ids(0..100));
        list.sync(&column(newer.clone()), &[Item::LoadMore]);
        assert_eq!(list.state.item_count(), 102);
        let top = list.state.logical_scroll_top();
        assert_eq!((top.item_ix, top.offset_in_item), (41, px(12.0)), "still on card c40");
        // The same board again changes nothing.
        list.sync(&column(newer), &[Item::LoadMore]);
        assert_eq!(list.state.logical_scroll_top().item_ix, 41);
        // Card c40 itself leaves: the list stays where the change was.
        let mut without = vec!["new".to_string()];
        without.extend(ids(0..100).into_iter().filter(|id| id != "c40"));
        list.sync(&column(without), &[]);
        assert_eq!(list.state.item_count(), 100, "the Load more row gone too");
        assert_eq!(list.state.logical_scroll_top().item_ix, 41);
    }

    /// A column the way the board draws one: a list over a [`ColumnList`],
    /// counting the cards it builds.
    struct Column {
        list: ColumnList,
        col: IssueColumn,
        built: Rc<Cell<usize>>,
    }

    impl Render for Column {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.list.sync(&self.col, &[]);
            let built = self.built.clone();
            div().flex().flex_col().size_full().child(
                gpui::list(self.list.state.clone(), move |_, _, _| {
                    built.set(built.get() + 1);
                    div().h(px(80.0)).into_any_element()
                })
                .with_sizing_behavior(gpui::ListSizingBehavior::Auto)
                .flex_1(),
            )
        }
    }

    #[gpui::test]
    fn a_long_column_builds_only_the_cards_near_view(cx: &mut TestAppContext) {
        let built = Rc::new(Cell::new(0));
        let b = built.clone();
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(300.0), px(800.0)));
        let window = cx.update(|cx| {
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|_| Column {
                        list: ColumnList::new(),
                        col: column(ids(0..1000)),
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
        // 800px of 80px cards in view, plus the overdraw measured once.
        let first = draw(&mut cx);
        assert!(first < 30, "the first frame built {first} of 1000 cards");
        let again = draw(&mut cx);
        assert!(again <= 11, "a redraw builds the visible cards only: {again}");
        // Reveal the last card (Up from the first wraps to it), never
        // measured: the view ends on it, built a screenful at a time.
        let view = window.root(&mut cx).unwrap();
        view.update(&mut cx, |v, _| v.list.reveal(999));
        let far = draw(&mut cx);
        assert!(far < 30, "revealing the last card built {far}");
        let bottom = view.read_with(&cx, |v, _| v.list.state.bounds_for_item(999));
        assert_eq!(bottom.map(|b| b.bottom()), Some(px(800.0)), "the last card ends the view");
        // A card just below the view (Down from the last visible one) is
        // revealed by the least scroll: it ends the view.
        view.update(&mut cx, |v, _| v.list.state.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.0) }));
        draw(&mut cx);
        view.update(&mut cx, |v, _| v.list.reveal(10));
        draw(&mut cx);
        let next = view.read_with(&cx, |v, _| v.list.state.bounds_for_item(10));
        assert_eq!(next.map(|b| b.bottom()), Some(px(800.0)));
        // A card in view doesn't move.
        let at = |cx: &mut VisualTestContext| {
            let top = view.read_with(cx, |v, _| v.list.state.logical_scroll_top());
            (top.item_ix, top.offset_in_item)
        };
        let before = at(&mut cx);
        view.update(&mut cx, |v, _| v.list.reveal(3));
        draw(&mut cx);
        assert_eq!(at(&mut cx), before);
        // One above the view comes in at the top.
        view.update(&mut cx, |v, _| v.list.reveal(0));
        draw(&mut cx);
        assert_eq!(at(&mut cx), (0, px(0.0)));
    }
}
