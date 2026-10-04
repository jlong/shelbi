//! The shell's sidebar: a renderer over shelbi-app's [`SidebarModel`].
//!
//! It adds no model logic — the rows (Chat / Issues / Activity, the workspace
//! pool, the review column) come from `shelbi_app::view::SidebarModel`, built by
//! the shell's background refresher. This module only lays those rows out and
//! maps a selection to what the main area should show. The labels and glyphs
//! match the existing tmux sidebar (`crate::sidebar`): 💬 Chat, 📋 Issues,
//! ⚡ Activity, and the `— Section —` separators.
//!
//! Selection is a single flat index over the *selectable* rows (section headers
//! are skipped), exactly as [`shelbi_app::nav::ClientState`] models it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use shelbi_app::view::SidebarModel;
use shelbi_app::nav::View;

use super::session::SessionRef;
use crate::theme::SELECTION_BG;

/// What a selectable sidebar row routes to when opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowTarget {
    /// A live session shown in the terminal view.
    Session(SessionRef),
    /// A native view (issues / activity / machines) — a placeholder until
    /// `rt-tui-native-views` lands.
    Native(View),
    /// A review-column task — a placeholder until the review interface
    /// (`rt-tui-review`) lands.
    Review(String),
}

/// One line in the sidebar.
enum Row {
    Section(String),
    Selectable { glyph: &'static str, label: String, subtitle: Option<String>, target: RowTarget },
}

/// A laid-out sidebar ready to render and hit-test.
pub struct SidebarView {
    project_label: String,
    rows: Vec<Row>,
    /// Indices into `rows` that are selectable, in order.
    selectable: Vec<usize>,
    zen_on: bool,
    unread_errors: usize,
}

impl SidebarView {
    pub fn build(model: &SidebarModel) -> Self {
        let mut rows: Vec<Row> = Vec::new();

        for nav in &model.nav {
            rows.push(Row::Selectable {
                glyph: nav_glyph(&nav.label),
                label: nav.label.clone(),
                subtitle: None,
                target: target_from_view(&nav.view),
            });
        }

        if !model.workspaces.is_empty() {
            rows.push(Row::Section("Workspaces".into()));
            for ws in &model.workspaces {
                let subtitle = match (&ws.current_task, &ws.agent) {
                    (Some(task), Some(agent)) => Some(format!("{task} · {agent}")),
                    (Some(task), None) => Some(task.clone()),
                    _ => Some("idle".into()),
                };
                rows.push(Row::Selectable {
                    glyph: " ",
                    label: ws.name.clone(),
                    subtitle,
                    target: RowTarget::Session(SessionRef::Workspace(ws.name.clone())),
                });
            }
        }

        if !model.reviews.is_empty() {
            rows.push(Row::Section("Ready for Review".into()));
            for r in &model.reviews {
                rows.push(Row::Selectable {
                    glyph: " ",
                    label: r.title.clone(),
                    subtitle: Some(r.task_id.clone()),
                    target: RowTarget::Review(r.task_id.clone()),
                });
            }
        }

        let selectable = rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| matches!(r, Row::Selectable { .. }).then_some(i))
            .collect();

        Self {
            project_label: model.project_label.clone(),
            rows,
            selectable,
            zen_on: model.zen_on,
            unread_errors: model.unread_errors,
        }
    }

    pub fn selectable_count(&self) -> usize {
        self.selectable.len()
    }

    /// The target of the `selection`-th selectable row.
    pub fn target_at(&self, selection: usize) -> Option<RowTarget> {
        let row = *self.selectable.get(selection)?;
        match &self.rows[row] {
            Row::Selectable { target, .. } => Some(target.clone()),
            Row::Section(_) => None,
        }
    }

    /// Map a click at viewer `(x, y)` to the selection index of the row there,
    /// or `None` for a header / blank / out-of-bounds cell. The layout matches
    /// [`Self::render`]: one title row, then one row per `rows` entry.
    pub fn hit(&self, area: Rect, x: u16, y: u16) -> Option<usize> {
        if !contains(area, x, y) {
            return None;
        }
        // Row 0 is the project title; rows begin at area.top() + 1.
        let first = area.top() + 1;
        if y < first {
            return None;
        }
        let row_idx = (y - first) as usize;
        if row_idx >= self.rows.len() {
            return None;
        }
        // Which selectable ordinal is this row, if any?
        self.selectable.iter().position(|&i| i == row_idx)
    }

    /// Paint the sidebar. `selection` is the selectable-row index; `focused`
    /// brightens the selected row (vs a dim marker when focus is in the main
    /// area).
    pub fn render(&self, buf: &mut Buffer, area: Rect, selection: usize, focused: bool) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let mut lines: Vec<Line> = Vec::with_capacity(self.rows.len() + 1);

        // Title.
        let mut title = self.project_label.clone();
        if self.zen_on {
            title.push_str("  ·  ZEN");
        }
        if self.unread_errors > 0 {
            title.push_str(&format!("  ·  {}!", self.unread_errors));
        }
        lines.push(Line::from(Span::styled(
            title,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));

        let sel_row = self.selectable.get(selection).copied();
        for (i, row) in self.rows.iter().enumerate() {
            match row {
                Row::Section(label) => {
                    lines.push(Line::from(Span::styled(
                        format!("— {label} —"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                Row::Selectable { glyph, label, subtitle, .. } => {
                    let selected = Some(i) == sel_row;
                    let base = if selected {
                        if focused {
                            Style::default()
                                .bg(SELECTION_BG)
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().bg(SELECTION_BG).fg(Color::Gray)
                        }
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    let mut spans = vec![Span::styled(format!("{glyph} {label}"), base)];
                    if let Some(sub) = subtitle {
                        spans.push(Span::styled(
                            format!("  {sub}"),
                            base.fg(Color::DarkGray).remove_modifier(Modifier::BOLD),
                        ));
                    }
                    lines.push(Line::from(spans));
                }
            }
        }

        Paragraph::new(lines).render(area, buf);
    }
}

fn nav_glyph(label: &str) -> &'static str {
    match label {
        "Chat" => "💬",
        "Issues" => "📋",
        "Activity" => "⚡",
        "Machines" => "🖥",
        _ => "•",
    }
}

/// Map a shelbi-app nav `View` to a shell route. The orchestrator chat is the
/// session literally named `"orch"`; every other `Session` view names a
/// workspace; the rest are native views.
fn target_from_view(view: &View) -> RowTarget {
    match view {
        View::Session(name) if name == "orch" => RowTarget::Session(SessionRef::Orchestrator),
        View::Session(name) => RowTarget::Session(SessionRef::Workspace(name.clone())),
        other => RowTarget::Native(other.clone()),
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.left() && x < area.right() && y >= area.top() && y < area.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_app::view::{NavItem, ReviewRow, WorkspaceRow};

    fn model() -> SidebarModel {
        SidebarModel {
            project_label: "shelbi".into(),
            nav: vec![
                NavItem { label: "Chat".into(), view: View::Session("orch".into()) },
                NavItem { label: "Issues".into(), view: View::Issues },
                NavItem { label: "Activity".into(), view: View::Activity },
            ],
            workspaces: vec![
                WorkspaceRow { name: "alpha".into(), current_task: Some("t-1".into()), agent: Some("developer".into()) },
                WorkspaceRow { name: "bravo".into(), current_task: None, agent: None },
            ],
            reviews: vec![ReviewRow { task_id: "rev-9".into(), title: "Fix it".into() }],
            zen_on: false,
            unread_errors: 0,
        }
    }

    #[test]
    fn selectable_rows_skip_section_headers_and_route_correctly() {
        let v = SidebarView::build(&model());
        // 3 nav + 2 workspaces + 1 review = 6 selectable rows (2 section headers
        // are not selectable).
        assert_eq!(v.selectable_count(), 6);
        assert_eq!(v.target_at(0), Some(RowTarget::Session(SessionRef::Orchestrator)));
        assert_eq!(v.target_at(1), Some(RowTarget::Native(View::Issues)));
        assert_eq!(v.target_at(2), Some(RowTarget::Native(View::Activity)));
        assert_eq!(v.target_at(3), Some(RowTarget::Session(SessionRef::Workspace("alpha".into()))));
        assert_eq!(v.target_at(4), Some(RowTarget::Session(SessionRef::Workspace("bravo".into()))));
        assert_eq!(v.target_at(5), Some(RowTarget::Review("rev-9".into())));
        assert_eq!(v.target_at(6), None);
    }

    #[test]
    fn click_hit_testing_maps_rows_and_skips_headers() {
        let v = SidebarView::build(&model());
        let area = Rect::new(0, 0, 28, 20);
        // Row layout from top: 0=title, 1=Chat, 2=Issues, 3=Activity,
        // 4="— Workspaces —" (header), 5=alpha, 6=bravo,
        // 7="— Ready for Review —" (header), 8=rev-9.
        assert_eq!(v.hit(area, 2, 0), None, "title row isn't selectable");
        assert_eq!(v.hit(area, 2, 1), Some(0), "Chat");
        assert_eq!(v.hit(area, 2, 3), Some(2), "Activity");
        assert_eq!(v.hit(area, 2, 4), None, "Workspaces header isn't selectable");
        assert_eq!(v.hit(area, 2, 5), Some(3), "alpha");
        assert_eq!(v.hit(area, 2, 7), None, "Review header isn't selectable");
        assert_eq!(v.hit(area, 2, 8), Some(5), "rev-9");
        assert_eq!(v.hit(area, 2, 50), None, "below the rows");
        assert_eq!(v.hit(Rect::new(0, 0, 28, 20), 99, 5), None, "outside the sidebar");
    }

    #[test]
    fn renders_without_panicking_on_a_small_area() {
        let v = SidebarView::build(&model());
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        v.render(&mut buf, area, 0, true); // must not panic
    }
}
