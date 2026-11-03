//! The Harness and Default model pickers as the TUI draws them: a text-field
//! box with the choice (and its provider, muted) and a `▾`, and the menu of
//! choices that drops below it. Shared by every place that asks for a runner
//! and model over `shelbi_app::harness_picker`: the orchestrator onboarding
//! (`shell/onboarding.rs`) and the new-project flow's Agent defaults page
//! (`overlay/add_project.rs`).

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};
use shelbi_app::view::{HarnessOption, ModelOption};

use crate::panel;
use crate::textbox;
use crate::theme::palette;

/// The Default model picker's last entry: a name typed in.
pub(crate) const CUSTOM_MODEL: &str = "Custom model…";

/// An open picker menu: which picker (the host's own tag) and its
/// highlighted row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Menu<T> {
    pub(crate) item: T,
    pub(crate) selected: usize,
}

/// Harness's menu rows ("Claude Code  Anthropic"), and the chosen one.
pub(crate) fn harness_labels(harnesses: &[HarnessOption]) -> (Vec<String>, Option<usize>) {
    let labels = harnesses
        .iter()
        .map(|h| match &h.provider {
            Some(p) => format!("{}  {p}", h.label),
            None => h.label.clone(),
        })
        .collect();
    (labels, harnesses.iter().position(|h| h.selected))
}

/// Default model's menu rows, then [`CUSTOM_MODEL`], and the chosen one.
pub(crate) fn model_labels(models: &[ModelOption]) -> (Vec<String>, Option<usize>) {
    let mut labels: Vec<String> = models.iter().map(|o| o.label.clone()).collect();
    labels.push(CUSTOM_MODEL.into());
    (labels, models.iter().position(|o| o.selected))
}

/// A picker's box: the choice (and its provider, muted) and a `▾` at the
/// right, on the text field's fill.
pub(crate) fn select_lines(
    label: &str,
    secondary: Option<&str>,
    focused: bool,
    disabled: bool,
    w: usize,
) -> [Line<'static>; 3] {
    let fill = textbox::fill(focused, disabled, false);
    let (above, below) = textbox::caps(w, fill);
    let on = Style::default().bg(fill);
    let fg = if disabled {
        palette().muted
    } else if focused {
        palette().white
    } else {
        palette().foreground
    };
    let room = w.saturating_sub(2 * textbox::INSET + 2);
    let label = panel::truncate_right(label, room);
    let mut used = panel::display_width(&label);
    let mut spans = vec![Span::styled(" ".repeat(textbox::INSET), on), Span::styled(label, on.fg(fg))];
    if let Some(s) = secondary.filter(|_| used + 2 < room) {
        let s = panel::truncate_right(s, room - used - 2);
        used += 2 + panel::display_width(&s);
        spans.push(Span::styled("  ", on));
        spans.push(Span::styled(s, on.fg(palette().muted)));
    }
    spans.push(Span::styled(" ".repeat(room.saturating_sub(used) + 1), on));
    spans.push(Span::styled("▾", on.fg(if disabled { palette().muted } else { palette().gray })));
    spans.push(Span::styled(" ".repeat(textbox::INSET), on));
    [above, Line::from(spans), below]
}

/// Draw a picker's menu from `at` (one row per label, clipped to `area`):
/// the chosen row's ✓, the highlighted row lit. Returns each drawn row's
/// rect, by index.
pub(crate) fn render_menu(
    f: &mut Frame,
    area: Rect,
    at: Rect,
    labels: &[String],
    chosen: Option<usize>,
    selected: usize,
) -> Vec<(Rect, usize)> {
    let w = at.width as usize;
    let bg = palette().dialog;
    let mut rows = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        let y = at.y + i as u16;
        if y >= area.bottom() {
            break;
        }
        let rect = Rect { y, height: 1, ..at };
        let lit = i == selected;
        let fill = if lit { palette().input_focus } else { bg };
        let mark = if Some(i) == chosen { "✓ " } else { "  " };
        let text = panel::truncate_right(&format!(" {mark}{label}"), w);
        let pad = w.saturating_sub(panel::display_width(&text));
        let fg = if lit { palette().white } else { palette().foreground };
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(text, Style::default().fg(fg).bg(fill)),
                Span::styled(" ".repeat(pad), Style::default().bg(fill)),
            ])),
            rect,
        );
        rows.push((rect, i));
    }
    rows
}
