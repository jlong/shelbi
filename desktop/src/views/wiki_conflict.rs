//! The Wiki's **conflict inspector** (`wiki-conflicts`): a document the
//! sync couldn't merge, shown in the main area as its hunks, ours beside
//! theirs, each with its own choice, modeled on ContextStore's conflict
//! view.
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────┐
//! │ Resolve › Plans › Auth · 1 of 2 chosen   All ours  All theirs  Cancel  Apply │
//! │ This document changed here and elsewhere in ways that can't be …     │
//! │ ──────────────────────────────────────────────────────────────────── │
//! │   ## Alpha                                                           │
//! │ ┌ Ours · This machine's version ✓ ┐┌ Theirs · From elsewhere        ┐│
//! │ │ Alpha, edited here.             ││ Alpha, edited elsewhere.       ││
//! │ └──────────────────── Use ours ───┘└────────────────── Use theirs ──┘│
//! └──────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! While documents are held, a notice sits over the Wiki's footer (with
//! Resolve), and the open document, when it's one of them, shows a bar
//! (with Resolve…). The state is the shared [`Inspector`] (the TUI walks
//! the same one); Apply sends its [`shelbi_app::wiki::Resolution`], which the
//! hub applies to its own diff of the sides, refusing choices made on sides
//! that have moved on. A hub change to the inspected document reads its
//! sides again.

use gpui::{div, prelude::*, px, AnyElement, ClickEvent, Context, Hsla, ScrollHandle, SharedString, Window};
use shelbi_app::wiki::{Author, WikiReply, WikiRequest};
use shelbi_app::wiki_conflict::{context_shown, hidden_label, notice, Hunk, Inspector, Side, DOC_NOTICE};
use shelbi_app::wiki_tree_edit as edit;

use super::WikiView;
use crate::assets::{heroicon, icons};
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The inspector's widest.
const MAX_W: f32 = 1100.0;
/// A side's lines' tint.
const SIDE_TINT: f32 = 0.10;

/// The inspector over the main area, in place of the document.
pub(super) struct ConflictPane {
    pub(super) path: String,
    /// The hub's sides, once read (or why not).
    pub(super) inspector: Option<Result<Inspector, String>>,
    /// Apply is on its way.
    pub(super) applying: bool,
    /// Why the last Apply was refused.
    pub(super) error: Option<String>,
    scroll: ScrollHandle,
}

impl WikiView {
    // --- the state -----------------------------------------------------------

    /// Whether the hub holds `path` for a conflict.
    pub fn is_held(&self, path: &str) -> bool {
        matches!(&self.status, Some(Ok(s)) if s.conflicts.iter().any(|c| c == path))
    }

    /// The document Resolve opens: the open one when held, else the first.
    fn conflict_target(&self) -> Option<String> {
        let Some(Ok(s)) = &self.status else {
            return None;
        };
        self.open.clone().filter(|p| s.conflicts.contains(p)).or_else(|| s.conflicts.first().cloned())
    }

    /// The footer's Resolve.
    pub fn inspect_first(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.conflict_target() {
            self.inspect(&path, window, cx);
        }
    }

    /// Read `path`'s sides and show the inspector. What's typed in the open
    /// document is saved first, so it's ours.
    pub fn inspect(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_save(window, cx);
        self.doc_menu = None;
        self.conflict = Some(ConflictPane {
            path: path.to_string(),
            inspector: None,
            applying: false,
            error: None,
            scroll: ScrollHandle::new(),
        });
        self.read_conflict(path.to_string(), window, cx);
        cx.notify();
    }

    fn read_conflict(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let req = WikiRequest::Conflict { path: path.clone() };
        self.request(req, window, cx, move |this, reply, _, _| {
            let Some(pane) = this.conflict.as_mut().filter(|c| c.path == path) else {
                return;
            };
            pane.inspector = Some(match reply {
                WikiReply::Conflict(c) => Ok(Inspector::new(c)),
                WikiReply::Failed(f) => Err(f.message),
                other => Err(format!("the hub answered {other:?}")),
            });
        });
    }

    /// Cancel: back to the document.
    pub fn close_conflict(&mut self, cx: &mut Context<Self>) {
        self.conflict = None;
        cx.notify();
    }

    fn inspector_mut(&mut self) -> Option<&mut Inspector> {
        self.conflict.as_mut().and_then(|c| c.inspector.as_mut()).and_then(|r| r.as_mut().ok())
    }

    /// A hunk's Use ours / Use theirs (again: undecided).
    pub fn choose_hunk(&mut self, hunk: usize, side: Side, cx: &mut Context<Self>) {
        if let Some(ins) = self.inspector_mut() {
            ins.choose(hunk, side);
        }
        if let Some(pane) = self.conflict.as_mut() {
            pane.error = None;
        }
        cx.notify();
    }

    /// Use all ours / Use all theirs.
    pub fn choose_all(&mut self, side: Side, cx: &mut Context<Self>) {
        if let Some(ins) = self.inspector_mut() {
            ins.choose_all(side);
        }
        cx.notify();
    }

    /// Apply: send the choices; the hub commits "Resolve merge conflicts".
    pub fn apply_resolution(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.conflict.as_mut().filter(|c| !c.applying) else {
            return;
        };
        let Some(resolution) = pane.inspector.as_ref().and_then(|r| r.as_ref().ok()).and_then(Inspector::resolution) else {
            return;
        };
        pane.applying = true;
        pane.error = None;
        let path = pane.path.clone();
        let req = WikiRequest::Resolve {
            path: path.clone(),
            resolution,
            by: Author::Person,
        };
        self.request(req, window, cx, move |this, reply, window, cx| {
            let Some(pane) = this.conflict.as_mut().filter(|c| c.path == path) else {
                return;
            };
            pane.applying = false;
            match reply {
                WikiReply::Done => {
                    this.conflict = None;
                    this.read_status(window, cx);
                    if this.open.as_deref() == Some(path.as_str()) {
                        this.read_doc(window, cx);
                    }
                }
                WikiReply::Failed(f) => pane.error = Some(f.message),
                other => pane.error = Some(format!("the hub answered {other:?}")),
            }
        });
        cx.notify();
    }

    /// A status landed: an inspected document no longer held was resolved
    /// elsewhere (the CLI, another client).
    pub(super) fn conflict_status_landed(&mut self) {
        let Some(path) = self.conflict.as_ref().filter(|c| !c.applying).map(|c| c.path.clone()) else {
            return;
        };
        if matches!(&self.status, Some(Ok(_))) && !self.is_held(&path) {
            self.conflict = None;
            self.notice = Some(format!("{path} was resolved."));
        }
    }

    /// The hub changed `paths`: the inspected document's sides moved, so
    /// the choices made on them don't hold.
    pub(super) fn conflict_changed(&mut self, paths: &[String], window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.conflict.as_ref().filter(|c| !c.applying).map(|c| c.path.clone()) else {
            return;
        };
        if paths.contains(&path) {
            self.read_conflict(path, window, cx);
        }
    }

    #[cfg(test)]
    pub fn inspector(&self) -> Option<&Inspector> {
        self.conflict.as_ref().and_then(|c| c.inspector.as_ref()).and_then(|r| r.as_ref().ok())
    }

    #[cfg(test)]
    pub fn inspecting(&self) -> Option<&str> {
        self.conflict.as_ref().map(|c| c.path.as_str())
    }

    #[cfg(test)]
    pub fn conflict_error(&self) -> Option<&str> {
        self.conflict.as_ref().and_then(|c| c.error.as_deref())
    }

    /// Open the inspector on `path` with `choices` made, for the visual
    /// check's scene (it waits for the read).
    #[cfg(feature = "visual-check")]
    pub fn choose_for_visual(&mut self, choices: &[(usize, Side)], cx: &mut Context<Self>) {
        for &(hunk, side) in choices {
            if let Some(ins) = self.inspector_mut() {
                ins.choose(hunk, side);
            }
        }
        cx.notify();
    }

    // --- rendering -------------------------------------------------------------

    /// Over the footer while documents are held: how many, and Resolve.
    pub(super) fn render_conflict_notice(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(Ok(status)) = &self.status else {
            return None;
        };
        let text = notice(status)?;
        Some(
            div()
                .debug_selector(|| "wiki-conflict-notice".into())
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .mx(px(12.0))
                .mb(px(8.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(8.0))
                .bg(theme.red_tint)
                .type_style(Type::Small)
                .child(heroicon(icons::EXCLAMATION_TRIANGLE).flex_none().text_color(theme.error))
                .child(div().flex_1().min_w_0().text_color(theme.foreground).child(text))
                .child(
                    style::button("wiki-conflict-notice-resolve", "Resolve", ButtonKind::Ghost, ButtonSize::XSmall, false, theme)
                        .tooltip(style::tooltip("Choose which version to keep"))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.inspect_first(window, cx))),
                )
                .into_any_element(),
        )
    }

    /// Over the open document while it's held.
    pub(super) fn render_held_bar(&self, path: &str, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.is_held(path) {
            return None;
        }
        let path = path.to_string();
        Some(
            div()
                .debug_selector(|| "wiki-doc-held".into())
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(8.0))
                .px(px(24.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(theme.divider)
                .bg(theme.red_tint)
                .type_style(Type::Small)
                .child(heroicon(icons::EXCLAMATION_TRIANGLE).flex_none().text_color(theme.error))
                .child(div().flex_1().min_w_0().text_color(theme.foreground).child(DOC_NOTICE))
                .child(
                    style::button("wiki-doc-held-resolve", "Resolve…", ButtonKind::Primary, ButtonSize::XSmall, false, theme)
                        .tooltip(style::tooltip("Compare the two versions and choose"))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.inspect(&path, window, cx))),
                )
                .into_any_element(),
        )
    }

    /// The inspector in the main area.
    pub(super) fn render_conflict(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(pane) = self.conflict.as_ref() else {
            return div().into_any_element();
        };
        let ins = pane.inspector.as_ref().and_then(|r| r.as_ref().ok());
        let body = match &pane.inspector {
            None => super::note("wiki-conflict-loading", "Reading both versions…", None, theme).into_any_element(),
            Some(Err(e)) => super::note("wiki-conflict-read-error", "Couldn't read the conflict", Some(e.clone()), theme)
                .into_any_element(),
            Some(Ok(ins)) => self.render_hunks(ins, &pane.scroll, theme, cx),
        };
        div()
            .debug_selector(|| "wiki-conflict".into())
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(self.render_conflict_toolbar(pane, ins, theme, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(4.0))
                    .px(px(24.0))
                    .py(px(10.0))
                    .border_b_1()
                    .border_color(theme.divider)
                    .type_style(Type::Small)
                    .child(div().text_color(theme.muted).child(
                        "This document changed here and elsewhere in ways that can't be combined. \
                         Choose a side for each change, then apply. Until then it doesn't sync.",
                    ))
                    .children(pane.error.clone().map(|e| {
                        div().debug_selector(|| "wiki-conflict-error".into()).text_color(theme.error).child(e)
                    })),
            )
            .child(body)
            .into_any_element()
    }

    fn render_conflict_toolbar(
        &self,
        pane: &ConflictPane,
        ins: Option<&Inspector>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut crumbs = vec!["Resolve".to_string()];
        crumbs.extend(edit::breadcrumb(self.tree.nodes(), &pane.path));
        let last = crumbs.len() - 1;
        let crumb_row = div()
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
        let progress = match (ins, pane.applying) {
            (_, true) => "Applying…".to_string(),
            (Some(ins), false) => ins.progress(),
            (None, false) => String::new(),
        };
        let ready = !pane.applying && ins.is_some_and(Inspector::is_complete);
        let has_hunks = ins.is_some_and(|i| !i.is_empty()) && !pane.applying;
        let all = |id: &'static str, label: &'static str, side: Side| {
            if has_hunks {
                style::button(id, label, ButtonKind::Ghost, ButtonSize::XSmall, false, theme)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.choose_all(side, cx)))
            } else {
                style::disabled_button(id, label, ButtonKind::Ghost, ButtonSize::XSmall, theme)
            }
        };
        let apply = if ready {
            style::button("wiki-conflict-apply", "Apply", ButtonKind::Primary, ButtonSize::XSmall, false, theme)
                .tooltip(style::tooltip("Commit the result and sync"))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.apply_resolution(window, cx)))
        } else {
            style::disabled_button("wiki-conflict-apply", "Apply", ButtonKind::Primary, ButtonSize::XSmall, theme)
                .tooltip(style::tooltip("Choose a side for every change first"))
        };
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .h(px(44.0))
            .pl(px(24.0))
            .pr(px(12.0))
            .border_b_1()
            .border_color(theme.divider)
            .type_style(Type::Base)
            .child(crumb_row)
            .child(
                div()
                    .debug_selector(|| "wiki-conflict-progress".into())
                    .flex_none()
                    .type_style(Type::Small)
                    .text_color(theme.muted)
                    .child(progress),
            )
            .child(all("wiki-conflict-all-ours", "Use all ours", Side::Ours))
            .child(all("wiki-conflict-all-theirs", "Use all theirs", Side::Theirs))
            .child(
                style::button("wiki-conflict-cancel", "Cancel", ButtonKind::Secondary, ButtonSize::XSmall, false, theme)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_conflict(cx))),
            )
            .child(apply)
            .into_any_element()
    }

    fn render_hunks(&self, ins: &Inspector, scroll: &ScrollHandle, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        if ins.is_empty() {
            rows.push(
                super::note("wiki-conflict-same", "The two versions are the same", Some("Apply settles it.".into()), theme)
                    .into_any_element(),
            );
        }
        for (i, hunk) in ins.hunks().iter().enumerate() {
            rows.extend(context_block(&hunk.context_before, true, theme));
            rows.push(self.render_hunk(i, hunk, ins.choice(i), theme, cx));
        }
        rows.extend(context_block(&ins.diff().trailing_context, false, theme));
        div()
            .id("wiki-conflict-body")
            .debug_selector(|| "wiki-conflict-body".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(scroll)
            .px(px(24.0))
            .py(px(16.0))
            .child(div().flex().flex_col().gap(px(8.0)).w_full().max_w(px(MAX_W)).children(rows))
            .into_any_element()
    }

    fn render_hunk(&self, i: usize, hunk: &Hunk, chosen: Option<Side>, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let side = |side: Side| {
            let lines = hunk.side(side);
            let picked = chosen == Some(side);
            let dimmed = chosen.is_some() && !picked;
            let hue = side_hue(side, theme);
            let id = SharedString::from(format!("wiki-conflict-{i}-{}", slug(side)));
            let label = match side {
                Side::Ours => "Use ours",
                Side::Theirs => "Use theirs",
            };
            let button = style::button(
                id.clone(),
                label,
                if picked { ButtonKind::Select } else { ButtonKind::Secondary },
                ButtonSize::XSmall,
                false,
                theme,
            )
            .tooltip(style::tooltip(if picked { "Chosen. Click again to undo" } else { "Keep this side of the change" }))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.choose_hunk(i, side, cx)));
            let body: Vec<AnyElement> = if lines.is_empty() {
                vec![div()
                    .px(px(10.0))
                    .py(px(2.0))
                    .italic()
                    .text_color(theme.muted)
                    .child(match side {
                        Side::Ours => "Nothing here (removed on this machine)",
                        Side::Theirs => "Nothing here (removed elsewhere)",
                    })
                    .into_any_element()]
            } else {
                lines
                    .iter()
                    .map(|l| {
                        div()
                            .px(px(10.0))
                            .py(px(1.0))
                            .bg(hue.opacity(SIDE_TINT))
                            .text_color(theme.foreground)
                            .whitespace_normal()
                            .child(if l.is_empty() { " ".to_string() } else { l.clone() })
                            .into_any_element()
                    })
                    .collect()
            };
            div()
                .debug_selector(move || format!("wiki-conflict-{i}-{}-side", slug(side)))
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .rounded(px(8.0))
                .border_1()
                .border_color(if picked { hue } else { theme.divider })
                .overflow_hidden()
                .when(dimmed, |d| d.opacity(0.45))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .px(px(10.0))
                        .py(px(6.0))
                        .border_b_1()
                        .border_color(theme.divider)
                        .type_style(Type::XSmall)
                        .child(div().flex_none().size(px(8.0)).rounded_full().bg(hue))
                        .child(div().flex_none().text_color(theme.text_loud).child(side.label()))
                        .child(div().flex_1().min_w_0().truncate().text_color(theme.muted).child(side.detail()))
                        .children(picked.then(|| heroicon(icons::CHECK).flex_none().text_color(hue))),
                )
                .child(div().flex().flex_col().py(px(4.0)).type_style(Type::Small).children(body))
                .child(div().flex().flex_row().justify_end().px(px(8.0)).pb(px(8.0)).child(button))
        };
        div()
            .debug_selector(move || format!("wiki-conflict-hunk-{i}"))
            .flex()
            .flex_row()
            .gap(px(12.0))
            .w_full()
            .child(side(Side::Ours))
            .child(side(Side::Theirs))
            .into_any_element()
    }
}

/// The unchanged lines around a hunk, a few of them.
fn context_block(lines: &[String], above: bool, theme: &Theme) -> Option<AnyElement> {
    if lines.is_empty() {
        return None;
    }
    let (shown, hidden) = context_shown(lines, above);
    let more = (hidden > 0).then(|| div().italic().text_color(theme.muted).child(hidden_label(hidden)).into_any_element());
    let lines = shown
        .iter()
        .map(|l| div().text_color(theme.muted).child(if l.is_empty() { " ".to_string() } else { l.clone() }).into_any_element());
    let mut children: Vec<AnyElement> = Vec::new();
    if above {
        children.extend(more);
        children.extend(lines);
    } else {
        children.extend(lines);
        children.extend(more);
    }
    Some(
        div()
            .flex()
            .flex_col()
            .px(px(11.0))
            .type_style(Type::Small)
            .children(children)
            .into_any_element(),
    )
}

/// A side's color: ours the accent cyan, theirs purple.
fn side_hue(side: Side, theme: &Theme) -> Hsla {
    match side {
        Side::Ours => theme.icon_cyan,
        Side::Theirs => theme.icon_purple,
    }
}

fn slug(side: Side) -> &'static str {
    match side {
        Side::Ours => "ours",
        Side::Theirs => "theirs",
    }
}
