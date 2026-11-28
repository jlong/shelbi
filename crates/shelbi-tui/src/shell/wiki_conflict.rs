//! The Wiki's **conflict inspector** in the TUI (`wiki-conflicts`): a
//! document the sync couldn't merge, its hunks listed with ours beside
//! theirs, and keys to choose a side per hunk, then apply.
//!
//! ```text
//! Resolve Plans › Auth · 1 of 2 chosen
//! o ours  t theirs  O / T all  ↑↓ hunk  Enter apply  Esc cancel
//! ───────────────────────────────────────────────────────────────
//!   ## Alpha
//! ▌ Ours ✓                        │ Theirs
//! ▌ Alpha, edited here.           │ Alpha, edited elsewhere.
//!   …
//! ```
//!
//! The state is the shared [`Inspector`] (the desktop's inspector walks the
//! same one); Apply sends its [`shelbi_app::wiki::Resolution`], which the hub
//! applies to its own diff of the sides (refusing stale choices). `c` in the
//! tree or the document opens it: on the open document when it's held, else
//! the selected one, else the first held document.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};
use shelbi_app::wiki::{Author, WikiReply, WikiRequest};
use shelbi_app::wiki_conflict::{context_shown, hidden_label, Inspector, Side};

use super::{Msg, WikiPage, WikiStep};
use crate::panel::{display_width, truncate_right};
use crate::theme::palette;

/// The inspector over the main area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConflictView {
    pub(super) path: String,
    /// `None` until the hub's sides land.
    pub(super) inspector: Option<Inspector>,
    pub(super) error: Option<String>,
    /// Apply is on its way.
    pub(super) applying: bool,
    /// The first body line shown.
    scroll: usize,
    /// The hunk the view last scrolled to, so the wheel can move away
    /// from it until the keys move to another.
    followed: Option<usize>,
}

/// The key hints under the inspector's title.
pub(super) const HINTS: &str = "o ours  t theirs  O / T all  ↑↓ hunk  Enter apply  Esc cancel";

impl WikiPage {
    /// Whether the hub holds `path` for a conflict.
    pub(crate) fn is_held(&self, path: &str) -> bool {
        self.status.as_ref().is_some_and(|s| s.conflicts.iter().any(|c| c == path))
    }

    /// The held document `c` opens: the open one, the selected one, or the
    /// first.
    fn conflict_target(&self) -> Option<String> {
        let held = &self.status.as_ref()?.conflicts;
        let selected = self.tree.selected_path().map(str::to_string);
        [self.doc_path().map(str::to_string), selected]
            .into_iter()
            .flatten()
            .find(|p| held.contains(p))
            .or_else(|| held.first().cloned())
    }

    /// `c`: inspect the held document. `false` when none is held.
    pub(super) fn inspect_conflict(&mut self) -> bool {
        let Some(path) = self.conflict_target() else {
            self.note = Some("Nothing to resolve: every document synced".into());
            return false;
        };
        self.inspect(&path);
        true
    }

    /// Read `path`'s sides and show them.
    pub(crate) fn inspect(&mut self, path: &str) {
        self.conflict = Some(ConflictView {
            path: path.to_string(),
            inspector: None,
            error: None,
            applying: false,
            scroll: 0,
            followed: None,
        });
        let p = path.to_string();
        self.spawn(WikiRequest::Conflict { path: path.to_string() }, move |r| Msg::Conflict(p, r));
    }

    /// The inspector's state, for tests.
    #[cfg(test)]
    pub(crate) fn inspector(&self) -> Option<&Inspector> {
        self.conflict.as_ref().and_then(|c| c.inspector.as_ref())
    }

    #[cfg(test)]
    pub(crate) fn inspecting(&self) -> Option<&str> {
        self.conflict.as_ref().map(|c| c.path.as_str())
    }

    /// The sides landed.
    pub(super) fn take_conflict(&mut self, path: String, reply: WikiReply) {
        let Some(view) = self.conflict.as_mut().filter(|c| c.path == path) else {
            return;
        };
        match reply {
            WikiReply::Conflict(c) => {
                view.inspector = Some(Inspector::new(c));
                view.error = None;
            }
            WikiReply::Failed(f) => view.error = Some(f.message),
            other => view.error = Some(format!("the hub answered {other:?}")),
        }
    }

    /// Apply landed: the document is settled, or why not.
    pub(super) fn take_resolved(&mut self, path: String, reply: WikiReply) {
        let Some(view) = self.conflict.as_mut().filter(|c| c.path == path) else {
            return;
        };
        view.applying = false;
        match reply {
            WikiReply::Done => {
                self.conflict = None;
                self.note = Some(format!("Resolved {path}"));
                self.read_status();
                if self.doc_path() == Some(path.as_str()) {
                    self.read_doc(&path);
                }
            }
            WikiReply::Failed(f) => view.error = Some(f.message),
            other => view.error = Some(format!("the hub answered {other:?}")),
        }
    }

    fn apply_resolution(&mut self) {
        let Some(view) = self.conflict.as_mut() else {
            return;
        };
        if view.applying {
            return;
        }
        let Some(resolution) = view.inspector.as_ref().and_then(Inspector::resolution) else {
            view.error = Some("Choose ours or theirs for every hunk first".into());
            return;
        };
        view.applying = true;
        view.error = None;
        let path = view.path.clone();
        let req = WikiRequest::Resolve {
            path: path.clone(),
            resolution,
            by: Author::Person,
        };
        self.spawn(req, move |r| Msg::Resolved(path, r));
    }

    /// A key while the inspector shows.
    pub(super) fn handle_conflict_key(&mut self, k: KeyEvent) -> WikiStep {
        let Some(view) = self.conflict.as_mut() else {
            return WikiStep::Stay;
        };
        if k.code == KeyCode::Esc {
            self.conflict = None;
            return WikiStep::Stay;
        }
        if matches!(k.code, KeyCode::Enter | KeyCode::Char('a')) {
            self.apply_resolution();
            return WikiStep::Stay;
        }
        let Some(ins) = view.inspector.as_mut() else {
            return WikiStep::Stay;
        };
        match k.code {
            KeyCode::Char('o') => ins.choose_here(Side::Ours),
            KeyCode::Char('t') => ins.choose_here(Side::Theirs),
            KeyCode::Char('O') => ins.choose_all(Side::Ours),
            KeyCode::Char('T') => ins.choose_all(Side::Theirs),
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => ins.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => ins.move_cursor(1),
            _ => return WikiStep::Stay,
        }
        view.error = None;
        WikiStep::Stay
    }

    /// Scroll the inspector (the wheel over the main area). `false` when
    /// it isn't showing.
    pub(super) fn scroll_conflict(&mut self, delta: isize) -> bool {
        let Some(view) = self.conflict.as_mut() else {
            return false;
        };
        view.scroll = view.scroll.saturating_add_signed(delta);
        true
    }

    /// The notice over the tree's status line while documents are held.
    pub(super) fn conflict_notice(&self) -> Option<Line<'static>> {
        let notice = shelbi_app::wiki_conflict::notice(self.status.as_ref()?)?;
        let p = palette();
        Some(Line::from(vec![
            Span::styled(format!("⚠ {notice}"), Style::default().fg(p.ansi_red)),
            Span::styled(" · c resolves", Style::default().fg(p.ansi_gray)),
        ]))
    }

    /// Draw the inspector into the main area.
    pub(super) fn render_conflict(&mut self, buf: &mut Buffer, area: Rect) {
        let p = palette();
        let x = area.x + 2;
        let w = area.width.saturating_sub(4) as usize;
        let title_of = |path: &str| self.tree.node(path).map(|n| n.title.clone());
        let Some(view) = self.conflict.as_mut() else {
            return;
        };
        let mut crumbs: Vec<String> = view.path.split('/').map(str::to_string).collect();
        if let (Some(last), Some(title)) = (crumbs.last_mut(), title_of(&view.path)) {
            *last = title;
        }
        let progress = match (&view.inspector, view.applying) {
            (_, true) => "Applying…".to_string(),
            (Some(ins), false) => ins.progress(),
            (None, false) => "Reading…".to_string(),
        };
        let title = Line::from(vec![
            Span::styled(
                format!("Resolve {}", crumbs.join(" › ")),
                Style::default().fg(p.ansi_white).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" · {progress}"), Style::default().fg(p.ansi_gray)),
        ]);
        buf.set_line(x, area.y, &title, w as u16);
        buf.set_stringn(x, area.y + 1, truncate_right(HINTS, w), w, Style::default().fg(p.ansi_dark_gray));
        let mut top = area.y + 2;
        if let Some(e) = &view.error {
            buf.set_stringn(x, top, truncate_right(e, w), w, Style::default().fg(p.ansi_red));
            top += 1;
        }
        buf.set_stringn(x, top, "─".repeat(w), w, Style::default().fg(p.ansi_dark_gray));
        top += 1;
        let height = area.bottom().saturating_sub(top) as usize;
        let Some(ins) = view.inspector.as_ref() else {
            return;
        };
        let (lines, cursor_at) = body_lines(ins, w);
        // Bring the hunk with the keys into view when they move to it.
        if let Some((start, end)) = cursor_at.filter(|_| view.followed != Some(ins.cursor())) {
            view.followed = Some(ins.cursor());
            if start < view.scroll {
                view.scroll = start;
            } else if end > view.scroll + height {
                view.scroll = end.saturating_sub(height).min(start);
            }
        }
        view.scroll = view.scroll.min(lines.len().saturating_sub(height));
        for (i, line) in lines.iter().skip(view.scroll).take(height).enumerate() {
            buf.set_line(area.x, top + i as u16, line, area.width);
        }
    }
}

/// The inspector's body at width `w`, and the lines the cursor's hunk
/// spans.
fn body_lines(ins: &Inspector, w: usize) -> (Vec<Line<'static>>, Option<(usize, usize)>) {
    let p = palette();
    let muted = Style::default().fg(p.ansi_gray);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cursor_at = None;
    let context = |out: &mut Vec<Line<'static>>, lines: &[String], above: bool| {
        let (shown, hidden) = context_shown(lines, above);
        if hidden > 0 && above {
            out.push(Line::from(Span::styled(format!("  {}", hidden_label(hidden)), Style::default().fg(p.ansi_dark_gray))));
        }
        for line in shown {
            out.push(Line::from(Span::styled(format!("  {}", truncate_right(line, w.saturating_sub(2))), muted)));
        }
        if hidden > 0 && !above {
            out.push(Line::from(Span::styled(format!("  {}", hidden_label(hidden)), Style::default().fg(p.ansi_dark_gray))));
        }
    };
    if ins.is_empty() {
        out.push(Line::from(Span::styled("  The two versions are the same. Enter settles it.", muted)));
        return (out, None);
    }
    let col = w.saturating_sub(5) / 2;
    for (i, hunk) in ins.hunks().iter().enumerate() {
        context(&mut out, &hunk.context_before, true);
        let here = i == ins.cursor();
        let start = out.len();
        let chosen = ins.choice(i);
        let mark = if here { "▌ " } else { "  " };
        let side_style = |side: Side| {
            let base = match side {
                Side::Ours => Style::default().fg(p.ansi_cyan),
                Side::Theirs => Style::default().fg(p.ansi_magenta),
            };
            match chosen {
                Some(c) if c != side => Style::default().fg(p.ansi_dark_gray),
                _ => base,
            }
        };
        let head = |side: Side| {
            let tick = if chosen == Some(side) { " ✓" } else { "" };
            pad(&format!("{}{tick}", side.label()), col)
        };
        out.push(Line::from(vec![
            Span::styled(mark, Style::default().fg(p.ansi_yellow)),
            Span::styled(head(Side::Ours), side_style(Side::Ours).add_modifier(Modifier::BOLD)),
            Span::styled(" │ ", Style::default().fg(p.ansi_dark_gray)),
            Span::styled(head(Side::Theirs), side_style(Side::Theirs).add_modifier(Modifier::BOLD)),
        ]));
        let rows = hunk.ours.len().max(hunk.theirs.len()).max(1);
        for r in 0..rows {
            let cell = |side: &[String]| match (side.get(r), r, side.is_empty()) {
                (Some(line), _, _) => pad(&truncate_right(line, col), col),
                (None, 0, true) => pad("(nothing)", col),
                _ => pad("", col),
            };
            out.push(Line::from(vec![
                Span::styled(mark, Style::default().fg(p.ansi_yellow)),
                Span::styled(cell(&hunk.ours), side_style(Side::Ours)),
                Span::styled(" │ ", Style::default().fg(p.ansi_dark_gray)),
                Span::styled(cell(&hunk.theirs), side_style(Side::Theirs)),
            ]));
        }
        if here {
            cursor_at = Some((start, out.len()));
        }
        out.push(Line::default());
    }
    context(&mut out, &ins.diff().trailing_context, false);
    (out, cursor_at)
}

/// `text` padded with spaces to `width` columns.
fn pad(text: &str, width: usize) -> String {
    let n = display_width(text);
    if n >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - n))
    }
}
