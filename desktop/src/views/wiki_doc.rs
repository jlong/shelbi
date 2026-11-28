//! The Wiki's open document (`wiki-desktop-doc`): the main area's toolbar,
//! the in-place editor, and its saves through the hub.
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────┐
//! │ Plans › OpenCode integration      Updated by an agent · 2m ago   ⋯  │
//! │ ⚠ Changed elsewhere while you were editing.     Keep mine   Reload  │
//! │ ──────────────────────────────────────────────────────────────────── │
//! │  # OpenCode integration                                              │
//! │  ## Context                                                          │
//! │  OpenCode is a terminal coding agent …▏                              │
//! └──────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! The document is the task pane's editor ([`DocEditor`], over
//! `shelbi_app::markdown_edit`): no edit mode, the caret goes where you
//! click. Typing saves through the hub's wiki service (a whole-document
//! [`WikiRequest::Write`]) once it pauses for
//! [`shelbi_app::wiki_doc::SAVE_AFTER`], at once on ⌘S
//! or when the editor loses the keys; the hub commits it on its own clock.
//! [`DocSync`] keeps the hub's side: when the hub announces a change to the
//! open document it is read again, our own save's echo changes nothing, a
//! clean document takes the change in place, and a dirty one shows the
//! changed-elsewhere bar (Reload / Keep mine), saving nothing until it's
//! answered.
//!
//! The toolbar: the breadcrumb (folders › the document's title), "Updated
//! by you / an agent / <git author> · 2m ago" (the document's last commit,
//! whose `Shelbi-By` trailer says who, or a change seen since), and ⋯ with
//! Open in Editor (this machine's file, in the user's editor), Copy Path
//! (the wiki path `shelbi wiki` takes) and Show History (its commits).

use std::time::Instant;

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, ClipboardItem, Context, Entity, MouseButton, MouseDownEvent,
    ScrollHandle, SharedString, Subscription, Window,
};
use shelbi_app::wiki::{Author, WikiCommit, WikiReply, WikiRequest};
use shelbi_app::wiki_doc::{DocSync, Read, Updated, Who};
use shelbi_app::wiki_tree_edit as edit;

use super::WikiView;
use crate::views::doc_editor::layout::Field as DocField;
use crate::views::doc_editor::{DocEditor, DocEvent};
use crate::views::{external_edit, sidebar, tab_bar};
use crate::assets::{heroicon, icons};
use crate::elements::placed::placed;
use crate::elements::terminal::settings::TerminalSettings;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The document's body size: the terminal font size
/// ([`TerminalSettings::font_size`]), so the Wiki reads at the size of the
/// panes beside it. Headings and code scale from it
/// ([`DocEditor::set_body_size`]); the toolbar and bars keep the UI's sizes.
pub fn body_size(cx: &App) -> f32 {
    TerminalSettings::current(cx).font_size
}

/// How many commits Show History lists.
pub const HISTORY_LIMIT: usize = 30;
/// The document menu's width.
const MENU_W: f32 = 220.0;
/// The history list's width.
const HISTORY_W: f32 = 360.0;
/// The document column's widest at the UI's base size ([`Type::Base`]).
const MAX_W: f32 = 760.0;
/// The room either side of the column when the main area is narrower than
/// it.
const SIDE_PAD: f32 = 32.0;

/// The document column's widest with the body at `size` points:
/// [`MAX_W`] scaled as the text is, so a line holds about as many
/// characters at any font size.
pub fn column_width(size: f32) -> f32 {
    MAX_W * (size / Type::Base.size()).max(0.1)
}

/// The open document's editor and the hub's side of it.
pub(super) struct DocPane {
    pub(super) path: String,
    pub(super) editor: Entity<DocEditor>,
    pub(super) sync: DocSync,
    scroll: ScrollHandle,
    _sub: Subscription,
}

/// The toolbar's ⋯: its actions, or the history they opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocMenu {
    Actions,
    /// Show History: the document's commits, newest first, once read.
    History(Option<Result<Vec<WikiCommit>, String>>),
}

/// What the document menu offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocAction {
    OpenInEditor,
    CopyPath,
    ShowHistory,
}

impl DocAction {
    pub const ALL: [DocAction; 3] = [DocAction::OpenInEditor, DocAction::CopyPath, DocAction::ShowHistory];

    pub fn label(self) -> &'static str {
        match self {
            DocAction::OpenInEditor => "Open in Editor",
            DocAction::CopyPath => "Copy Path",
            DocAction::ShowHistory => "Show History",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            DocAction::OpenInEditor => "open-in-editor",
            DocAction::CopyPath => "copy-path",
            DocAction::ShowHistory => "show-history",
        }
    }
}

impl WikiView {
    // --- the document's state ----------------------------------------------

    /// Nothing open any more (it was deleted, or went away elsewhere).
    pub(super) fn close_doc(&mut self) {
        self.open = None;
        self.doc = None;
        self.pane = None;
        self.updated = None;
        self.doc_menu = None;
        self.save_timer = None;
    }

    /// Open `path` from outside the tree (the palette's wiki result, a
    /// `wiki:` link): selected and shown now when the tree is in, else once
    /// it is read.
    pub fn open_at(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tree.is_loaded() {
            if self.open.as_deref() != Some(path) {
                self.flush_save(window, cx);
                self.close_doc();
                self.open = Some(path.to_string());
            }
            return;
        }
        if self.tree.node(path).is_none() {
            self.notice = Some(format!("There's no {path} in this wiki."));
            cx.notify();
            return;
        }
        self.reveal(path, window, cx);
    }

    /// A read of the open document landed: the editor's first text, or the
    /// hub's text after a change it announced.
    pub(super) fn take_doc(&mut self, path: String, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.doc = Some(Ok(()));
        if let Some(pane) = self.pane.as_mut().filter(|p| p.path == path) {
            let ours = pane.editor.read(cx).text().to_string();
            match pane.sync.read(text, &ours) {
                Read::Same | Read::Elsewhere => {}
                Read::Reload(theirs) => pane.editor.update(cx, |e, cx| e.set_from_outside(&theirs, cx)),
            }
            cx.notify();
            return;
        }
        let scroll = ScrollHandle::new();
        let editor = cx.new(|cx| {
            DocEditor::new(&text, DocField::Body, "Write something…", "Wiki document", Some(scroll.clone()), window, cx)
        });
        let size = body_size(cx);
        editor.update(cx, |e, cx| e.set_body_size(size, cx));
        let sub = cx.subscribe_in(&editor, window, |this, _, ev: &DocEvent, window, cx| this.doc_event(ev, window, cx));
        self.pane = Some(DocPane {
            sync: DocSync::new(path.clone(), text),
            path,
            editor,
            scroll,
            _sub: sub,
        });
        self.read_updated(window, cx);
        cx.notify();
    }

    /// Set the open document's text at [`body_size`] again (the terminal
    /// font size changed).
    pub(super) fn apply_body_size(&mut self, cx: &mut Context<Self>) {
        if let Some(pane) = &self.pane {
            let size = body_size(cx);
            pane.editor.update(cx, |e, cx| e.set_body_size(size, cx));
        }
    }

    /// What the editor said.
    fn doc_event(&mut self, ev: &DocEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            DocEvent::Changed => {
                if let Some(pane) = self.pane.as_mut() {
                    let text = pane.editor.read(cx).text().to_string();
                    pane.sync.edited(&text, Instant::now());
                }
                self.schedule_save(window, cx);
            }
            // ⌘S, and the keys leaving the document, save what's typed now.
            DocEvent::Save | DocEvent::Blurred => self.flush_save(window, cx),
            // Esc hands the keys back to the tree, saving what's typed.
            DocEvent::Escape => {
                self.flush_save(window, cx);
                self.focus.focus(window, cx);
            }
            DocEvent::Focused | DocEvent::Submit | DocEvent::Tab { .. } => {}
        }
        cx.notify();
    }

    /// Wake when the waiting save is due. Each edit replaces the timer, so
    /// the one that fires is the last edit's: the pause ran out.
    fn schedule_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(due) = self.pane.as_ref().and_then(|p| p.sync.due_at()) else {
            self.save_timer = None;
            return;
        };
        let wait = due.saturating_duration_since(Instant::now());
        self.save_timer = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update_in(cx, |this, window, cx| this.flush_save(window, cx)).ok();
        }));
    }

    /// Save whatever is typed now (unless a save is on its way, or the
    /// changed-elsewhere bar waits).
    pub fn flush_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.pane.as_mut() else {
            return;
        };
        let text = pane.editor.read(cx).text().to_string();
        let Some(content) = pane.sync.take_save(&text, Instant::now(), true) else {
            return;
        };
        self.save_timer = None;
        let path = pane.path.clone();
        let retitled = title_of(&content) != self.tree.node(&path).map(|n| n.title.as_str());
        let req = WikiRequest::Write {
            path: path.clone(),
            content,
            section: None,
            by: Author::Person,
        };
        self.request(req, window, cx, move |this, reply, window, cx| {
            let Some(pane) = this.pane.as_mut().filter(|p| p.path == path) else {
                return;
            };
            let ok = matches!(reply, WikiReply::Done);
            pane.sync.saved(match reply {
                WikiReply::Done => Ok(()),
                WikiReply::Failed(f) => Err(f.message),
                other => Err(format!("the hub answered {other:?}")),
            });
            if ok {
                this.updated = Some(Updated {
                    who: Who::You,
                    at: chrono::Utc::now(),
                });
                // A new title is the tree's label.
                if retitled {
                    this.read_tree(window, cx);
                }
            }
            // Typing went on while it was on its way.
            this.schedule_save(window, cx);
        });
    }

    /// The bar's Reload: the hub's text, ours dropped.
    pub fn reload_theirs(&mut self, cx: &mut Context<Self>) {
        if let Some(pane) = self.pane.as_mut() {
            if let Some(theirs) = pane.sync.take_theirs() {
                pane.editor.update(cx, |e, cx| e.set_from_outside(&theirs, cx));
            }
        }
        self.save_timer = None;
        cx.notify();
    }

    /// The bar's Keep mine: ours saves over theirs.
    pub fn keep_mine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.pane.as_mut() else {
            return;
        };
        if pane.sync.keep_mine(Instant::now()) {
            self.flush_save(window, cx);
        }
        cx.notify();
    }

    /// Who last changed the open document: its last commit, unless a
    /// change seen since is newer.
    pub(super) fn read_updated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.open.clone() else {
            return;
        };
        let req = WikiRequest::Log {
            path: Some(path.clone()),
            limit: 1,
        };
        self.request(req, window, cx, move |this, reply, _, _| {
            if this.open.as_deref() != Some(path.as_str()) {
                return;
            }
            if let WikiReply::Log(commits) = reply {
                let committed = commits.first().and_then(Updated::of_commit);
                this.updated = match (this.updated.take(), committed) {
                    (Some(seen), Some(c)) => Some(if seen.at > c.at { seen } else { c }),
                    (seen, c) => seen.or(c),
                };
            }
        });
    }

    // --- the document menu ---------------------------------------------------

    pub fn open_doc_menu(&mut self, at: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        self.doc_menu_anchor.set(Some(gpui::Bounds::new(at, gpui::Size::default())));
        self.doc_menu = Some(DocMenu::Actions);
        cx.notify();
    }

    pub fn close_doc_menu(&mut self, cx: &mut Context<Self>) {
        self.doc_menu = None;
        cx.notify();
    }

    #[cfg(test)]
    pub fn doc_menu(&self) -> Option<&DocMenu> {
        self.doc_menu.as_ref()
    }

    /// Why `action` can't run, or `None` when it can.
    pub fn doc_action_refusal(&self, action: DocAction) -> Option<&'static str> {
        match action {
            DocAction::OpenInEditor if !self.local => Some("The wiki is on another machine"),
            _ => None,
        }
    }

    pub fn doc_action(&mut self, action: DocAction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.open.clone() else {
            self.doc_menu = None;
            return;
        };
        match action {
            DocAction::OpenInEditor => {
                self.doc_menu = None;
                // What's typed goes to the file first; the editor's saves
                // come back through the hub's watcher.
                self.flush_save(window, cx);
                if let Some(abs) = self.local_path(&path) {
                    if let Err(e) = external_edit::open_file(&abs) {
                        self.notice = Some(format!("Couldn't open {path}: {e}"));
                    }
                }
            }
            DocAction::CopyPath => {
                self.doc_menu = None;
                cx.write_to_clipboard(ClipboardItem::new_string(path));
            }
            DocAction::ShowHistory => {
                self.doc_menu = Some(DocMenu::History(None));
                let req = WikiRequest::Log {
                    path: Some(path.clone()),
                    limit: HISTORY_LIMIT,
                };
                self.request(req, window, cx, move |this, reply, _, _| {
                    if this.open.as_deref() != Some(path.as_str()) || !matches!(this.doc_menu, Some(DocMenu::History(_))) {
                        return;
                    }
                    this.doc_menu = Some(DocMenu::History(Some(match reply {
                        WikiReply::Log(commits) => Ok(commits),
                        WikiReply::Failed(f) => Err(f.message),
                        other => Err(format!("the hub answered {other:?}")),
                    })));
                });
            }
        }
        cx.notify();
    }

    /// Type `text` at the end of the open document, as the visual check's
    /// changed-elsewhere scene needs (no keys reach an offscreen window's
    /// editor).
    #[cfg(feature = "visual-check")]
    pub fn edit_for_visual(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.pane.as_ref() else {
            return;
        };
        let editor = pane.editor.clone();
        editor.update(cx, |e, cx| {
            let mut next = e.text().trim_end().to_string();
            next.push_str(text);
            next.push('\n');
            e.set_from_outside(&next, cx);
            e.focus_at(true, window, cx);
        });
        let next = editor.read(cx).text().to_string();
        if let Some(pane) = self.pane.as_mut() {
            pane.sync.edited(&next, Instant::now());
        }
        cx.notify();
    }

    // --- test reads ----------------------------------------------------------

    #[cfg(test)]
    pub fn doc_editor(&self) -> Option<Entity<DocEditor>> {
        self.pane.as_ref().map(|p| p.editor.clone())
    }

    #[cfg(test)]
    pub fn changed_elsewhere(&self) -> bool {
        self.pane.as_ref().is_some_and(|p| p.sync.changed_elsewhere())
    }

    /// The toolbar's "Updated by …" line at `now`.
    pub fn updated_line(&self, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
        self.updated.as_ref().map(|u| u.line(now))
    }

    // --- rendering -------------------------------------------------------------

    /// The open document: its toolbar, the changed-elsewhere bar, the
    /// editor.
    pub(super) fn render_doc(&self, path: String, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let body = match (&self.doc, &self.pane) {
            (Some(Ok(())), Some(pane)) if pane.path == path => {
                // The room under the text: a click there puts the caret at
                // the end, as in any editor.
                let editor = pane.editor.clone();
                let rest = div()
                    .id("wiki-doc-rest")
                    .flex_1()
                    .min_h(px(160.0))
                    .cursor_text()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _: &MouseDownEvent, window, cx| {
                            editor.update(cx, |e, cx| e.focus_at(true, window, cx));
                        }),
                    );
                div()
                    .id("wiki-doc-body")
                    .debug_selector(|| "wiki-doc-body".into())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&pane.scroll)
                    // The column sits in the middle; the scroll container
                    // keeps the main area's width, so the scrollbar stays
                    // at its right edge.
                    .items_center()
                    .px(px(SIDE_PAD))
                    .pt(px(24.0))
                    .child(
                        div()
                            .debug_selector(|| "wiki-doc-column".into())
                            .flex()
                            .flex_col()
                            .flex_1()
                            .w_full()
                            .max_w(px(column_width(body_size(cx))))
                            .child(div().w_full().min_w_0().child(pane.editor.clone()))
                            .child(rest),
                    )
                    .into_any_element()
            }
            (Some(Err(e)), _) => super::note("wiki-doc-error", "Couldn't read this document", Some(e.clone()), theme)
                .into_any_element(),
            _ => super::note("wiki-doc-loading", "Loading…", None, theme).into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(self.render_toolbar(&path, theme, cx))
            .children(self.render_held_bar(&path, theme, cx))
            .children(self.render_elsewhere(theme, cx))
            .child(body)
            .children(self.render_doc_menu(cx))
            .into_any_element()
    }

    /// The breadcrumb, who last changed it, and ⋯.
    fn render_toolbar(&self, path: &str, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let crumbs = edit::breadcrumb(self.tree.nodes(), path);
        let last = crumbs.len().saturating_sub(1);
        let crumb_row = div()
            .debug_selector(|| "wiki-breadcrumb".into())
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(6.0))
            .overflow_hidden()
            .children(crumbs.into_iter().enumerate().flat_map(|(i, c)| {
                let sep = (i > 0).then(|| div().flex_none().text_color(theme.muted).child("›").into_any_element());
                let label = div()
                    .min_w_0()
                    .truncate()
                    .when(i < last, |d| d.flex_none())
                    .text_color(if i == last { theme.text_loud } else { theme.muted })
                    .child(c)
                    .into_any_element();
                sep.into_iter().chain([label])
            }));
        let error = self.pane.as_ref().and_then(|p| p.sync.error()).map(str::to_string);
        let saving = self.pane.as_ref().is_some_and(|p| p.sync.is_saving());
        let line = match (error, self.updated_line(chrono::Utc::now())) {
            (Some(e), _) => Some((format!("Couldn't save: {e}"), theme.error)),
            (None, _) if saving => Some(("Saving…".to_string(), theme.muted)),
            (None, Some(u)) => Some((u, theme.muted)),
            (None, None) => None,
        };
        let updated = line.map(|(text, color)| {
            div()
                .debug_selector(|| "wiki-doc-updated".into())
                .flex_none()
                .max_w(px(320.0))
                .truncate()
                .type_style(Type::Small)
                .text_color(color)
                .child(text)
        });
        let more = sidebar::toolbar_button("wiki-doc-more", heroicon(icons::ELLIPSIS_VERTICAL), theme)
            .tooltip(style::tooltip("More"))
            .on_click(cx.listener(|this, e: &ClickEvent, _, cx| this.open_doc_menu(e.position(), cx)));
        div()
            .debug_selector(|| "wiki-doc-toolbar".into())
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(12.0))
            .h(px(44.0))
            .pl(px(24.0))
            .pr(px(12.0))
            .border_b_1()
            .border_color(theme.divider)
            .type_style(Type::Base)
            .child(crumb_row)
            .children(updated)
            .child(more)
            .into_any_element()
    }

    /// "Changed elsewhere": the hub's text changed under unsaved edits.
    fn render_elsewhere(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pane = self.pane.as_ref()?;
        if !pane.sync.changed_elsewhere() {
            return None;
        }
        Some(
            div()
                .debug_selector(|| "wiki-changed-elsewhere".into())
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(8.0))
                .px(px(24.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(theme.divider)
                .bg(theme.nested(theme.background_hover))
                .type_style(Type::Small)
                .child(heroicon(icons::EXCLAMATION_TRIANGLE).text_color(theme.icon_yellow))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.foreground)
                        .child("Changed elsewhere while you were editing."),
                )
                .child(
                    style::button("wiki-keep-mine", "Keep mine", ButtonKind::Secondary, ButtonSize::XSmall, false, theme)
                        .tooltip(style::tooltip("Save your version over it"))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.keep_mine(window, cx))),
                )
                .child(
                    style::button("wiki-reload", "Reload", ButtonKind::Primary, ButtonSize::XSmall, false, theme)
                        .tooltip(style::tooltip("Show the new version, dropping your edits"))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.reload_theirs(cx))),
                )
                .into_any_element(),
        )
    }

    /// ⋯'s menu, or the history it opened.
    fn render_doc_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.doc_menu.as_ref()?;
        let theme = cx.global::<Theme>().opaque();
        let panel = match menu {
            DocMenu::Actions => {
                let items: Vec<AnyElement> = DocAction::ALL
                    .iter()
                    .map(|&action| {
                        let id = SharedString::from(format!("wiki-doc-menu-{}", action.slug()));
                        let base = match self.doc_action_refusal(action) {
                            Some(why) => style::disabled_menu_item(id, &theme).tooltip(style::tooltip(why)),
                            None => style::menu_item(id, false, &theme).on_click(cx.listener(
                                move |this, _: &ClickEvent, window, cx| this.doc_action(action, window, cx),
                            )),
                        };
                        base.child(div().flex_1().min_w_0().truncate().child(action.label())).into_any_element()
                    })
                    .collect();
                style::menu_panel("wiki-doc-menu", MENU_W, &theme).children(items)
            }
            DocMenu::History(commits) => {
                let now = chrono::Utc::now();
                let rows: Vec<AnyElement> = match commits {
                    None => vec![history_note("Reading the history…", &theme)],
                    Some(Err(e)) => vec![history_note(&format!("Couldn't read the history: {e}"), &theme)],
                    Some(Ok(c)) if c.is_empty() => vec![history_note("Not committed yet.", &theme)],
                    Some(Ok(c)) => c
                        .iter()
                        .map(|commit| {
                            let who = Updated::of_commit(commit)
                                .map(|u| u.line(now).trim_start_matches("Updated by ").to_string())
                                .unwrap_or_else(|| commit.author.clone());
                            let short: String = commit.sha.chars().take(7).collect();
                            div()
                                .debug_selector(|| "wiki-history-row".into())
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .px(px(10.0))
                                .py(px(6.0))
                                .child(
                                    div()
                                        .type_style(Type::Small)
                                        .text_color(theme.foreground)
                                        .truncate()
                                        .child(commit.subject.clone()),
                                )
                                .child(
                                    div()
                                        .type_style(Type::XSmall)
                                        .text_color(theme.muted)
                                        .truncate()
                                        .child(format!("{who} · {short}")),
                                )
                                .into_any_element()
                        })
                        .collect(),
                };
                style::menu_panel("wiki-history", HISTORY_W, &theme).child(
                    div()
                        .id("wiki-history-scroll")
                        .debug_selector(|| "wiki-history".into())
                        .flex()
                        .flex_col()
                        .max_h(px(420.0))
                        .overflow_y_scroll()
                        .child(
                            div()
                                .px(px(10.0))
                                .py(px(4.0))
                                .type_style(Type::XSmall)
                                .text_color(theme.muted)
                                .child("History"),
                        )
                        .children(rows),
                )
            }
        };
        let menu = placed(&self.doc_menu_anchor, panel)
            .gap(tab_bar::CONTEXT_GAP)
            .on_dismiss(cx.listener(|this, _: &MouseDownEvent, _, cx| this.close_doc_menu(cx)));
        Some(gpui::deferred(menu).with_priority(1).into_any_element())
    }
}

/// A muted line in the history list.
fn history_note(text: &str, theme: &Theme) -> AnyElement {
    div()
        .px(px(10.0))
        .py(px(6.0))
        .type_style(Type::Small)
        .text_color(theme.muted)
        .child(text.to_string())
        .into_any_element()
}

/// A document's title: its first H1, as the tree labels it.
fn title_of(text: &str) -> Option<&str> {
    let body = without_frontmatter(text);
    body.lines()
        .map(str::trim_end)
        .find_map(|l| l.strip_prefix("# "))
        .map(str::trim)
}

/// `text` past a leading frontmatter block.
fn without_frontmatter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n") else {
        return text;
    };
    match rest.find("\n---\n") {
        Some(end) => &rest[end + 5..],
        None => text,
    }
}
