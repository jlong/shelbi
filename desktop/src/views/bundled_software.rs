//! The Acknowledgements window (`dt-about-bundled-software`, Figma "Bundled
//! Software · Dark", 2453:8397): the third-party software, fonts and icons
//! the app ships ([`crate::licenses`]), each with its full license.
//!
//! The titlebar row holds the traffic lights, the title and Done (Primary),
//! which closes the window as ⌘W does. On the left (280pt, the toolbar's
//! tint) a search over the names and licenses, the list (name, license
//! under it; the selected row in the row-selected fill, ↑ / ↓ move it, ⌘F
//! searches), filling the pane. On the right the selected entry: its name,
//! who made it and what it is, View source (its homepage or repository in
//! the browser), what it does in Shelbi, a rule, then its license under the
//! license's name, scrollable, selectable and copyable. The window opens on
//! the first entry, at 1000 × 760, and resizes down to [`MIN_SIZE`].

use std::collections::HashMap;

use gpui::{
    div, prelude::*, px, rems, AnyElement, ClickEvent, Context, Entity, FocusHandle, Focusable, KeyDownEvent,
    ScrollHandle, SharedString, StyleRefinement, Subscription, Window,
};
use gpui_kit::base::text::{TextView, TextViewState, TextViewStyle};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use shelbi_app::theme::Appearance;

use crate::app::CloseWindow;
use crate::assets::icons;
use crate::licenses::{self, entries, Browser, Entry};
use crate::theme::style::{self, ButtonContent, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::{self, Theme};
use crate::views::about::{heading_type, open_url};
use crate::views::sidebar::{BESIDE_TRAFFIC_LIGHTS, TITLEBAR_H};

/// The window's size (the frame's).
pub const SIZE: (f32, f32) = (1000.0, 760.0);
/// The smallest it resizes to: the list beside a readable license.
pub const MIN_SIZE: (f32, f32) = (720.0, 480.0);
/// The list's column.
pub const LIST_W: f32 = 280.0;
/// A list row.
const ROW_H: f32 = 48.0;
/// The license text's paragraph gap: one blank line of its 18pt.
const PARAGRAPH_GAP: f32 = 18.0;

/// The Acknowledgements window.
pub struct BundledSoftwareView {
    browser: Browser,
    search: Entity<InputState>,
    focus: FocusHandle,
    list_scroll: ScrollHandle,
    /// Each entry's license, parsed the first time it shows (the parse runs
    /// off the UI thread) and kept, with its scroll, while the window is
    /// open.
    texts: HashMap<usize, Entity<TextViewState>>,
    _subscriptions: Vec<Subscription>,
}

impl BundledSoftwareView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx));
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        theme::window::apply_to(window, cx);
        let subscriptions = vec![
            theme::follow_system(window, cx),
            cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    let query = input.read(cx).value().to_string();
                    this.browser.set_query(&query);
                    this.reveal_selected();
                    cx.notify();
                }
            }),
        ];
        BundledSoftwareView {
            browser: licenses::browser(),
            search,
            focus,
            list_scroll: ScrollHandle::new(),
            texts: HashMap::new(),
            _subscriptions: subscriptions,
        }
    }

    #[cfg(any(test, feature = "visual-check"))]
    pub fn browser(&self) -> &Browser {
        &self.browser
    }

    /// Select entry `ix` (a clicked row).
    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.browser.select(ix);
        self.reveal_selected();
        cx.notify();
    }

    /// ↑ / ↓: the row above or below, scrolled into view.
    pub fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.browser.step(delta);
        self.reveal_selected();
        cx.notify();
    }

    /// Search for `query`, as typing it into the field would.
    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.set_value(query.to_string(), window, cx));
        self.browser.set_query(query);
        self.reveal_selected();
        cx.notify();
    }

    /// Scroll the list so the selected row shows.
    fn reveal_selected(&self) {
        if let Some(sel) = self.browser.selected() {
            if let Some(at) = self.browser.visible().iter().position(|i| *i == sel) {
                self.list_scroll.scroll_to_item(at);
            }
        }
    }

    /// Esc in the search: clear it, or, empty, hand the keys back to the
    /// list.
    fn escape_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.browser.query().is_empty() {
            self.focus.focus(window, cx);
        } else {
            self.set_query("", window, cx);
        }
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let m = k.modifiers;
        let plain = !m.control && !m.alt && !m.platform && !m.shift && !m.function;
        match k.key.as_str() {
            "up" if plain => self.step(-1, cx),
            "down" if plain => self.step(1, cx),
            "f" if m.platform && !m.control && !m.alt && !m.shift => {
                let focus = self.search.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The license view for entry `ix`, parsed on first use.
    fn text_state(&mut self, ix: usize, entry: &Entry, cx: &mut Context<Self>) -> Entity<TextViewState> {
        self.texts
            .entry(ix)
            .or_insert_with(|| {
                let markdown = licenses::license_markdown(entry.license_title, entry.text);
                cx.new(|cx| TextViewState::markdown(&markdown, cx))
            })
            .clone()
    }

    fn render_header(&self, theme: &Theme) -> impl IntoElement {
        let done = style::button("bundled-done", "Done", ButtonKind::Primary, ButtonSize::Small, false, theme)
            .on_click(|_: &ClickEvent, window, _| window.remove_window());
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .h(px(TITLEBAR_H))
            .pl(px(BESIDE_TRAFFIC_LIGHTS))
            .pr(px(12.0))
            .bg(theme.toolbar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .type_style(Type::BaseSemibold)
                    .text_color(theme.text_loud)
                    .child("Acknowledgements"),
            )
            .child(done)
    }

    fn render_search(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.browser.query().is_empty();
        let focus = self.search.read(cx).focus_handle(cx);
        let focused = focus.is_focused(window);
        let field = div()
            .debug_selector(|| "bundled-search-field".into())
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .type_style(Type::Base)
            .text_color(theme.form_text)
            .child(
                Input::new(&self.search)
                    .appearance(false)
                    .px(px(0.0))
                    .py(px(0.0))
                    .h_full()
                    .type_style(Type::Base),
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
                        .child("Search acknowledgements"),
                )
            });
        let refocus = focus.clone();
        style::search_box_base("bundled-search", field, focused, theme)
            .w_full()
            .on_click(move |_, window, cx| window.focus(&focus, cx))
            .when(!empty, |d| {
                d.child(style::search_clear("bundled-search-clear", theme).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.set_query("", window, cx);
                        window.focus(&refocus, cx);
                        cx.stop_propagation();
                    },
                )))
            })
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.escape_search(window, cx);
                cx.stop_propagation();
            }))
    }

    fn render_row(&self, ix: usize, entry: &Entry, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.browser.selected() == Some(ix);
        let id = SharedString::from(format!("bundled-row-{ix}"));
        let selector = id.clone();
        div()
            .id(id)
            .debug_selector(move || selector.to_string())
            .flex()
            .flex_col()
            .flex_none()
            .justify_center()
            .gap(px(2.0))
            .h(px(ROW_H))
            .px(px(8.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .when(selected, |d| d.bg(theme.row_selected))
            .child(
                div()
                    .type_style(Type::SmallMedium)
                    .text_color(if selected { theme.text_loud } else { theme.foreground })
                    .truncate()
                    .child(entry.name),
            )
            .child(div().type_style(Type::XSmall).text_color(theme.muted).truncate().child(entry.license))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.select(ix, cx)))
            .into_any_element()
    }

    fn render_list(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<AnyElement> = self
            .browser
            .visible()
            .into_iter()
            .map(|ix| self.render_row(ix, &entries()[ix], theme, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(LIST_W))
            .h_full()
            .gap(px(16.0))
            .p(px(16.0))
            .bg(theme.toolbar)
            .child(self.render_search(theme, window, cx))
            .child(
                div()
                    .id("bundled-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.list_scroll)
                    .children(rows),
            )
    }

    fn render_detail(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let pane = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme.canvas);
        let Some(ix) = self.browser.selected() else {
            return pane
                .items_center()
                .justify_center()
                .type_style(Type::Small)
                .text_color(theme.muted)
                .child("Nothing matches the search.")
                .into_any_element();
        };
        let entry = &entries()[ix];
        let source = entry.source;
        let view_source = style::content_button(
            "bundled-view-source",
            ButtonContent::label("View source").right(icons::ARROW_TOP_RIGHT_ON_SQUARE),
            ButtonKind::Secondary,
            ButtonSize::Small,
            false,
            theme,
        )
        .on_click(move |_: &ClickEvent, _, cx| open_url(source, cx));
        let identity = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.0))
                    .child(
                        heading_type(div())
                            .debug_selector(|| "bundled-name".into())
                            .text_color(theme.text_loud)
                            .child(entry.name),
                    )
                    .child(
                        div()
                            .type_style(Type::Small)
                            .text_color(theme.foreground)
                            .child(entry.attribution.clone()),
                    ),
            )
            .child(view_source);
        let state = self.text_state(ix, entry, cx);
        let license = TextView::new(&state)
            .style(text_style(theme))
            .scrollable(true)
            .size_full();
        pane.gap(px(24.0))
            .p(px(24.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(12.0))
                    .child(identity)
                    .child(
                        div()
                            .type_style(Type::Small)
                            .text_color(theme.foreground)
                            .child(entry.description),
                    ),
            )
            .child(div().flex_none().h(px(1.0)).w_full().bg(theme.divider))
            .child(
                div()
                    .id("bundled-license")
                    .debug_selector(|| "bundled-license".into())
                    .flex_1()
                    .min_h_0()
                    .type_style(Type::Small)
                    .text_color(theme.foreground)
                    .child(license),
            )
            .into_any_element()
    }

    /// The selected entry's license view, for tests.
    #[cfg(test)]
    pub fn license_state(&self) -> Option<Entity<TextViewState>> {
        self.texts.get(&self.browser.selected()?).cloned()
    }
}

/// The license text's style: `Desktop/Small/Regular` paragraphs a blank
/// line apart; the license's name over it in `Desktop/Base/Semibold`, loud,
/// 12pt above the text; its section headings (`PREAMBLE`) in
/// `Desktop/Small/Medium`.
fn text_style(theme: &Theme) -> TextViewStyle {
    let loud = theme.text_loud;
    let body = theme.foreground;
    TextViewStyle::default()
        .with_foreground(theme.foreground)
        .with_muted_foreground(theme.muted)
        .with_link(theme.link)
        .with_selection(theme.text_selection)
        .with_border(theme.divider)
        .with_paragraph_gap(rems(PARAGRAPH_GAP / 16.0))
        .with_heading(move |level| {
            let (ty, color, below) = if level == 1 {
                (Type::BaseSemibold, loud, 12.0)
            } else {
                (Type::SmallMedium, body, PARAGRAPH_GAP)
            };
            StyleRefinement::default().type_style(ty).text_color(color).pt(px(0.0)).pb(px(below))
        })
        .with_dark(theme.appearance == Appearance::Dark)
}

impl Render for BundledSoftwareView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let list = self.render_list(&theme, window, cx);
        let detail = self.render_detail(&theme, cx);
        div()
            .id("bundled")
            .track_focus(&self.focus)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .capture_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.canvas)
            .font_family(style::UI_FAMILY)
            .child(self.render_header(&theme))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .child(div().flex_none().w(px(1.0)).h_full().bg(theme.divider))
                    .child(detail),
            )
    }
}

/// Open the window as [`super::about::show_bundled_software`] does, but
/// a new one every time.
#[cfg(test)]
pub fn open_for_test(cx: &mut gpui::App) -> gpui::WindowHandle<BundledSoftwareView> {
    cx.open_window(super::about::options("Acknowledgements", SIZE, true, cx), |window, cx| {
        cx.new(|cx| BundledSoftwareView::new(window, cx))
    })
    .expect("open Acknowledgements")
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::views::about::tests::{init, OPENED};

    fn open(cx: &mut TestAppContext) -> (Entity<BundledSoftwareView>, &mut VisualTestContext) {
        init(cx);
        let (view, vcx) = cx.add_window_view(BundledSoftwareView::new);
        vcx.run_until_parked();
        (view, vcx)
    }

    #[gpui::test]
    fn it_opens_on_the_first_entry_and_the_arrows_move_the_selection(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        assert_eq!(view.read_with(cx, |v, _| v.browser().selected()), Some(0));
        assert!(cx.debug_bounds("bundled-row-0").is_some());
        cx.simulate_keystrokes("down down");
        assert_eq!(view.read_with(cx, |v, _| v.browser().selected()), Some(2));
        cx.simulate_keystrokes("up");
        assert_eq!(view.read_with(cx, |v, _| v.browser().selected()), Some(1));
        // A click selects.
        let row = cx.debug_bounds("bundled-row-4").unwrap();
        cx.simulate_click(row.center(), Modifiers::none());
        assert_eq!(view.read_with(cx, |v, _| v.browser().selected()), Some(4));
    }

    #[gpui::test]
    fn the_search_narrows_the_list(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        view.update_in(cx, |v, window, cx| v.set_query("ofl", window, cx));
        cx.run_until_parked();
        let names: Vec<&str> = view.read_with(cx, |v, _| {
            v.browser().visible().into_iter().map(|i| entries()[i].name).collect()
        });
        assert_eq!(names, ["Geist", "Geist Mono", "Source Code Pro"]);
        assert!(cx.debug_bounds("bundled-search-clear").is_some(), "a query shows the clear button");
        let clear = cx.debug_bounds("bundled-search-clear").unwrap();
        cx.simulate_click(clear.center(), Modifiers::none());
        assert_eq!(view.read_with(cx, |v, _| v.browser().visible().len()), entries().len());
    }

    #[gpui::test]
    fn the_detail_shows_the_full_license_selectable_and_view_source_opens(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let ix = entries().iter().position(|e| e.name == "Source Code Pro").unwrap();
        view.update(cx, |v, cx| v.select(ix, cx));
        cx.run_until_parked();
        let state = view.read_with(cx, |v, _| v.license_state()).expect("the license view");
        // Select all of it, as ⌘A would: the copy is the shipped text,
        // unescaped, under the license's name.
        state.update(cx, |s, cx| s.select_all(cx));
        let copied = state.read_with(cx, |s, _| s.selected_text());
        assert!(copied.starts_with("SIL Open Font License 1.1"), "{copied:.80}");
        assert!(copied.contains("© 2023 Adobe (http://www.adobe.com/), with Reserved Font Name 'Source'."));
        assert!(copied.contains("\"Font Software\" refers to the set of files"));
        assert!(copied.contains("OTHER DEALINGS IN THE FONT SOFTWARE."));

        OPENED.with(|o| o.borrow_mut().clear());
        let button = cx.debug_bounds("bundled-view-source").unwrap();
        cx.simulate_click(button.center(), Modifiers::none());
        assert_eq!(OPENED.with(|o| o.borrow().clone()), ["https://github.com/adobe-fonts/source-code-pro"]);
    }

    #[gpui::test]
    fn done_and_cmd_w_close_it(cx: &mut TestAppContext) {
        init(cx);
        let before = cx.update(|cx| cx.windows().len());
        let window = cx.update(open_for_test);
        cx.run_until_parked();
        let vcx = &mut VisualTestContext::from_window(window.into(), cx);
        let done = vcx.debug_bounds("bundled-done").unwrap();
        vcx.simulate_click(done.center(), Modifiers::none());
        vcx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), before, "Done closed it");

        let window = cx.update(open_for_test);
        cx.run_until_parked();
        window.update(cx, |_, window, cx| window.dispatch_action(Box::new(CloseWindow), cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), before, "⌘W closed it");
    }
}
