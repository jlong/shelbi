//! The Linear connection's rows as the TUI draws them, over
//! `shelbi_app::linear_setup::LinearChoice`: the account (whose key, or the
//! masked key field to paste one into), the Team picker (required), then the
//! optional Project, Label and Assignee pickers and the sentence saying what
//! the picks show. Shared by the new-project dialog's Issues page
//! (`overlay/add_project.rs`) and Settings ▸ Issues
//! (`shell/settings/issues.rs`), after the desktop's
//! `views/linear_fields.rs`.
//!
//! ```text
//!  Linear account  Ada Lovelace <ada@example.com>
//!  Team *
//!  ▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁
//!   ENG  Engineering             ▾
//!  ▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔
//!  Project … Label … Assignee …
//!  Shelbi shows and creates issues in ENG, assigned to you.
//! ```
//!
//! The host owns focus and menus: it says which part is focused, and gets
//! back each row's part (for clicks) and where each picker's box starts (to
//! drop its menu under).

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use shelbi_app::linear_setup::{
    Filter, KeyState, LinearChoice, Loadable, PickOption, ACCOUNT_LABEL, ANY, CHANGE_KEY, CHOOSE_TEAM, FILTERS_HELP, KEY_HELP,
    KEY_LABEL, LOADING, NOT_CONNECTED, TEAM_LABEL,
};

use crate::panel;
use crate::picker::select_lines;
use crate::textbox;
use crate::theme::palette;

/// A part of the rows a click or the keyboard lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Part {
    Key,
    /// "Change key…" under the account (where the host offers it).
    ChangeKey,
    Team,
    Filter(Filter),
}

/// The rows drawn: each line with the part it belongs to, and the row each
/// picker's box starts on.
#[derive(Debug, Default)]
pub(crate) struct Drawn {
    pub(crate) lines: Vec<(Line<'static>, Option<Part>)>,
    pub(crate) boxes: Vec<(Part, usize)>,
}

/// The pasted key as the field shows it: one dot a character.
pub(crate) fn masked(key: &str) -> String {
    "•".repeat(key.chars().count())
}

fn muted(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(palette().muted))
}

fn wrap(text: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let need = if line.is_empty() { 0 } else { 1 } + panel::display_width(word);
        if !line.is_empty() && panel::display_width(&line) + need > w {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// What a picker's box shows: the row picked, else the list's state.
fn shown(list: &Loadable<Vec<PickOption>>, picked: Option<&PickOption>, empty: &str) -> (String, Option<String>) {
    match (list, picked) {
        (_, Some(row)) => (row.label.clone(), row.detail.clone()),
        (Loadable::Loading, None) => (LOADING.to_string(), None),
        (Loadable::Failed(_), None) => ("Couldn't load".to_string(), None),
        _ => (empty.to_string(), None),
    }
}

/// The Linear rows for `choice`, `w` columns wide, `focus` lit. `editable`
/// false greys the pickers (a connection running); `change_key` offers
/// "Change key…" under a working key's account.
pub(crate) fn rows(choice: &LinearChoice, focus: Option<Part>, editable: bool, change_key: bool, w: usize) -> Drawn {
    let mut d = Drawn::default();
    let gray = Style::default().fg(palette().gray);
    // The account.
    if choice.needs_key() {
        d.lines.push((textbox::label(ACCOUNT_LABEL, focus == Some(Part::Key), false, w), None));
        let (note, style) = match choice.key_error() {
            Some(why) => (why.to_string(), Style::default().fg(palette().ansi_red)),
            None if choice.changing_key() => {
                ("Paste the new key. The current one stays until it's verified.".to_string(), gray)
            }
            None => (NOT_CONNECTED.to_string(), gray),
        };
        for line in wrap(&note, w) {
            d.lines.push((Line::from(Span::styled(line, style)), None));
        }
        let focused = focus == Some(Part::Key);
        let mut spans = vec![Span::styled(
            format!("{KEY_LABEL}  "),
            if focused { Style::default().fg(palette().white).add_modifier(Modifier::BOLD | Modifier::UNDERLINED) } else { gray },
        )];
        let key = choice.key_input();
        if key.is_empty() && !focused {
            spans.push(muted("lin_api_…"));
        } else {
            spans.push(Span::raw(masked(key)));
        }
        if focused {
            spans.push(Span::styled("▏", Style::default().fg(palette().ansi_cyan)));
        }
        if choice.logging_in() {
            spans.push(muted("  Verifying…"));
        } else if focused {
            spans.push(muted("  ⏎ verify"));
        }
        d.lines.push((Line::from(spans), Some(Part::Key)));
        for line in wrap(KEY_HELP, w) {
            d.lines.push((Line::from(muted(line)), Some(Part::Key)));
        }
    } else {
        let who = match choice.key_state() {
            KeyState::Valid { viewer: Some(v) } => v.clone(),
            KeyState::Valid { viewer: None } => "Connected".into(),
            _ => "Checking the Linear API key…".into(),
        };
        d.lines.push((
            Line::from(vec![Span::styled(format!("{ACCOUNT_LABEL}  "), gray), Span::raw(panel::truncate_right(&who, w))]),
            None,
        ));
        if change_key && matches!(choice.key_state(), KeyState::Valid { .. }) {
            let style = if focus == Some(Part::ChangeKey) {
                Style::default().fg(palette().white).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(palette().cyan)
            };
            d.lines.push((Line::from(Span::styled(CHANGE_KEY, style)), Some(Part::ChangeKey)));
        }
    }
    d.lines.push((Line::raw(""), None));

    // The team.
    let teams = choice.teams();
    let picked = choice.team_option();
    let (mut label, mut detail) = shown(teams, picked, CHOOSE_TEAM);
    if picked.is_none() && teams.loaded().is_some() && !choice.team().is_empty() {
        (label, detail) = (choice.team().to_string(), None);
    }
    if choice.needs_key() && teams.loaded().is_none() {
        (label, detail) = ("Verify an API key first".into(), None);
    }
    let disabled = !editable || teams.loaded().is_none();
    d.lines.push((textbox::label(&format!("{TEAM_LABEL} *"), focus == Some(Part::Team), disabled, w), None));
    d.boxes.push((Part::Team, d.lines.len()));
    for line in select_lines(&label, detail.as_deref(), focus == Some(Part::Team), disabled, w) {
        d.lines.push((line, Some(Part::Team)));
    }
    if let Some(e) = teams.error().filter(|_| !choice.needs_key()) {
        for line in wrap(&format!("✗ Couldn't load the teams: {e} (r retries)"), w) {
            d.lines.push((Line::from(Span::styled(line, Style::default().fg(palette().ansi_red))), None));
        }
    }

    // The filters, once a team is picked.
    if choice.team_option().is_some() {
        for filter in Filter::ALL {
            let part = Part::Filter(filter);
            let list = choice.filter_options(filter);
            let picked = choice.filter_option(filter);
            let mut label = shown(&list, picked.as_ref(), ANY).0;
            if picked.is_none() && list.loaded().is_some() {
                label = choice.filter(filter).unwrap_or(ANY).to_string();
            }
            let disabled = !editable || list.loaded().is_none();
            d.lines.push((Line::raw(""), None));
            d.lines.push((textbox::label(filter.label(), focus == Some(part), disabled, w), None));
            d.boxes.push((part, d.lines.len()));
            for line in select_lines(&label, None, focus == Some(part), disabled, w) {
                d.lines.push((line, Some(part)));
            }
            if let Some(e) = list.error() {
                for line in wrap(&format!("✗ {e} (r retries)"), w) {
                    d.lines.push((Line::from(Span::styled(line, Style::default().fg(palette().ansi_red))), None));
                }
            }
        }
        for line in wrap(FILTERS_HELP, w) {
            d.lines.push((Line::from(muted(line)), None));
        }
    }
    if let Some(scope) = choice.scope_line() {
        d.lines.push((Line::raw(""), None));
        for line in wrap(&scope, w) {
            d.lines.push((Line::from(Span::styled(line, Style::default().fg(palette().foreground))), None));
        }
    }
    d
}

/// A picker's menu rows (`ENG  Engineering`) and the one picked.
pub(crate) fn menu(choice: &LinearChoice, part: Part) -> (Vec<String>, Option<usize>) {
    let (rows, picked) = match part {
        Part::Key | Part::ChangeKey => return (Vec::new(), None),
        Part::Team => (choice.teams().loaded().cloned().unwrap_or_default(), choice.team_option().cloned()),
        Part::Filter(f) => (choice.filter_options(f).loaded().cloned().unwrap_or_default(), choice.filter_option(f)),
    };
    let labels = rows
        .iter()
        .map(|r| match &r.detail {
            Some(d) => format!("{}  {d}", r.label),
            None => r.label.clone(),
        })
        .collect();
    let at = picked.and_then(|p| rows.iter().position(|r| *r == p));
    (labels, at)
}

/// The value menu row `index` picks.
pub(crate) fn menu_value(choice: &LinearChoice, part: Part, index: usize) -> Option<String> {
    let rows = match part {
        Part::Key | Part::ChangeKey => return None,
        Part::Team => choice.teams().loaded().cloned()?,
        Part::Filter(f) => choice.filter_options(f).loaded().cloned()?,
    };
    rows.get(index).map(|r| r.value.clone())
}
