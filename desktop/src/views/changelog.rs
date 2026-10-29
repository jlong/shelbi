//! The Changelog page: the project's record of what has shipped, as an
//! old-style blog over the shared model (`shelbi_app::changelog`). The TUI
//! draws the same [`ChangelogModel`].
//!
//! The Figma file has the sidebar row (the rocket, ⌘L, in the Navigation
//! component) but no page, so the page follows the app's other views: the
//! Issues Header's 52pt band with its Searchbox at the right, then the
//! entries newest first in one centered column of comfortable measure
//! ([`MEASURE`]). Each entry is its title (`Desktop/Heading`, loud), its
//! date (muted), its description through the app's Markdown renderer
//! (`markdown.rs`), and a chip per link, with a divider between entries.
//! Every link is a chip, so the description is drawn without the list of
//! bare links the changelog template ends it with
//! ([`ChangelogEntryView::prose`]).
//! The band's subtitle is the month of the entry at the top of the view, so
//! the date stays in sight however far down a long entry the reader is.
//!
//! Reads never run on the UI thread: a background worker re-reads when the
//! changelog directory's stamp changes ([`ChangelogWatch`]), polled on the
//! TUI's 750ms cadence, so an entry the orchestrator adds appears live. The
//! entries are a virtualized `gpui::list` that builds only the ones in
//! view; the model shows [`shelbi_app::changelog::PAGE_SIZE`] at a time and
//! the next page loads when the row under the last entry scrolls into view.
//!
//! Search (⌘F or `/`) narrows to the entries holding every word of the
//! query and highlights the words in the titles and descriptions (and draws
//! a matching link's chip in the Select style).
//!
//! Keys, listed in the footer: ↑/↓ scroll a line, Space / ⇧Space and
//! PgDn/PgUp a page, Home/End (⌘↑/⌘↓) the ends, J/K or ⌥↓/⌥↑ the next and
//! previous entry, ⌘F or `/` search, Esc clear the search.
//!
//! Links: an issue chip opens the issue in Shelbi (its detail on the board,
//! [`ChangelogEvent::OpenIssue`]); a PR or URL chip opens the browser. A PR
//! number with no known GitHub repository draws disabled.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Mutex;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    HighlightStyle,
    KeyDownEvent, Keystroke, ListOffset, Pixels, SharedString, StyledText, Subscription, Task, Window,
};
use gpui_kit::base::text::{RangeHighlight, RenderedText, TextViewState};
use gpui_kit::component::input::{Enter, Escape, Input, InputEvent, InputState};
use shelbi_app::changelog::{
    self, ChangelogData, ChangelogEntryView, ChangelogLink, ChangelogModel, ChangelogWatch, LinkKind, LinkTarget,
};

use super::activity_list::FeedList;
use super::activity_state::ItemKey;
use super::{board_scroll, markdown};
use crate::assets::{icons, Icon};
use crate::theme::style::{self, ButtonContent, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// How often the view asks whether the changelog changed (the TUI's
/// cadence). The check is one directory listing, off the UI thread.
const REFRESH_INTERVAL: Duration = Duration::from_millis(750);
/// The band's height and its search field's width (the Issues Header's).
const HEADER_H: f32 = 52.0;
const SEARCH_W: f32 = 220.0;
/// The column's widest: about 75 characters of `Desktop/Base`, a
/// comfortable measure for reading.
const MEASURE: f32 = 680.0;
/// The room above and below each entry, and either side of the column.
const ENTRY_PAD_Y: f32 = 32.0;
const PAGE_PAD_X: f32 = 32.0;
/// One step of ↑/↓.
const LINE_STEP: f32 = 40.0;
/// How much of the previous page Space leaves in view.
const PAGE_OVERLAP: f32 = 48.0;
/// The key-hint footer's height, its rule included (Activity's).
const FOOTER_H: f32 = 27.0;

/// What the page asks of the window.
#[derive(Debug, Clone)]
pub enum ChangelogEvent {
    /// Show this issue's detail (on the Issues board).
    OpenIssue(String),
}

/// A key the page takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    LineDown,
    LineUp,
    PageDown,
    PageUp,
    Top,
    Bottom,
    NextEntry,
    PreviousEntry,
    Search,
    ClearSearch,
}

/// The page's key for `ks`, if it is one (see the module docs).
pub fn nav_for(ks: &Keystroke) -> Option<Nav> {
    let m = &ks.modifiers;
    let bare = !m.control && !m.alt && !m.platform && !m.function;
    let plain = bare && !m.shift;
    let only_alt = m.alt && !m.control && !m.platform && !m.shift;
    let only_cmd = m.platform && !m.control && !m.alt && !m.shift;
    Some(match ks.key.as_str() {
        "down" if only_alt => Nav::NextEntry,
        "up" if only_alt => Nav::PreviousEntry,
        "down" if only_cmd => Nav::Bottom,
        "up" if only_cmd => Nav::Top,
        "down" if plain => Nav::LineDown,
        "up" if plain => Nav::LineUp,
        "j" if plain => Nav::NextEntry,
        "k" if plain => Nav::PreviousEntry,
        "space" if bare && m.shift => Nav::PageUp,
        "space" if plain => Nav::PageDown,
        "pagedown" if plain => Nav::PageDown,
        "pageup" if plain => Nav::PageUp,
        "home" if plain => Nav::Top,
        "end" if plain => Nav::Bottom,
        "f" if only_cmd => Nav::Search,
        "/" if bare => Nav::Search,
        "escape" if plain => Nav::ClearSearch,
        _ => return None,
    })
}

/// The entry to move to from the view's top, `top`, among `entries`:
/// the next one's start, or the start of the one the top is in (the one
/// before it when the top is already at its start).
pub fn entry_target(top: ListOffset, entries: usize, next: bool) -> Option<usize> {
    if entries == 0 {
        return None;
    }
    let at_start = top.offset_in_item <= px(4.0);
    let ix = if next {
        top.item_ix + 1
    } else if at_start {
        top.item_ix.checked_sub(1)?
    } else {
        top.item_ix
    };
    (ix < entries).then_some(ix)
}

/// One entry's description, rendered by a text view the page keeps so it
/// can highlight the search's words in it.
struct Body {
    state: Entity<TextViewState>,
    /// The Markdown it was given.
    text: String,
    /// The rendered text and query it was last highlighted for.
    searched: Option<(RenderedText, String)>,
    _parsed: Subscription,
}

pub struct ChangelogView {
    model: ChangelogModel,
    focus: FocusHandle,
    needs_focus: bool,
    /// Reads when the changelog changed; `None` when it didn't.
    refresher: shelbi_app::Refresher<Option<ChangelogData>>,
    list: FeedList,
    /// The keys the list last synced to.
    synced: Vec<ItemKey>,
    bodies: HashMap<String, Body>,
    /// The band's search field, made on first draw (an input needs the
    /// window).
    search_input: Option<Entity<InputState>>,
    /// A page load asked for by the row under the entries, not yet run.
    loading_more: bool,
    /// An entry to bring into view (the palette's Changelog result) once
    /// it's read.
    reveal: Option<String>,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

impl EventEmitter<ChangelogEvent> for ChangelogView {}

impl ChangelogView {
    pub fn new(project: String, cx: &mut Context<Self>) -> Self {
        let watch = Mutex::new(ChangelogWatch::new(project.clone()));
        let refresher = shelbi_app::spawn_worker(move |_| {
            let changed = watch.lock().map(|mut w| w.changed()).unwrap_or(true);
            changed.then(|| changelog::read_changelog(&project))
        });
        refresher.request();
        let poll = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(REFRESH_INTERVAL).await;
            if this.update(cx, |view, cx| view.tick(cx)).is_err() {
                break;
            }
        });
        ChangelogView {
            model: ChangelogModel::new(),
            focus: cx.focus_handle(),
            needs_focus: true,
            refresher,
            list: FeedList::new(),
            synced: Vec::new(),
            bodies: HashMap::new(),
            search_input: None,
            loading_more: false,
            reveal: None,
            _subscriptions: Vec::new(),
            _poll: poll,
        }
    }

    /// The window is dropping this view (a project switch): stop its
    /// background reads without waiting on one in flight.
    pub fn stop_workers(&mut self) {
        self.refresher.stop_without_waiting();
    }

    /// Take keyboard focus on the next frame (the window calls this when the
    /// page is shown).
    pub fn request_focus(&mut self) {
        self.needs_focus = true;
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if let Some(Some(data)) = self.refresher.latest() {
            self.model.set_data(data);
            self.apply_reveal(cx);
            cx.notify();
        }
        self.refresher.request();
    }

    /// Every entry of the last read, newest first, or `None` before the
    /// first (the palette searches them).
    pub fn entries(&self) -> Option<Vec<ChangelogEntryView>> {
        self.model.data().map(|d| d.entries.clone())
    }

    /// Show entry `id` (the palette's Changelog result): the search
    /// cleared and the entry at the top of the list. Waits for the first
    /// read if it hasn't landed.
    pub fn reveal(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_search(window, cx);
        self.reveal = Some(id.to_string());
        self.apply_reveal(cx);
    }

    fn apply_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.reveal.clone() else {
            return;
        };
        if self.model.entry(&id).is_none() {
            return;
        }
        self.reveal = None;
        let position = |m: &ChangelogModel| m.visible().position(|e| e.id == id);
        while position(&self.model).is_none() && self.model.has_more() {
            self.model.load_more();
        }
        if let Some(ix) = position(&self.model) {
            self.sync_list();
            self.list.state.scroll_to(ListOffset {
                item_ix: ix,
                offset_in_item: Pixels::ZERO,
            });
        }
        cx.notify();
    }

    /// Fold in a read now (the offscreen checks drive the view by hand: their
    /// dispatcher doesn't run the poll timer).
    #[cfg(feature = "visual-check")]
    pub fn poll(&mut self, cx: &mut Context<Self>) {
        self.tick(cx);
    }

    /// The page's model, for the offscreen checks.
    #[cfg(any(test, feature = "visual-check"))]
    pub fn model(&self) -> &ChangelogModel {
        &self.model
    }

    /// Type `query` into the search field, as the keyboard would (the
    /// offscreen checks; an input's `set_value` sends no change event).
    #[cfg(any(test, feature = "visual-check"))]
    pub fn search_for(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_search(window, cx);
        if let Some(input) = &self.search_input {
            input.update(cx, |s, cx| s.set_value(query.to_string(), window, cx));
        }
        self.set_query(query.to_string(), cx);
    }

    /// Take a page key, as a key press would (the offscreen checks).
    #[cfg(feature = "visual-check")]
    pub fn press(&mut self, nav: Nav, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(nav, window, cx);
    }

    /// The list's top, for the offscreen checks.
    #[cfg(any(test, feature = "visual-check"))]
    pub fn scroll_top(&self) -> ListOffset {
        self.list.state.logical_scroll_top()
    }

    // ---- search -----------------------------------------------------------

    /// The band's search field: typing narrows the entries as it goes.
    fn search_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(input) = &self.search_input {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx));
        let sub = cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                let query = input.read(cx).value().to_string();
                this.set_query(query, cx);
            }
        });
        self._subscriptions.push(sub);
        self.search_input = Some(input.clone());
        input
    }

    fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        if query == self.model.query() {
            return;
        }
        self.model.set_query(query);
        // A new search reads from the top.
        self.list.state.scroll_to(ListOffset::default());
        let ids: Vec<String> = self.bodies.keys().cloned().collect();
        for id in ids {
            self.highlight_body(&id, cx);
        }
        cx.notify();
    }

    /// Put the keyboard in the search field.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.search_field(window, cx);
        let focus = input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn search_focused(&self, window: &Window, cx: &App) -> bool {
        self.search_input
            .as_ref()
            .is_some_and(|s| s.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Empty the search, leaving the keyboard where it is.
    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = &self.search_input {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.set_query(String::new(), cx);
    }

    /// Leave the search field for the entries: `clear` empties the search
    /// too (Esc), else it stays (Enter).
    fn leave_search(&mut self, clear: bool, window: &mut Window, cx: &mut Context<Self>) {
        if clear {
            self.clear_search(window, cx);
        }
        self.needs_focus = true;
        cx.notify();
    }

    /// Highlight the query's words in entry `id`'s description, once its
    /// text has rendered (a text view parses off the UI thread) and again
    /// whenever the text or the query changes.
    fn highlight_body(&mut self, id: &str, cx: &mut Context<Self>) {
        let query = self.model.query().to_string();
        let color = cx.global::<Theme>().search_match;
        let Some(body) = self.bodies.get_mut(id) else {
            return;
        };
        let text = body.state.read(cx).rendered_text();
        if body.searched.as_ref().is_some_and(|(t, q)| *t == text && *q == query) {
            return;
        }
        let ranges = changelog::match_ranges(text.as_str(), &query);
        body.searched = Some((text, query));
        body.state.update(cx, |s, cx| {
            if ranges.is_empty() {
                s.clear_range_highlights(cx);
            } else if s
                .set_range_highlights(ranges.into_iter().map(|r| RangeHighlight::new(r, color)), cx)
                .is_err()
            {
                // The text changed under the search; its next parse
                // searches again.
                s.clear_range_highlights(cx);
            }
        });
    }

    /// Entry `entry`'s description view, made the first time it is drawn.
    fn body_state(&mut self, entry: &ChangelogEntryView, cx: &mut Context<Self>) -> Entity<TextViewState> {
        if let Some(body) = self.bodies.get_mut(&entry.id) {
            if body.text != entry.prose() {
                // The entry was edited on disk: the same view, the new text.
                body.text = entry.prose().to_string();
                body.searched = None;
                let text = body.text.clone();
                body.state.update(cx, |s, cx| s.set_text(&text, cx));
            }
            return body.state.clone();
        }
        let text = entry.prose().to_string();
        let state = cx.new(|cx| TextViewState::markdown(&text, cx));
        let id = entry.id.clone();
        let parsed = cx.observe(&state, move |this, _, cx| this.highlight_body(&id, cx));
        self.bodies.insert(
            entry.id.clone(),
            Body {
                state: state.clone(),
                text,
                searched: None,
                _parsed: parsed,
            },
        );
        state
    }

    // ---- keys and scrolling -------------------------------------------------

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The search field owns its keys (its Esc and Enter come in as
        // actions, caught in `render_search`).
        if self.search_focused(window, cx) {
            return;
        }
        let Some(nav) = nav_for(&ev.keystroke) else {
            return;
        };
        if nav == Nav::ClearSearch && self.model.query().is_empty() {
            // Nothing to clear: Esc is the window's.
            return;
        }
        cx.stop_propagation();
        self.navigate(nav, window, cx);
    }

    fn navigate(&mut self, nav: Nav, window: &mut Window, cx: &mut Context<Self>) {
        let list = &self.list.state;
        let page = (list.viewport_bounds().size.height - px(PAGE_OVERLAP)).max(px(LINE_STEP));
        match nav {
            Nav::LineDown => list.scroll_by(px(LINE_STEP)),
            Nav::LineUp => list.scroll_by(px(-LINE_STEP)),
            Nav::PageDown => list.scroll_by(page),
            Nav::PageUp => list.scroll_by(-page),
            Nav::Top => list.scroll_to(ListOffset::default()),
            Nav::Bottom => list.scroll_to_end(),
            Nav::NextEntry | Nav::PreviousEntry => {
                let entries = self.model.visible().count();
                let top = list.logical_scroll_top();
                match entry_target(top, entries, nav == Nav::NextEntry) {
                    Some(ix) => list.scroll_to(ListOffset {
                        item_ix: ix,
                        offset_in_item: Pixels::ZERO,
                    }),
                    // Past the last entry: its end, and the next page.
                    None if nav == Nav::NextEntry => list.scroll_to_end(),
                    None => list.scroll_to(ListOffset::default()),
                }
            }
            Nav::Search => return self.focus_search(window, cx),
            Nav::ClearSearch => return self.clear_search(window, cx),
        }
        cx.notify();
    }

    /// The row under the entries is in view: show the next page.
    fn load_more(&mut self, cx: &mut Context<Self>) {
        self.loading_more = false;
        if self.model.has_more() {
            self.model.load_more();
            cx.notify();
        }
    }

    /// Tell the list what changed since it last drew: one item per shown
    /// entry (by id and content), then the row under them.
    fn sync_list(&mut self) {
        let mut keys: Vec<ItemKey> = self.model.visible().map(entry_key).collect();
        keys.push(ItemKey::of("changelog-end", (self.model.has_more(), self.model.match_count())));
        if keys != self.synced {
            self.list.sync(&keys);
            self.synced = keys;
        }
    }

    fn open_link(&mut self, target: &LinkTarget, cx: &mut Context<Self>) {
        match target {
            LinkTarget::Issue(id) => cx.emit(ChangelogEvent::OpenIssue(id.clone())),
            LinkTarget::Url(url) => cx.open_url(url),
            LinkTarget::Unresolved => {}
        }
    }

    // ---- rendering --------------------------------------------------------

    /// The band: the page's name and the month in view on the left, the
    /// search field on the right.
    fn render_header(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let subtitle = self.subtitle();
        let search = self.render_search(theme, window, cx);
        div()
            .debug_selector(|| "changelog-header".into())
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .pl_4()
            .pr_4()
            .h(px(HEADER_H))
            .flex_none()
            .bg(theme.toolbar)
            .border_l_1()
            .border_color(theme.canvas)
            .child(div().type_style(Type::BaseMedium).text_color(theme.text_loud).child("Changelog"))
            .when_some(subtitle, |d, s| {
                d.child(
                    div()
                        .debug_selector(|| "changelog-subtitle".into())
                        .type_style(Type::XSmall)
                        .text_color(theme.muted)
                        .child(s),
                )
            })
            .child(div().flex_1())
            .child(search)
    }

    /// Under the name: how many entries match a search, else the month of
    /// the entry at the top of the view.
    fn subtitle(&self) -> Option<String> {
        if !self.model.query().trim().is_empty() {
            let (n, total) = (self.model.match_count(), self.model.total());
            return Some(format!("{n} of {total} {}", if total == 1 { "entry" } else { "entries" }));
        }
        let top = self.list.state.logical_scroll_top().item_ix;
        let entry = self.model.visible().nth(top).or_else(|| self.model.visible().last())?;
        Some(entry.date.format("%B %Y").to_string())
    }

    /// The band's search field (the Issues Header's): the input inside
    /// [`style::search_box`], `Search` while it is empty, the clear button
    /// while it isn't. Esc clears it and Enter keeps it, both handing the
    /// keyboard back to the entries.
    fn render_search(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let input = self.search_field(window, cx);
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let empty = self.model.query().is_empty();
        let field = div()
            .debug_selector(|| "changelog-search-field".into())
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .type_style(Type::Small)
            .text_color(theme.foreground)
            .child(
                Input::new(&input)
                    .appearance(false)
                    .px(px(0.0))
                    .py(px(0.0))
                    .h_full()
                    .type_style(Type::Small),
            )
            .when(empty, |d| {
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .h_full()
                        .flex()
                        .items_center()
                        .text_color(theme.form_hint)
                        .child("Search"),
                )
            });
        let focus = input.read(cx).focus_handle(cx);
        let refocus = focus.clone();
        style::search_box("changelog-search", field, focused, theme)
            .w(px(SEARCH_W))
            .on_click(move |_, window, cx| window.focus(&focus, cx))
            .when(!empty, |d| {
                d.child(style::search_clear("changelog-search-clear", theme).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.clear_search(window, cx);
                        window.focus(&refocus, cx);
                        cx.stop_propagation();
                    },
                )))
            })
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.leave_search(true, window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Enter, window, cx| {
                this.leave_search(false, window, cx);
                cx.stop_propagation();
            }))
    }

    fn render_body(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let placeholder = |text: String| {
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_color(theme.muted)
                .child(text)
                .into_any_element()
        };
        let Some(data) = self.model.data() else {
            return placeholder("Loading…".into());
        };
        if let Some(error) = &data.error {
            return placeholder(format!("Couldn't read the changelog: {error}"));
        }
        if self.model.total() == 0 {
            return style::empty_state(
                "changelog-empty",
                "Nothing has shipped yet. As work lands, the orchestrator writes it up here, newest first.",
                None,
                theme,
            )
            .into_any_element();
        }
        if self.model.match_count() == 0 {
            let clear = style::button("changelog-clear-search", "Clear search", ButtonKind::Secondary, ButtonSize::Base, false, theme)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.clear_search(window, cx)));
            return style::empty_state(
                "changelog-no-match",
                format!("No entries match \u{201c}{}\u{201d}.", self.model.query().trim()),
                Some(clear),
                theme,
            )
            .into_any_element();
        }
        self.sync_list();
        let list = gpui::list(
            self.list.state.clone(),
            cx.processor(|this, ix: usize, window, cx| this.render_item(ix, window, cx)),
        );
        board_scroll::wheel_list(list, &self.list.state)
            .debug_selector(|| "changelog-entries".into())
            .into_any_element()
    }

    /// Item `ix`: an entry, or the row under them.
    fn render_item(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.global::<Theme>();
        let entry = self.model.visible().nth(ix).cloned();
        let inner = match entry {
            Some(entry) => {
                let last = ix + 1 == self.model.visible().count() && !self.model.has_more();
                self.render_entry(ix, &entry, last, &theme, cx).into_any_element()
            }
            None => {
                if self.model.has_more() && !self.loading_more {
                    // In view (or nearly): the next page, after this frame.
                    self.loading_more = true;
                    cx.defer_in(window, |this, _, cx| this.load_more(cx));
                }
                self.render_end(&theme).into_any_element()
            }
        };
        div()
            .w_full()
            .flex()
            .flex_row()
            .justify_center()
            .px(px(PAGE_PAD_X))
            .child(div().w_full().max_w(px(MEASURE)).child(inner))
            .into_any_element()
    }

    fn render_entry(
        &mut self,
        ix: usize,
        entry: &ChangelogEntryView,
        last: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query = self.model.query().to_string();
        let highlight = HighlightStyle {
            background_color: Some(theme.search_match),
            ..Default::default()
        };
        let title_marks: Vec<(Range<usize>, HighlightStyle)> = changelog::match_ranges(&entry.title, &query)
            .into_iter()
            .map(|r| (r, highlight))
            .collect();
        let body = (!entry.prose().is_empty()).then(|| {
            let state = self.body_state(entry, cx);
            markdown::body_with_state(&state, theme)
        });
        let chips: Vec<AnyElement> = entry
            .links
            .iter()
            .enumerate()
            .map(|(li, link)| self.render_chip(ix, li, link, &query, theme, cx))
            .collect();
        let id = entry.id.clone();
        div()
            .id(SharedString::from(format!("changelog-entry-{id}")))
            .debug_selector(move || format!("changelog-entry-{id}"))
            .flex()
            .flex_col()
            .py(px(ENTRY_PAD_Y))
            .when(!last, |d| d.border_b_1().border_color(theme.divider))
            .child(
                div()
                    .type_style(Type::Heading)
                    .text_color(theme.text_loud)
                    .child(StyledText::new(entry.title.clone()).with_highlights(title_marks)),
            )
            .child(
                div()
                    .mt(px(4.0))
                    .type_style(Type::Small)
                    .text_color(theme.muted)
                    .child(entry.date_label()),
            )
            .when_some(body, |d, body| d.child(div().mt(px(16.0)).type_style(Type::Base).child(body)))
            .when(!chips.is_empty(), |d| {
                d.child(div().mt(px(16.0)).flex().flex_row().flex_wrap().gap(px(6.0)).children(chips))
            })
    }

    /// A link's chip: the Button at Small with the link kind's icon. One
    /// whose label holds a searched word draws in the Select style; a PR
    /// with nowhere to go draws disabled.
    fn render_chip(
        &self,
        ix: usize,
        li: usize,
        link: &ChangelogLink,
        query: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = format!("changelog-link-{ix}-{li}");
        let content = ButtonContent::label(link.label.clone()).left(link_icon(link.kind));
        if link.target == LinkTarget::Unresolved {
            return style::disabled_content_button(id, content, ButtonKind::Secondary, ButtonSize::Small, theme)
                .tooltip(style::tooltip("No GitHub repository is known for this project"))
                .into_any_element();
        }
        let kind = if changelog::match_ranges(&link.label, query).is_empty() {
            ButtonKind::Secondary
        } else {
            ButtonKind::Select
        };
        let target = link.target.clone();
        let tip = match &link.target {
            LinkTarget::Issue(_) => "Open the issue".to_string(),
            LinkTarget::Url(url) => url.clone(),
            LinkTarget::Unresolved => String::new(),
        };
        style::content_button(id, content, kind, ButtonSize::Small, false, theme)
            .tooltip(style::tooltip(tip))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_link(&target, cx)))
            .into_any_element()
    }

    /// Under the entries: the next page on its way, or where the log starts.
    fn render_end(&self, theme: &Theme) -> impl IntoElement {
        let total = self.model.match_count();
        let text = if self.model.has_more() {
            "Loading more…".to_string()
        } else if total == 1 {
            "That\u{2019}s the whole changelog: 1 entry".to_string()
        } else if self.model.query().trim().is_empty() {
            format!("That\u{2019}s the whole changelog: {total} entries")
        } else {
            format!("{total} entries match")
        };
        div()
            .debug_selector(|| "changelog-end".into())
            .py(px(ENTRY_PAD_Y))
            .type_style(Type::XSmall)
            .text_color(theme.muted)
            .child(text)
    }

    fn render_footer(&self, theme: &Theme) -> impl IntoElement {
        let hints = "\u{2191}/\u{2193} scroll   space page   j/k or \u{2325}\u{2193}/\u{2325}\u{2191} next/previous entry   home/end top/bottom   \u{2318}F search   esc clear";
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .h(px(FOOTER_H))
            .px_4()
            .border_t_1()
            .border_color(theme.divider)
            .type_style(Type::XSmall)
            .text_color(theme.muted)
            .child(div().flex_1().truncate().child(hints))
    }
}

/// A link kind's chip icon: the Issues view's stack for an issue, the
/// arrows of a merge for a PR, the globe for any other URL.
pub fn link_icon(kind: LinkKind) -> Icon {
    match kind {
        LinkKind::Issue => icons::RECTANGLE_STACK,
        LinkKind::Pr => icons::ARROWS_RIGHT_LEFT,
        LinkKind::Url => icons::GLOBE_ALT,
    }
}

/// An entry's list key: its id, and what it reads (an edited entry is
/// re-measured in place).
fn entry_key(entry: &ChangelogEntryView) -> ItemKey {
    let ident = ItemKey::of("changelog-entry", &entry.id).ident;
    let content = ItemKey::of("changelog-entry", (&entry.title, &entry.description, entry.links.len())).content;
    ItemKey { ident, content }
}

impl Render for ChangelogView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.needs_focus {
            self.needs_focus = false;
            window.focus(&self.focus, cx);
        }
        let theme = *cx.global::<Theme>();
        let header = self.render_header(&theme, window, cx);
        let body = self.render_body(&theme, cx);
        div()
            .id("changelog")
            .track_focus(&self.focus)
            .key_context("Changelog")
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(theme.nested(theme.canvas))
            .type_style(Type::Base)
            .text_color(theme.foreground)
            .child(header)
            .child(body)
            .child(self.render_footer(&theme))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use shelbi_state::changelog::{add_entry, NewEntry, PrRef};
    use std::time::Instant;

    fn nav(spec: &str) -> Option<Nav> {
        nav_for(&Keystroke::parse(spec).unwrap())
    }

    #[test]
    fn the_page_keys() {
        assert_eq!(nav("down"), Some(Nav::LineDown));
        assert_eq!(nav("up"), Some(Nav::LineUp));
        assert_eq!(nav("j"), Some(Nav::NextEntry));
        assert_eq!(nav("k"), Some(Nav::PreviousEntry));
        assert_eq!(nav("alt-down"), Some(Nav::NextEntry));
        assert_eq!(nav("alt-up"), Some(Nav::PreviousEntry));
        assert_eq!(nav("cmd-down"), Some(Nav::Bottom));
        assert_eq!(nav("cmd-up"), Some(Nav::Top));
        assert_eq!(nav("home"), Some(Nav::Top));
        assert_eq!(nav("end"), Some(Nav::Bottom));
        assert_eq!(nav("space"), Some(Nav::PageDown));
        assert_eq!(nav("shift-space"), Some(Nav::PageUp));
        assert_eq!(nav("pagedown"), Some(Nav::PageDown));
        assert_eq!(nav("pageup"), Some(Nav::PageUp));
        assert_eq!(nav("cmd-f"), Some(Nav::Search));
        assert_eq!(nav("/"), Some(Nav::Search));
        assert_eq!(nav("escape"), Some(Nav::ClearSearch));
        // The window's chords and other keys pass by.
        assert_eq!(nav("cmd-l"), None);
        assert_eq!(nav("cmd-j"), None);
        assert_eq!(nav("shift-j"), None);
        assert_eq!(nav("x"), None);
    }

    #[test]
    fn next_and_previous_entry_from_the_top() {
        let at = |item_ix, offset: f32| ListOffset {
            item_ix,
            offset_in_item: px(offset),
        };
        assert_eq!(entry_target(at(0, 0.0), 3, true), Some(1));
        assert_eq!(entry_target(at(1, 120.0), 3, true), Some(2));
        assert_eq!(entry_target(at(2, 0.0), 3, true), None, "past the last");
        // Back: to the start of the entry the top is in, then the one before.
        assert_eq!(entry_target(at(2, 120.0), 3, false), Some(2));
        assert_eq!(entry_target(at(2, 0.0), 3, false), Some(1));
        assert_eq!(entry_target(at(0, 0.0), 3, false), None);
        assert_eq!(entry_target(at(0, 0.0), 0, true), None);
    }

    fn entry(title: &str, issues: &[&str], body: &str) -> NewEntry {
        NewEntry {
            title: title.into(),
            date: shelbi_state::changelog::now(),
            author: "orchestrator".into(),
            issues: issues.iter().map(|s| s.to_string()).collect(),
            prs: vec![PrRef::Url("https://github.com/a/b/pull/12".into())],
            urls: vec![],
            description: body.into(),
        }
    }

    /// Settle until `done` (the page's worker reads on its own thread).
    fn until(view: &Entity<ChangelogView>, cx: &mut VisualTestContext, done: impl Fn(&ChangelogModel) -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            view.update(cx, |v, cx| v.tick(cx));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
            if view.read_with(cx, |v, _| done(&v.model)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[gpui::test]
    fn reveal_clears_the_search_and_brings_the_entry_to_the_top(cx: &mut TestAppContext) {
        let _env = crate::test_env::lock();
        let home = std::env::temp_dir().join(format!("shbcl-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("projects")).unwrap();
        // SAFETY: the env lock is held.
        unsafe { std::env::set_var("SHELBI_HOME", &home) };
        let project = "clr";
        let mut ids = Vec::new();
        for i in 0..8 {
            let body = "A paragraph that takes some room.\n\n".repeat(4);
            let mut new = entry(&format!("Entry {i}"), &[], &body);
            // Each a minute after the last, so the first is the oldest.
            new.date -= chrono::Duration::minutes(10 - i);
            ids.push(add_entry(project, new).unwrap().id);
        }

        cx.update(|cx| {
            cx.set_global(Theme::resolve(shelbi_state::ThemePreference::Dark, gpui::WindowAppearance::Dark));
            gpui_kit::init(cx);
        });
        let handle = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| cx.new(|cx| ChangelogView::new(project.into(), cx)))
                .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(handle.into(), cx);
        let view = handle.root(&mut vcx).unwrap();
        // Asked before the first read lands: it waits for it.
        let oldest = ids[0].clone();
        vcx.update(|window, cx| view.update(cx, |v, cx| v.search_for("entry 7", window, cx)));
        vcx.update(|window, cx| view.update(cx, |v, cx| v.reveal(&oldest, window, cx)));
        assert!(until(&view, &mut vcx, |m| m.total() == 8), "the first read lands");
        view.update(&mut vcx, |v, cx| v.tick(cx));
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        vcx.run_until_parked();
        let (query, at, top) = view.read_with(&vcx, |v, _| {
            let at = v.model().visible().position(|e| e.id == oldest);
            (v.model().query().to_string(), at, v.scroll_top().item_ix)
        });
        assert_eq!(query, "", "the search is cleared");
        assert_eq!(at, Some(7), "the oldest entry, newest first");
        assert!(top > 0, "scrolled down to it (the list stops at its end): {top}");

        unsafe { std::env::remove_var("SHELBI_HOME") };
        let _ = std::fs::remove_dir_all(&home);
    }

    #[gpui::test]
    fn the_page_reads_live_searches_and_opens_issues(cx: &mut TestAppContext) {
        let _env = crate::test_env::lock();
        let home = std::env::temp_dir().join(format!("shbcl-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("projects")).unwrap();
        // SAFETY: the env lock is held.
        unsafe { std::env::set_var("SHELBI_HOME", &home) };
        let project = "cl";
        add_entry(project, entry("A faster board", &["dt-board-perf"], "Only the cards in view are built.")).unwrap();
        add_entry(project, entry("Contact Us", &["dt-feedback"], "Send a note.")).unwrap();

        cx.update(|cx| {
            cx.set_global(Theme::resolve(shelbi_state::ThemePreference::Dark, gpui::WindowAppearance::Dark));
            gpui_kit::init(cx);
        });
        let handle = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| cx.new(|cx| ChangelogView::new(project.into(), cx)))
                .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(handle.into(), cx);
        let view = handle.root(&mut vcx).unwrap();
        let opened = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let seen = opened.clone();
        vcx.update(|_, cx| {
            cx.subscribe(&view, move |_, ev: &ChangelogEvent, _| {
                let ChangelogEvent::OpenIssue(id) = ev;
                seen.borrow_mut().push(id.clone());
            })
            .detach();
        });

        assert!(until(&view, &mut vcx, |m| m.total() == 2), "the first read lands");
        let titles: Vec<String> = view.read_with(&vcx, |v, _| v.model.visible().map(|e| e.title.clone()).collect());
        assert_eq!(titles, ["Contact Us", "A faster board"], "newest first");
        assert!(vcx.debug_bounds("changelog-entries").is_some());
        assert!(vcx.debug_bounds("changelog-end").is_some(), "the row under the entries");

        // The first entry's issue chip opens the issue in Shelbi.
        let chip = vcx.debug_bounds("changelog-link-0-0").expect("the issue chip");
        vcx.simulate_click(chip.center(), gpui::Modifiers::none());
        vcx.run_until_parked();
        assert_eq!(*opened.borrow(), ["dt-feedback"]);

        // An entry added outside the page appears on its own.
        add_entry(project, entry("Changelog view", &["changelog-desktop"], "Blog layout.")).unwrap();
        assert!(until(&view, &mut vcx, |m| m.total() == 3), "the new entry appears live");

        // ⌘F, then a query: the entries narrow, the subtitle counts them.
        vcx.simulate_keystrokes("cmd-f");
        view.update_in(&mut vcx, |v, window, cx| {
            assert!(v.search_focused(window, cx), "⌘F puts the keyboard in the search");
            v.search_for("board", window, cx);
        });
        assert!(until(&view, &mut vcx, |m| m.match_count() == 1));
        assert_eq!(view.read_with(&vcx, |v, _| v.subtitle()).as_deref(), Some("1 of 3 entries"));
        // A query nothing matches says so, with a way back.
        view.update_in(&mut vcx, |v, window, cx| v.search_for("zzz", window, cx));
        assert!(until(&view, &mut vcx, |m| m.match_count() == 0));
        assert!(vcx.debug_bounds("changelog-no-match").is_some());
        // Esc in the entries clears it.
        view.update_in(&mut vcx, |v, window, cx| v.navigate(Nav::ClearSearch, window, cx));
        assert!(until(&view, &mut vcx, |m| m.query().is_empty() && m.match_count() == 3));

        view.update(&mut vcx, |v, _| v.stop_workers());
        unsafe { std::env::remove_var("SHELBI_HOME") };
        let _ = std::fs::remove_dir_all(&home);
    }

    #[gpui::test]
    fn an_empty_changelog_says_so(cx: &mut TestAppContext) {
        let _env = crate::test_env::lock();
        let home = std::env::temp_dir().join(format!("shbcl-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("projects")).unwrap();
        // SAFETY: the env lock is held.
        unsafe { std::env::set_var("SHELBI_HOME", &home) };
        cx.update(|cx| {
            cx.set_global(Theme::resolve(shelbi_state::ThemePreference::Light, gpui::WindowAppearance::Light));
            gpui_kit::init(cx);
        });
        let handle = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| cx.new(|cx| ChangelogView::new("cl-empty".into(), cx)))
                .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(handle.into(), cx);
        let view = handle.root(&mut vcx).unwrap();
        assert!(until(&view, &mut vcx, |m| !m.is_loading()), "the read lands");
        assert!(vcx.debug_bounds("changelog-empty").is_some());
        assert_eq!(view.read_with(&vcx, |v, _| v.subtitle()), None);
        view.update(&mut vcx, |v, _| v.stop_workers());
        unsafe { std::env::remove_var("SHELBI_HOME") };
        let _ = std::fs::remove_dir_all(&home);
    }
}
