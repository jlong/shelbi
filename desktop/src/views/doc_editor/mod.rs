//! The task pane's in-place editors: the title and the Markdown body, typed
//! into where they're read (`dt-task-wysiwyg-editor`).
//!
//! There is no edit mode. A [`DocEditor`] draws its text formatted (the
//! body's headings, lists, checklists, code and links as the frames set
//! them) whether or not it has the keys, so clicking into it puts a caret in
//! the text you were reading and nothing moves. The editing itself is
//! `shelbi_app::markdown_edit::Editor`'s, over the Markdown source: typing,
//! Enter and Backspace keep the Markdown well formed, the formatting
//! commands (⌘B, ⌘I, ⌘E, ⌘K, ⌥⌘1–3, ⇧⌘7/8/9, and a small toolbar over a
//! selection) rewrite only the syntax they touch, and a click on a checkbox
//! checks it. The view emits [`DocEvent`]s; the board hands each change to
//! the issue's draft, which saves itself.
//!
//! With the keys, the editor keeps them: every key it handles stops there
//! (so `s`, `a`, `e`, `h`/`j`/`k`/`l` and the arrows never reach the
//! board), typed text arrives through the platform's input method
//! (compositions included), and Esc hands the keys back to the pane.

mod element;
pub mod layout;

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    anchored, deferred, div, point, prelude::*, px, AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton, MouseDownEvent,
    Pixels, Point, Role, ScrollHandle, SharedString, Subscription, Task, UTF16Selection, Window,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use shelbi_app::markdown_edit::{word_left, word_right, BlockKind, Editor, MarkKind};

use self::element::DocElement;
use self::layout::{DocLayout, Field};
use crate::menus;
use crate::theme::style::{self, Type, Typography};
use crate::theme::Theme;

/// How long the caret shows, and hides, while it blinks.
const BLINK: Duration = Duration::from_millis(530);

/// What the editor tells its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocEvent {
    /// The text changed (typing, a command, a checkbox).
    Changed,
    /// The editor took the keys.
    Focused,
    /// The keys left it.
    Blurred,
    /// Enter in the title.
    Submit,
    /// Esc (with no prompt open).
    Escape,
    /// Tab (or Shift+Tab, `back`) wasn't the editor's.
    Tab { back: bool },
    /// Cmd+S.
    Save,
}

/// The last layout painted, for the pointer, the input method and the
/// vertical moves.
pub struct Painted {
    pub bounds: Bounds<Pixels>,
    pub layout: Rc<DocLayout>,
}

/// The link prompt (⌘K): a URL field under the selection.
struct LinkPrompt {
    input: Entity<InputState>,
    _sub: Subscription,
}

pub struct DocEditor {
    pub model: Editor,
    focus: FocusHandle,
    field: Field,
    placeholder: SharedString,
    label: SharedString,
    painted: Option<Painted>,
    /// The x a run of vertical moves keeps to.
    goal_x: Option<Pixels>,
    dragging: bool,
    caret_on: bool,
    focused: bool,
    /// Scroll the caret into view at the next paint.
    reveal: bool,
    /// The scroll the editor sits in.
    scroll: Option<ScrollHandle>,
    link: Option<LinkPrompt>,
    _subs: Vec<Subscription>,
    _blink: Task<()>,
}

impl EventEmitter<DocEvent> for DocEditor {}

impl Focusable for DocEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl DocEditor {
    /// An editor on `text`: the title's one plain line, or the Markdown
    /// body. `scroll` is the scroll it sits in (the caret is kept in view).
    pub fn new(
        text: &str,
        field: Field,
        placeholder: impl Into<SharedString>,
        label: impl Into<SharedString>,
        scroll: Option<ScrollHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let subs = vec![
            cx.on_focus_in(&focus, window, |this, _, cx| {
                if !this.focused {
                    this.focused = true;
                    this.caret_on = true;
                    cx.emit(DocEvent::Focused);
                    cx.notify();
                }
            }),
            cx.on_focus_out(&focus, window, |this, _, _, cx| {
                this.focused = false;
                this.link = None;
                this.dragging = false;
                cx.emit(DocEvent::Blurred);
                cx.notify();
            }),
        ];
        let blink = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(BLINK).await;
            let alive = this.update(cx, |e, cx| {
                if e.focused {
                    e.caret_on = !e.caret_on;
                    cx.notify();
                }
            });
            if alive.is_err() {
                break;
            }
        });
        DocEditor {
            model: if field == Field::Title {
                Editor::plain(text)
            } else {
                Editor::markdown(text)
            },
            focus,
            field,
            placeholder: placeholder.into(),
            label: label.into(),
            painted: None,
            goal_x: None,
            dragging: false,
            caret_on: true,
            focused: false,
            reveal: false,
            scroll,
            link: None,
            _subs: subs,
            _blink: blink,
        }
    }

    pub fn text(&self) -> &str {
        self.model.text()
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// Take the keys with the caret at the end (the Edit command) or the
    /// start.
    pub fn focus_at(&mut self, end: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.model.move_to_edge(end, false);
        self.reveal = true;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The text changed from outside (a read, the draft store): put it in
    /// as one splice, the caret and undo kept. Not while the input method
    /// is composing.
    pub fn set_from_outside(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.model.is_composing() {
            return;
        }
        if self.model.replace_from_outside(text) {
            cx.notify();
        }
    }

    /// Run an edit: the change is announced, the caret shown and kept in
    /// view.
    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Editor)) {
        let before = self.model.revision();
        f(&mut self.model);
        if self.model.revision() != before {
            cx.emit(DocEvent::Changed);
        }
        self.caret_on = true;
        self.reveal = true;
        cx.notify();
    }

    /// A caret move (the goal x is kept only by vertical moves).
    fn moved(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Editor)) {
        self.goal_x = None;
        self.edit(cx, f);
    }

    // ---- keys -----------------------------------------------------------------

    fn key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // An input method mid-composition owns the keys.
        if self.model.is_composing() {
            return;
        }
        let k = &ev.keystroke;
        let m = &k.modifiers;
        let (cmd, alt, ctrl, shift) = (m.platform, m.alt, m.control, m.shift);
        let body = self.field == Field::Body;
        let handled = match k.key.as_str() {
            "escape" if !cmd && !alt && !ctrl => {
                cx.emit(DocEvent::Escape);
                true
            }
            "enter" if cmd && body => {
                self.edit(cx, |e| {
                    e.toggle_task_at_caret();
                });
                true
            }
            "enter" if !body => {
                cx.emit(DocEvent::Submit);
                true
            }
            "enter" if shift => {
                self.edit(cx, |e| e.line_break());
                true
            }
            "enter" => {
                self.edit(cx, |e| {
                    e.enter();
                });
                true
            }
            "tab" => {
                let mut ours = false;
                if body {
                    let before = self.model.revision();
                    ours = if shift {
                        self.model.outdent()
                    } else {
                        self.model.indent()
                    };
                    if self.model.revision() != before {
                        cx.emit(DocEvent::Changed);
                        cx.notify();
                    }
                }
                if !ours {
                    cx.emit(DocEvent::Tab { back: shift });
                }
                true
            }
            "backspace" if cmd => {
                let start = self.line_edge(false);
                self.edit(cx, |e| e.delete_back_to(start));
                true
            }
            "backspace" => {
                self.edit(cx, |e| e.backspace(alt));
                true
            }
            "delete" => {
                self.edit(cx, |e| e.delete(alt));
                true
            }
            "left" if cmd => {
                let to = self.line_edge(false);
                self.moved(cx, |e| e.move_to(to, shift));
                true
            }
            "right" if cmd => {
                let to = self.line_edge(true);
                self.moved(cx, |e| e.move_to(to, shift));
                true
            }
            "left" => {
                self.moved(cx, |e| e.move_left(alt, shift));
                true
            }
            "right" => {
                self.moved(cx, |e| e.move_right(alt, shift));
                true
            }
            "up" if cmd => {
                self.moved(cx, |e| e.move_to_edge(false, shift));
                true
            }
            "down" if cmd => {
                self.moved(cx, |e| e.move_to_edge(true, shift));
                true
            }
            "up" => {
                self.vertical(-1, shift, cx);
                true
            }
            "down" => {
                self.vertical(1, shift, cx);
                true
            }
            "pageup" => {
                self.vertical(-15, shift, cx);
                true
            }
            "pagedown" => {
                self.vertical(15, shift, cx);
                true
            }
            "home" => {
                let to = self.line_edge(false);
                self.moved(cx, |e| e.move_to(to, shift));
                true
            }
            "end" => {
                let to = self.line_edge(true);
                self.moved(cx, |e| e.move_to(to, shift));
                true
            }
            "a" if ctrl => {
                let to = self.line_edge(false);
                self.moved(cx, |e| e.move_to(to, shift));
                true
            }
            "e" if ctrl => {
                let to = self.line_edge(true);
                self.moved(cx, |e| e.move_to(to, shift));
                true
            }
            "z" if cmd && shift => {
                self.edit(cx, |e| {
                    e.redo();
                });
                true
            }
            "z" if cmd => {
                self.edit(cx, |e| {
                    e.undo();
                });
                true
            }
            "a" if cmd => {
                self.moved(cx, |e| e.select_all());
                true
            }
            "c" if cmd => {
                self.copy(cx);
                true
            }
            "x" if cmd => {
                self.cut(cx);
                true
            }
            "v" if cmd => {
                self.paste(cx);
                true
            }
            "s" if cmd => {
                cx.emit(DocEvent::Save);
                true
            }
            key if cmd && body => self.format_key(key, alt, shift, window, cx),
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    /// The formatting chords.
    fn format_key(&mut self, key: &str, alt: bool, shift: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match (key, alt, shift) {
            ("b", false, false) => self.edit(cx, |e| e.toggle_mark(MarkKind::Bold)),
            ("i", false, false) => self.edit(cx, |e| e.toggle_mark(MarkKind::Italic)),
            ("e", false, false) => self.edit(cx, |e| e.toggle_mark(MarkKind::Code)),
            ("x", false, true) => self.edit(cx, |e| e.toggle_mark(MarkKind::Strike)),
            ("k", false, false) => self.open_link(window, cx),
            ("1", true, false) => self.edit(cx, |e| e.set_block(BlockKind::Heading(1))),
            ("2", true, false) => self.edit(cx, |e| e.set_block(BlockKind::Heading(2))),
            ("3", true, false) => self.edit(cx, |e| e.set_block(BlockKind::Heading(3))),
            ("0", true, false) => self.edit(cx, |e| e.set_block(BlockKind::Paragraph)),
            ("c", true, false) => self.edit(cx, |e| e.set_block(BlockKind::Code)),
            ("7", false, true) => self.edit(cx, |e| e.set_block(BlockKind::Ordered)),
            ("8", false, true) => self.edit(cx, |e| e.set_block(BlockKind::Bullet)),
            ("9", false, true) => self.edit(cx, |e| e.set_block(BlockKind::Task)),
            _ => return false,
        }
        true
    }

    /// Where the caret's visual line starts (or ends).
    fn line_edge(&self, end: bool) -> usize {
        let doc = self.model.doc();
        let (row, d) = doc.locate(self.model.head());
        let r = &doc.rows[row];
        let Some(p) = &self.painted else {
            return r.source_of(if end { r.display.len() } else { 0 });
        };
        let visuals = p.layout.visuals();
        let v = p.layout.visual_of(&visuals, row, d);
        let last = !visuals.iter().any(|o| o.row == v.row && o.top > v.top);
        let to = if !end {
            v.start
        } else if last || v.end == v.start {
            v.end
        } else {
            shelbi_app::markdown_edit::prev_boundary(&r.display, v.end)
        };
        r.source_of(to)
    }

    /// Up or down `lines` visual lines, keeping to the goal x.
    fn vertical(&mut self, lines: i32, extend: bool, cx: &mut Context<Self>) {
        let Some(p) = &self.painted else {
            return;
        };
        let doc = self.model.doc();
        let visuals = p.layout.visuals();
        let (row, d) = doc.locate(self.model.head());
        let v = p.layout.visual_of(&visuals, row, d);
        let x = self.goal_x.unwrap_or_else(|| p.layout.x_at(&v, d));
        let at = visuals
            .iter()
            .position(|o| o.row == v.row && o.line == v.line && o.sub == v.sub)
            .unwrap_or(0) as i32;
        let target = at + lines;
        let to = if target < 0 {
            None
        } else if let Some(t) = visuals.get(target as usize) {
            let r = &doc.rows[t.row];
            Some(r.source_of(p.layout.offset_at_x(t, x, &r.display)))
        } else {
            None
        };
        let goal = Some(x);
        self.edit(cx, |e| match to {
            Some(src) => e.move_to(src, extend),
            None => e.move_to_edge(lines > 0, extend),
        });
        self.goal_x = goal;
    }

    // ---- pointer ---------------------------------------------------------------

    fn mouse_down(&mut self, e: &MouseDownEvent, on_check: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.painted else {
            return;
        };
        let rel = e.position - p.bounds.origin;
        if on_check {
            if let Some(row) = p.layout.check_at(rel) {
                self.edit(cx, |m| {
                    m.toggle_task(row);
                });
                return;
            }
        }
        let src = p.layout.hit(self.model.doc(), rel);
        if e.modifiers.platform {
            if let Some(url) = self.model.link_at(src) {
                cx.open_url(&url);
                return;
            }
        }
        window.focus(&self.focus, cx);
        self.goal_x = None;
        self.dragging = true;
        match e.click_count {
            2 => {
                let doc = self.model.doc();
                let (row, d) = doc.locate(src);
                let r = &doc.rows[row];
                let end = word_right(&r.display, d);
                let start = word_left(&r.display, end);
                let (a, b) = (r.source_of(start), r.source_of(end));
                self.model.select(a, b);
            }
            n if n >= 3 => {
                let doc = self.model.doc();
                let r = &doc.rows[doc.row_of(src)];
                let (a, b) = (r.source_of(0), r.source_of(r.display.len()));
                self.model.select(a, b);
            }
            _ if e.modifiers.shift => self.model.move_to(src, true),
            _ => self.model.select(src, src),
        }
        self.caret_on = true;
        cx.notify();
    }

    fn mouse_drag(&mut self, pos: Point<Pixels>, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.dragging {
            return;
        }
        let Some(p) = &self.painted else {
            return;
        };
        let src = p.layout.hit(self.model.doc(), pos - p.bounds.origin);
        if src != self.model.head() {
            self.model.move_to(src, true);
            self.reveal = true;
            cx.notify();
        }
    }

    fn mouse_up(&mut self, cx: &mut Context<Self>) {
        if self.dragging {
            self.dragging = false;
            cx.notify();
        }
    }

    /// Keep the caret inside the scroll it sits in (after a keyboard move
    /// or an edit).
    fn reveal_caret(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.reveal) || !self.focused {
            return;
        }
        let (Some(scroll), Some(p)) = (&self.scroll, &self.painted) else {
            return;
        };
        let visuals = p.layout.visuals();
        let (row, d) = self.model.doc().locate(self.model.head());
        let c = p.layout.caret(&visuals, row, d);
        let top = p.bounds.origin.y + c.origin.y;
        let bottom = top + c.size.height;
        let view = scroll.bounds();
        if view.size.height <= px(0.0) {
            return;
        }
        let margin = px(24.0);
        let mut off = scroll.offset();
        if top < view.top() + margin {
            off.y += view.top() + margin - top;
        } else if bottom > view.bottom() - margin {
            off.y -= bottom - (view.bottom() - margin);
        }
        off.y = off.y.min(px(0.0)).max(-scroll.max_offset().y);
        if off != scroll.offset() {
            scroll.set_offset(off);
            window.refresh();
        }
    }

    // ---- clipboard ---------------------------------------------------------------

    fn copy(&mut self, cx: &mut Context<Self>) {
        if self.model.has_selection() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.model.selected_markdown()));
        }
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        if self.model.has_selection() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.model.selected_markdown()));
            self.edit(cx, |e| e.delete_selection());
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
            self.edit(cx, |e| e.insert(&text));
        }
    }

    // ---- links -------------------------------------------------------------------

    /// ⌘K: the link prompt for the link at the caret, or the selection.
    fn open_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.model.link_target() else {
            return;
        };
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Paste or type a link")
                .default_value(target.url.clone())
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                let url = input.read(cx).value().to_string();
                this.link = None;
                this.edit(cx, |e| {
                    e.set_link(&url);
                });
                window.focus(&this.focus, cx);
            }
        });
        self.link = Some(LinkPrompt { input, _sub: sub });
        cx.notify();
    }

    fn close_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.link = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The caret's (or the selection start's) box in window coordinates.
    fn anchor_box(&self) -> Option<Bounds<Pixels>> {
        let p = self.painted.as_ref()?;
        let visuals = p.layout.visuals();
        let (row, d) = self.model.doc().locate(self.model.selection().start);
        let c = p.layout.caret(&visuals, row, d);
        Some(Bounds {
            origin: p.bounds.origin + c.origin,
            size: c.size,
        })
    }

    // ---- the floating controls -------------------------------------------------------

    /// The format toolbar over a selection in the body.
    fn toolbar(&self, focused: bool, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.field != Field::Body || !focused || !self.model.has_selection() || self.dragging || self.link.is_some()
        {
            return None;
        }
        let at = self.anchor_box()?;
        let s = self.model.style_at_caret();
        let row = self
            .model
            .doc()
            .rows
            .get(self.model.caret_row())
            .map(|r| r.kind.clone());
        let heading = matches!(row, Some(shelbi_app::markdown_edit::RowKind::Heading(_)));
        let task = self
            .model
            .doc()
            .rows
            .get(self.model.caret_row())
            .and_then(|r| r.task())
            .is_some();
        let button =
            |id: &'static str, label: &'static str, tip: &'static str, on: bool, ty: Type, f: fn(&mut Editor)| {
                div()
                    .id(id)
                    .debug_selector(move || id.into())
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(26.0))
                    .min_w(px(26.0))
                    .px(px(6.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .type_style(ty)
                    .text_color(if on { theme.text_loud } else { theme.foreground })
                    .when(on, |d| d.bg(theme.background_active))
                    .hover(|d| d.bg(theme.background_hover))
                    .role(Role::Button)
                    .aria_label(tip)
                    .tooltip(style::tooltip(tip))
                    .child(label)
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.edit(cx, f);
                    }))
            };
        let bar = div()
            .debug_selector(|| "doc-toolbar".into())
            .occlude()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .p(px(3.0))
            .border_1()
            .border_color(theme.divider)
            .child(button(
                "fmt-bold",
                "B",
                "Bold (⌘B)",
                s.bold,
                Type::BaseSemibold,
                |e| e.toggle_mark(MarkKind::Bold),
            ))
            .child(button("fmt-italic", "I", "Italic (⌘I)", s.italic, Type::Base, |e| {
                e.toggle_mark(MarkKind::Italic)
            }))
            .child(button(
                "fmt-strike",
                "S",
                "Strikethrough (⇧⌘X)",
                s.strike,
                Type::Base,
                |e| e.toggle_mark(MarkKind::Strike),
            ))
            .child(button("fmt-code", "</>", "Code (⌘E)", s.code, Type::XSmall, |e| {
                e.toggle_mark(MarkKind::Code)
            }))
            .child(
                div()
                    .id("fmt-link")
                    .debug_selector(|| "fmt-link".into())
                    .flex()
                    .items_center()
                    .h(px(26.0))
                    .px(px(6.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .type_style(Type::Small)
                    .text_color(if s.link { theme.text_loud } else { theme.foreground })
                    .hover(|d| d.bg(theme.background_hover))
                    .role(Role::Button)
                    .aria_label("Link (⌘K)")
                    .tooltip(style::tooltip("Link (⌘K)"))
                    .child("Link")
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_link(window, cx))),
            )
            .child(div().w(px(1.0)).h(px(16.0)).mx(px(2.0)).bg(theme.divider))
            .child(button(
                "fmt-heading",
                "H",
                "Heading (⌥⌘2)",
                heading,
                Type::BaseSemibold,
                |e| e.set_block(BlockKind::Heading(2)),
            ))
            .child(button(
                "fmt-list",
                "•",
                "Bulleted list (⇧⌘8)",
                false,
                Type::Base,
                |e| e.set_block(BlockKind::Bullet),
            ))
            .child(button(
                "fmt-task",
                "☐",
                "Checklist (⇧⌘9)",
                task,
                Type::Base,
                |e| e.set_block(BlockKind::Task),
            ));
        let bar = style::popover(bar, theme);
        Some(
            deferred(
                anchored()
                    .position(point(at.origin.x - px(8.0), at.origin.y - px(40.0)))
                    .snap_to_window()
                    .child(bar),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    /// The link prompt under the selection.
    fn link_prompt(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let prompt = self.link.as_ref()?;
        let at = self.anchor_box()?;
        let panel = div()
            .debug_selector(|| "doc-link-prompt".into())
            .occlude()
            .w(px(320.0))
            .p(px(8.0))
            .border_1()
            .border_color(theme.divider)
            .key_context("DocLinkPrompt")
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.close_link(window, cx)))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    this.close_link(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.link = None;
                cx.notify();
            }))
            .child(Input::new(&prompt.input));
        let panel = style::popover(panel, theme);
        Some(
            deferred(
                anchored()
                    .position(point(at.origin.x - px(8.0), at.origin.y + at.size.height + px(6.0)))
                    .snap_to_window()
                    .child(panel),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
impl DocEditor {
    /// Where row `row`'s checkbox is painted, in window coordinates.
    pub fn check_point(&self, row: usize) -> Option<Point<Pixels>> {
        let p = self.painted.as_ref()?;
        let (_, g) = p.layout.rows.get(row)?.gutter.as_ref()?;
        Some(p.bounds.origin + layout::check_box(*g).center())
    }

    /// Where source offset `src` is painted (the caret's middle), in window
    /// coordinates.
    pub fn point_of(&self, src: usize) -> Option<Point<Pixels>> {
        let p = self.painted.as_ref()?;
        let visuals = p.layout.visuals();
        let (row, d) = self.model.doc().locate(src);
        let c = p.layout.caret(&visuals, row, d);
        Some(p.bounds.origin + c.center())
    }
}

impl Render for DocEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().opaque();
        let toolbar = self.toolbar(self.focus.is_focused(window), &theme, cx);
        let prompt = self.link_prompt(&theme, cx);
        let selector: &'static str = if self.field == Field::Title {
            "pane-title-editor"
        } else {
            "pane-body-editor"
        };
        div()
            .id(selector)
            .debug_selector(move || selector.into())
            .track_focus(&self.focus)
            .key_context("DocEditor")
            .role(if self.field == Field::Title {
                Role::TextInput
            } else {
                Role::MultilineTextInput
            })
            .aria_label(self.label.clone())
            .relative()
            .w_full()
            .min_w_0()
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(|this, _: &menus::Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &menus::Cut, _, cx| this.cut(cx)))
            .on_action(cx.listener(|this, _: &menus::Paste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &menus::SelectAll, _, cx| this.moved(cx, |e| e.select_all())))
            .child(DocElement::new(cx.entity()))
            .children(toolbar)
            .children(prompt)
    }
}

impl EntityInputHandler for DocEditor {
    fn text_for_range(
        &mut self,
        range: std::ops::Range<usize>,
        adjusted: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let (a, b) = (self.model.from_utf16(range.start), self.model.from_utf16(range.end));
        let b = b.max(a);
        *adjusted = Some(self.model.to_utf16(a)..self.model.to_utf16(b));
        Some(self.model.text()[a..b].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let sel = self.model.selection();
        Some(UTF16Selection {
            range: self.model.to_utf16(sel.start)..self.model.to_utf16(sel.end),
            reversed: self.model.head() < self.model.anchor(),
        })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<std::ops::Range<usize>> {
        self.model
            .marked()
            .map(|m| self.model.to_utf16(m.start)..self.model.to_utf16(m.end))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |e| e.unmark());
        cx.emit(DocEvent::Changed);
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.map(|r| self.model.from_utf16(r.start)..self.model.from_utf16(r.end));
        let composing = self.model.is_composing();
        self.goal_x = None;
        self.edit(cx, |e| e.commit(range, text));
        if composing {
            cx.emit(DocEvent::Changed);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<std::ops::Range<usize>>,
        new_text: &str,
        new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.map(|r| self.model.from_utf16(r.start)..self.model.from_utf16(r.end));
        // The selection inside the new text, from UTF-16 to bytes.
        let to_bytes = |units: usize| {
            let mut seen = 0;
            for (i, c) in new_text.char_indices() {
                if seen >= units {
                    return i;
                }
                seen += c.len_utf16();
            }
            new_text.len()
        };
        let selected = new_selected_range.map(|r| to_bytes(r.start)..to_bytes(r.end));
        self.model.set_marked(range, new_text, selected);
        self.caret_on = true;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let p = self.painted.as_ref()?;
        let src = self.model.from_utf16(range_utf16.start);
        let visuals = p.layout.visuals();
        let (row, d) = self.model.doc().locate(src);
        let c = p.layout.caret(&visuals, row, d);
        Some(Bounds {
            origin: element_bounds.origin + c.origin,
            size: gpui::size(px(2.0), c.size.height),
        })
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let p = self.painted.as_ref()?;
        let src = p.layout.hit(self.model.doc(), point - p.bounds.origin);
        Some(self.model.to_utf16(src))
    }
}
